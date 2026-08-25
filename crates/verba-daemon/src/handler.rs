//! IPC 请求处理器。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;
use verba_ai::{LlmClient, LlmConfig, LlmRequest};
use verba_config::{ApiKeyStore, Config, ConfigManager};
use verba_core::VERSION;
use verba_ipc::server::{Outbound, RequestHandler};
use verba_librime::{RimeConfig, RimeEngine};
use verba_protos::{
    request, response, stream_event, ApiKeySet, Audio as AudioMsg, Candidates, Chunk,
    Config as ConfigMsg, Error as ProtoError, Final, LlmCandidates, LlmGenerate, Ok as OkMsg, Pong,
    Response, StreamEvent,
};

/// 默认 AI 系统提示词（用户未配置时使用）。
const DEFAULT_AI_SYSTEM: &str =
    "你是一个输入法里的 AI 助手。回答应简洁、直接，以可上屏的文本输出，不要使用 Markdown。";

/// 候选融合系统提示词：只输出候选本身，便于按行解析。
const CANDIDATE_SYSTEM: &str = "你是输入法智能候选生成器。根据用户输入的拼音串生成中文候选。只输出候选本身，每行一个；不要编号、不要序号、不要标点、不要任何解释或前后缀。";

/// 候选融合单次请求上限。
const CANDIDATE_MAX: usize = 6;
/// OCR 历史保留条数。
const OCR_HISTORY_MAX: usize = 8;

pub struct DaemonHandler {
    mgr: ConfigManager,
    config: Arc<RwLock<Config>>,
    llm_config: Arc<RwLock<LlmConfig>>,
    llm: LlmClient,
    /// 取消注册表：键为 `(conn_id, req_id)`——请求 id 每连接从 1 自增，
    /// 全局键会跨连接互踩（架构审查 P1-1）。
    cancels: Arc<Mutex<HashMap<(u64, u64), CancellationToken>>>,
    /// 可选 Rime 引擎（config 引擎=rime 时惰性加载；串行化访问）。
    /// Arc 包裹：同步 FFI 查询走 spawn_blocking（架构审查 P2-3，避免阻塞 tokio worker）。
    rime: Arc<Mutex<Option<RimeEngine>>>,
    /// AI 多轮上下文（role, content），按 session_id 分组（多会话隔离，
    /// 架构审查会话维度）；config ai_context_turns>0 时使用。
    history: Mutex<HashMap<u64, VecDeque<(String, String)>>>,
    /// 最近若干条 OCR 结果（供 `//上次OCR` / `//OCR <序号>` 复用）。
    ocr_history: Mutex<VecDeque<String>>,
}

/// 取消解析：精确键 `(conn_id, id)` 优先（P1-1 同连接隔离语义），查不到时按
/// id 全局 fallback（#27：Windows 前端取消走独立控制连接，流注册在 worker
/// 连接——键控隔离使取消永远查不到）。本地 IPC 已按用户隔离（B1），同用户
/// 内全局匹配可接受。
fn resolve_cancel(
    cancels: &mut HashMap<(u64, u64), CancellationToken>,
    conn_id: u64,
    id: u64,
) -> Option<CancellationToken> {
    cancels.remove(&(conn_id, id)).or_else(|| {
        // 先取 key 再 remove（避免 iter 借用与可变借用冲突）
        let key = cancels.iter().find(|(k, _)| k.1 == id).map(|(k, _)| *k);
        key.and_then(|k| cancels.remove(&k))
    })
}

/// 会话历史存储：`session_id` → 该会话的 (role, content) 轮次队列。
/// 多会话（多输入上下文）按 session_id 隔离，互不串上下文（架构审查会话维度 B4b）。
type SessionHistory = HashMap<u64, VecDeque<(String, String)>>;

/// 读取某会话最近 `context_turns` 轮上下文（按时间序），供拼入 LLM 请求。
/// 会话不存在时返回空。`session_id == 0` 为旧客户端默认共享槽，按槽位 0 读取。
fn history_snapshot(
    store: &SessionHistory,
    session_id: u64,
    context_turns: usize,
) -> Vec<(String, String)> {
    let mut history = Vec::new();
    if let Some(sess_hist) = store.get(&session_id) {
        let start = sess_hist.len().saturating_sub(context_turns * 2);
        for (role, content) in sess_hist.iter().skip(start) {
            history.push((role.clone(), content.clone()));
        }
    }
    history
}

/// 追加一轮 (user, assistant) 到某会话，并按 `context_turns` 截断到上限。
fn history_append(
    store: &mut SessionHistory,
    session_id: u64,
    user: String,
    assistant: String,
    context_turns: usize,
) {
    let sess_hist = store.entry(session_id).or_default();
    sess_hist.push_back(("user".to_owned(), user));
    sess_hist.push_back(("assistant".to_owned(), assistant));
    let max = context_turns * 2;
    while sess_hist.len() > max {
        sess_hist.pop_front();
    }
}

/// 取消注册 RAII 守卫：函数任何退出路径（含 early-return）都移除注册，
/// 防止取消表无界泄漏（架构审查 P2-8）。
struct CancelGuard {
    cancels: Arc<Mutex<HashMap<(u64, u64), CancellationToken>>>,
    key: (u64, u64),
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.cancels.lock().unwrap().remove(&self.key);
    }
}

impl DaemonHandler {
    pub fn new(mgr: ConfigManager, config: Config, llm_config: LlmConfig, llm: LlmClient) -> Self {
        Self {
            mgr,
            config: Arc::new(RwLock::new(config)),
            llm_config: Arc::new(RwLock::new(llm_config)),
            llm,
            cancels: Arc::new(Mutex::new(HashMap::new())),
            rime: Arc::new(Mutex::new(None)),
            history: Mutex::new(HashMap::new()),
            ocr_history: Mutex::new(VecDeque::new()),
        }
    }

    fn config_map(&self) -> HashMap<String, String> {
        self.config.read().unwrap().to_map()
    }

    fn llm_snapshot(&self) -> (LlmConfig, String) {
        // 每次从当前 config 派生，保证 config set 热更新生效；api_key 保留启动时的密钥库值。
        let cfg = self.config.read().unwrap().clone();
        let api_key = self.llm_config.read().unwrap().api_key.clone();
        let mut llm = LlmConfig::new(cfg.llm_base_url.clone(), api_key, cfg.llm_model.clone());
        llm.temperature = cfg.temperature;
        llm.max_tokens = cfg.max_tokens;
        (llm, cfg.ai_system_prompt)
    }
}

#[async_trait::async_trait]
impl RequestHandler for DaemonHandler {
    async fn handle(&self, conn_id: u64, req: verba_protos::Request, out: Outbound) {
        let id = req.id;
        let result = match req.kind {
            Some(request::Kind::Ping(_)) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Pong(Pong {
                        version: VERSION.to_owned(),
                    })),
                })
                .await
            }
            Some(request::Kind::GetConfig(_)) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Config(ConfigMsg {
                        values: self.config_map(),
                    })),
                })
                .await
            }
            Some(request::Kind::SetConfig(sc)) => {
                // 所有锁操作在 await 之前完成，避免非 Send 守卫跨 await。
                let updated: Result<Config, verba_config::ConfigError> = {
                    let mut guard = self.config.write().unwrap();
                    let r = guard.apply_map(&sc.values);
                    drop(guard);
                    match r {
                        Ok(()) => Ok(self.config.read().unwrap().clone()),
                        Err(e) => Err(e),
                    }
                };
                match updated {
                    Ok(cfg) => {
                        if let Err(e) = self.mgr.save(&cfg) {
                            log::warn!("配置保存失败: {e}");
                        }
                        out.response(&Response {
                            id,
                            kind: Some(response::Kind::Ok(OkMsg {})),
                        })
                        .await
                    }
                    Err(e) => {
                        out.response(&Response {
                            id,
                            kind: Some(response::Kind::Error(ProtoError {
                                code: 400,
                                message: e.to_string(),
                            })),
                        })
                        .await
                    }
                }
            }
            Some(request::Kind::SetMode(sm)) => {
                log::info!("模式切换: {}", sm.mode);
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Ok(OkMsg {})),
                })
                .await
            }
            Some(request::Kind::LlmGenerate(g)) => {
                self.handle_llm_generate(conn_id, id, g, out).await
            }
            Some(request::Kind::LlmCandidates(g)) => {
                self.handle_llm_candidates(conn_id, id, g, out).await
            }
            Some(request::Kind::RimeCandidates(g)) => self.handle_rime_candidates(id, g, out).await,
            Some(request::Kind::TtsSynthesize(g)) => self.handle_tts_synthesize(id, g, out).await,
            Some(request::Kind::OcrRecognize(g)) => self.handle_ocr_recognize(id, g, out).await,
            Some(request::Kind::AsrTranscribe(g)) => self.handle_asr_transcribe(id, g, out).await,
            Some(request::Kind::ApiKeySet(g)) => self.handle_api_key_set(id, g, out).await,
            Some(request::Kind::LlmCancel(_)) => {
                // 优先精确键（P1-1 修复：只取消本连接注册的流，跨连接 id 碰撞互踩）。
                // 精确查不到时按 id 全局 fallback（#27）。注意：guard 须在 await 前
                // drop（MutexGuard 不 Send）。
                let token = {
                    let mut cancels = self.cancels.lock().unwrap();
                    resolve_cancel(&mut cancels, conn_id, id)
                };
                if let Some(token) = token {
                    token.cancel();
                }
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Ok(OkMsg {})),
                })
                .await
            }
            None => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Error(ProtoError {
                        code: 400,
                        message: "空请求".into(),
                    })),
                })
                .await
            }
        };
        if let Err(e) = result {
            log::warn!("响应写入失败: {e}");
        }
    }
}

impl DaemonHandler {
    async fn handle_llm_generate(
        &self,
        conn_id: u64,
        id: u64,
        g: verba_protos::LlmGenerate,
        out: Outbound,
    ) -> Result<(), verba_ipc::IpcError> {
        let key = (conn_id, id);
        let token = CancellationToken::new();
        self.cancels.lock().unwrap().insert(key, token.clone());
        // RAII 守卫：任何退出路径（early-return/错误）都移除注册，防取消表泄漏
        let _guard = CancelGuard {
            cancels: Arc::clone(&self.cancels),
            key,
        };

        out.response(&Response {
            id,
            kind: Some(response::Kind::Ok(OkMsg {})),
        })
        .await?;

        let (mut llm_cfg, config_system) = self.llm_snapshot();
        let LlmGenerate {
            prompt,
            system,
            temperature,
            max_tokens,
            stream: _,
            image,
            image_mime,
            session_id,
        } = g;
        let image = image.map(|data| {
            let mime = image_mime.clone().unwrap_or_else(|| "image/png".to_owned());
            (mime, data)
        });
        let user_prompt = prompt.clone();
        let context_turns = self.config.read().unwrap().ai_context_turns.max(0) as usize;
        let trimmed = user_prompt.trim();
        if trimmed == "重置" || trimmed == "reset" {
            // 只清本会话上下文（多会话隔离）；旧客户端（session_id=0）清无会话槽
            self.history.lock().unwrap().remove(&session_id);
            let _ = out
                .event(&StreamEvent {
                    id,
                    kind: Some(stream_event::Kind::Final(Final {
                        text: "已重置上下文".to_owned(),
                    })),
                })
                .await;
            return Ok(());
        }
        // `//会话`：查看当前会话的 AI 多轮上下文轮数。
        if trimmed == "会话" {
            let turns = self
                .history
                .lock()
                .unwrap()
                .get(&session_id)
                .map(|h| h.len() / 2)
                .unwrap_or(0);
            let _ = out
                .event(&StreamEvent {
                    id,
                    kind: Some(stream_event::Kind::Final(Final {
                        text: format!("AI 上下文: {turns} 轮（`//重置` 清空）"),
                    })),
                })
                .await;
            return Ok(());
        }
        // `//上次OCR`：复用最新一条 OCR 结果。
        if trimmed == "上次OCR" {
            let text = self
                .ocr_history
                .lock()
                .unwrap()
                .front()
                .cloned()
                .unwrap_or_else(|| "暂无 OCR 历史".to_owned());
            let _ = out
                .event(&StreamEvent {
                    id,
                    kind: Some(stream_event::Kind::Final(Final { text })),
                })
                .await;
            return Ok(());
        }
        // `//OCR历史`：列出最近的 OCR 结果。
        if trimmed == "OCR历史" {
            let hist: Vec<String> = self.ocr_history.lock().unwrap().iter().cloned().collect();
            let text = if hist.is_empty() {
                "暂无 OCR 历史".to_owned()
            } else {
                let mut lines = Vec::new();
                for (i, t) in hist.iter().enumerate() {
                    lines.push(format!("{}. {}", i + 1, t));
                }
                lines.join("\n")
            };
            let _ = out
                .event(&StreamEvent {
                    id,
                    kind: Some(stream_event::Kind::Final(Final { text })),
                })
                .await;
            return Ok(());
        }
        // `//OCR <序号>`：复用第 N 新的一条（从 1 起）。
        if let Some(num) = trimmed
            .strip_prefix("OCR ")
            .and_then(|s| s.trim().parse::<usize>().ok())
        {
            if num >= 1 {
                let text = self
                    .ocr_history
                    .lock()
                    .unwrap()
                    .get(num - 1)
                    .cloned()
                    .unwrap_or_else(|| format!("无第 {num} 条 OCR 历史"));
                let _ = out
                    .event(&StreamEvent {
                        id,
                        kind: Some(stream_event::Kind::Final(Final { text })),
                    })
                    .await;
                return Ok(());
            }
        }
        let has_image = image.is_some();
        let mut history = Vec::new();
        if !has_image && context_turns > 0 {
            let guard = self.history.lock().unwrap();
            history = history_snapshot(&guard, session_id, context_turns);
        }
        // vision：请求携带图像时，若配置了独立 vision 模型则切换模型名。
        if image.is_some() {
            let vision_model = self.config.read().unwrap().llm_vision_model.clone();
            if !vision_model.is_empty() {
                llm_cfg.model = vision_model;
            }
        }
        let system = system
            .filter(|s| !s.is_empty())
            .or_else(|| (!config_system.is_empty()).then_some(config_system))
            .or_else(|| Some(DEFAULT_AI_SYSTEM.to_owned()));

        let req = LlmRequest {
            prompt,
            system,
            temperature,
            max_tokens,
            image,
            history,
        };

        match self.llm.stream(&llm_cfg, &req).await {
            Ok(mut stream) => {
                let mut failed = false;
                let mut cancelled = false;
                let mut final_text = String::new();
                tokio::select! {
                    _ = token.cancelled() => {
                        cancelled = true;
                        log::info!("LLM 请求 {id} 已取消");
                    }
                    _ = async {
                        while let Some(chunk) = stream.next().await {
                            match chunk {
                                Ok(text) => {
                                    final_text.push_str(&text);
                                    if let Err(e) = out
                                        .event(&StreamEvent {
                                            id,
                                            kind: Some(stream_event::Kind::Chunk(Chunk {
                                                text,
                                            })),
                                        })
                                        .await
                                    {
                                        log::warn!("事件写入失败: {e}");
                                        failed = true;
                                        break;
                                    }
                                }
                                Err(e) => {
                                    log::warn!("LLM 流错误: {e}");
                                    let _ = out
                                        .event(&StreamEvent {
                                            id,
                                            kind: Some(stream_event::Kind::Error(ProtoError {
                                                code: 500,
                                                message: e.to_string(),
                                            })),
                                        })
                                        .await;
                                    failed = true;
                                    break;
                                }
                            }
                        }
                    } => {}
                }
                // 取消时也补发 Final，保证客户端流线程能退出阻塞读。
                // 但取消轮不写入 AI 上下文——截断文本进历史会把半截回答
                // 当完整一轮发给下一轮（架构审查 P1-6）。
                if !failed {
                    let _ = out
                        .event(&StreamEvent {
                            id,
                            kind: Some(stream_event::Kind::Final(Final {
                                text: final_text.clone(),
                            })),
                        })
                        .await;
                    // 记录这一轮（文本 AI），供下一轮上下文；图像请求不入历史。
                    // 按会话分组（多会话隔离，架构审查会话维度）。
                    if !cancelled && !has_image && context_turns > 0 {
                        let mut guard = self.history.lock().unwrap();
                        history_append(
                            &mut guard,
                            session_id,
                            user_prompt.clone(),
                            final_text.clone(),
                            context_turns,
                        );
                    }
                }
            }
            Err(e) => {
                let _ = out
                    .event(&StreamEvent {
                        id,
                        kind: Some(stream_event::Kind::Error(ProtoError {
                            code: 502,
                            message: e.to_string(),
                        })),
                    })
                    .await;
            }
        }

        Ok(())
    }

    /// 候选融合：请求 LLM 为拼音补充候选，按行流式解析并增量推送
    /// `Candidates` 事件（去重 + 去编号），结束（含取消）时补发 `done=true`。
    async fn handle_llm_candidates(
        &self,
        conn_id: u64,
        id: u64,
        g: verba_protos::LlmCandidates,
        out: Outbound,
    ) -> Result<(), verba_ipc::IpcError> {
        let key = (conn_id, id);
        let token = CancellationToken::new();
        self.cancels.lock().unwrap().insert(key, token.clone());
        // RAII 守卫：任何退出路径都移除注册，防取消表泄漏
        let _guard = CancelGuard {
            cancels: Arc::clone(&self.cancels),
            key,
        };

        out.response(&Response {
            id,
            kind: Some(response::Kind::Ok(OkMsg {})),
        })
        .await?;

        let (llm_cfg, _) = self.llm_snapshot();
        let max = (g.max_candidates as usize).clamp(1, CANDIDATE_MAX);
        let dict_line = if g.dictionary.is_empty() {
            "（无）".to_owned()
        } else {
            g.dictionary.join("、")
        };
        let prompt = format!(
            "拼音：{}\n已有词库候选：{}\n请补充生成最多 {max} 个与拼音匹配、更符合语境的候选。",
            g.pinyin, dict_line
        );
        let req = LlmRequest {
            prompt,
            system: Some(CANDIDATE_SYSTEM.to_owned()),
            temperature: Some(0.3),
            max_tokens: Some(128),
            image: None,
            history: Vec::new(),
        };

        match self.llm.stream(&llm_cfg, &req).await {
            Ok(mut stream) => {
                let mut buf = String::new();
                let mut emitted: Vec<String> = Vec::new();
                let mut failed = false;
                tokio::select! {
                    _ = token.cancelled() => {
                        log::info!("候选请求 {id} 已取消");
                    }
                    _ = async {
                        while let Some(chunk) = stream.next().await {
                            match chunk {
                                Ok(text) => {
                                    buf.push_str(&text);
                                    while let Some(pos) = buf.find('\n') {
                                        let line: String = buf.drain(..=pos).collect();
                                        if let Err(e) = self
                                            .emit_candidate(&out, id, &g, &mut emitted, max, &line)
                                            .await
                                        {
                                            log::warn!("候选事件写入失败: {e}");
                                            failed = true;
                                            break;
                                        }
                                    }
                                }
                                Err(e) => {
                                    log::warn!("LLM 流错误: {e}");
                                    let _ = out
                                        .event(&StreamEvent {
                                            id,
                                            kind: Some(stream_event::Kind::Error(ProtoError {
                                                code: 500,
                                                message: e.to_string(),
                                            })),
                                        })
                                        .await;
                                    failed = true;
                                    break;
                                }
                            }
                        }
                        // 冲刷末尾无换行行
                        if !failed && !buf.trim().is_empty() {
                            let line = std::mem::take(&mut buf);
                            if let Err(e) = self.emit_candidate(&out, id, &g, &mut emitted, max, &line).await
                            {
                                log::warn!("候选事件写入失败: {e}");
                            }
                        }
                    } => {}
                }
                // 结束事件：即使取消也补发，保证客户端流线程能退出阻塞读。
                if !failed {
                    let _ = out
                        .event(&StreamEvent {
                            id,
                            kind: Some(stream_event::Kind::Candidates(Candidates {
                                pinyin: g.pinyin.clone(),
                                candidates: vec![],
                                done: true,
                            })),
                        })
                        .await;
                }
            }
            Err(e) => {
                let _ = out
                    .event(&StreamEvent {
                        id,
                        kind: Some(stream_event::Kind::Error(ProtoError {
                            code: 502,
                            message: e.to_string(),
                        })),
                    })
                    .await;
            }
        }

        Ok(())
    }

    /// Rime 引擎候选（config 引擎=rime）：一次性推送 `Candidates` 事件（done=true）。
    async fn handle_rime_candidates(
        &self,
        id: u64,
        g: verba_protos::RimeCandidates,
        out: Outbound,
    ) -> Result<(), verba_ipc::IpcError> {
        let max = (g.max_candidates as usize).clamp(1, 27);
        let schema = if g.schema.is_empty() {
            "luna_pinyin_simp".to_owned()
        } else {
            g.schema.clone()
        };
        // 同步 FFI 查询走 spawn_blocking（架构审查 P2-3）：首次部署 2-5s，
        // 直接跑在 async 处理器上会阻塞该 tokio worker 上的一切请求。
        let rime = Arc::clone(&self.rime);
        let input = g.input.clone();
        let task = tokio::task::spawn_blocking(move || {
            Self::rime_query_sync(&rime, |e| e.candidates(&input, &schema, max))
        });
        let cands = match tokio::time::timeout(std::time::Duration::from_secs(10), task).await {
            Ok(Ok(Ok(cands))) => cands,
            Ok(Ok(Err(e))) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Error(ProtoError {
                        code: 502,
                        message: e,
                    })),
                })
                .await?;
                return Ok(());
            }
            Ok(Err(_)) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Error(ProtoError {
                        code: 500,
                        message: "Rime 查询任务异常".into(),
                    })),
                })
                .await?;
                return Ok(());
            }
            Err(_) => {
                // 超时：调用方不再等待（阻塞线程继续运行，由下次查询的互斥体接管）
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Error(ProtoError {
                        code: 504,
                        message: "Rime 查询超时".into(),
                    })),
                })
                .await?;
                return Ok(());
            }
        };
        out.response(&Response {
            id,
            kind: Some(response::Kind::Ok(OkMsg {})),
        })
        .await?;
        out.event(&StreamEvent {
            id,
            kind: Some(stream_event::Kind::Candidates(Candidates {
                pinyin: g.input,
                candidates: cands.into_iter().map(|c| c.text).collect(),
                done: true,
            })),
        })
        .await
    }

    /// TTS 合成：按 config tts_provider/tts_voice 分发，返回音频字节。
    async fn handle_tts_synthesize(
        &self,
        id: u64,
        g: verba_protos::TtsSynthesize,
        out: Outbound,
    ) -> Result<(), verba_ipc::IpcError> {
        // 锁在 await 前释放，避免非 Send 守卫跨 await。
        let (provider, voice, model, base_url) = {
            let cfg = self.config.read().unwrap();
            let voice = g
                .voice
                .clone()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| cfg.tts_voice.clone());
            let base_url = if cfg.tts_base_url.is_empty() {
                cfg.llm_base_url.clone()
            } else {
                cfg.tts_base_url.clone()
            };
            (
                cfg.tts_provider.clone(),
                voice,
                cfg.tts_model.clone(),
                base_url,
            )
        };
        let api_key = self.llm_config.read().unwrap().api_key.clone();
        let client = match verba_tts::TtsClient::from_config(
            &provider,
            &voice,
            &base_url,
            api_key.as_deref(),
            &model,
        ) {
            Ok(c) => c,
            Err(e) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Error(ProtoError {
                        code: 400,
                        message: e.to_string(),
                    })),
                })
                .await?;
                return Ok(());
            }
        };
        match client.synthesize(&g.text).await {
            Ok(audio) => {
                log::info!(
                    "TTS 合成: text_len={} provider={} format={} bytes={}",
                    g.text.chars().count(),
                    provider,
                    audio.format,
                    audio.bytes.len()
                );
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Audio(AudioMsg {
                        format: audio.format.to_owned(),
                        data: audio.bytes,
                    })),
                })
                .await
            }
            Err(e) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Error(ProtoError {
                        code: 500,
                        message: e.to_string(),
                    })),
                })
                .await
            }
        }
    }

    /// OCR 识别：按 config ocr_provider 分发，返回识别文字。
    async fn handle_ocr_recognize(
        &self,
        id: u64,
        g: verba_protos::OcrRecognize,
        out: Outbound,
    ) -> Result<(), verba_ipc::IpcError> {
        let provider = self.config.read().unwrap().ocr_provider.clone();
        let client = match verba_ocr::OcrClient::from_config(&provider) {
            Ok(c) => c,
            Err(e) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Error(ProtoError {
                        code: 400,
                        message: e.to_string(),
                    })),
                })
                .await?;
                return Ok(());
            }
        };
        match client.recognize(&g.image).await {
            Ok(text) => {
                log::info!(
                    "OCR 识别: image_bytes={} provider={} text_len={}",
                    g.image.len(),
                    provider,
                    text.chars().count()
                );
                {
                    let mut hist = self.ocr_history.lock().unwrap();
                    hist.push_front(text.clone());
                    while hist.len() > OCR_HISTORY_MAX {
                        hist.pop_back();
                    }
                }
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Text(verba_protos::Text { text })),
                })
                .await
            }
            Err(e) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Error(ProtoError {
                        code: 500,
                        message: e.to_string(),
                    })),
                })
                .await
            }
        }
    }

    /// ASR 转写：按 config asr_provider 分发，返回识别文字。
    async fn handle_asr_transcribe(
        &self,
        id: u64,
        g: verba_protos::AsrTranscribe,
        out: Outbound,
    ) -> Result<(), verba_ipc::IpcError> {
        let (provider, model, base_url) = {
            let cfg = self.config.read().unwrap();
            let base_url = if cfg.asr_base_url.is_empty() {
                cfg.llm_base_url.clone()
            } else {
                cfg.asr_base_url.clone()
            };
            (cfg.asr_provider.clone(), cfg.asr_model.clone(), base_url)
        };
        let api_key = self.llm_config.read().unwrap().api_key.clone();
        let client = match verba_asr::AsrClient::from_config(
            &provider,
            &base_url,
            api_key.as_deref(),
            &model,
        ) {
            Ok(c) => c,
            Err(e) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Error(ProtoError {
                        code: 400,
                        message: e.to_string(),
                    })),
                })
                .await?;
                return Ok(());
            }
        };
        match client.transcribe(&g.audio).await {
            Ok(text) => {
                log::info!(
                    "ASR 转写: audio_bytes={} provider={} text_len={}",
                    g.audio.len(),
                    provider,
                    text.chars().count()
                );
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Text(verba_protos::Text { text })),
                })
                .await
            }
            Err(e) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Error(ProtoError {
                        code: 500,
                        message: e.to_string(),
                    })),
                })
                .await
            }
        }
    }

    /// 设置/清除 API Key：写系统密钥库并热更新 daemon 内存（空字符串 = 删除）。
    async fn handle_api_key_set(
        &self,
        id: u64,
        g: ApiKeySet,
        out: Outbound,
    ) -> Result<(), verba_ipc::IpcError> {
        let result: Result<(), verba_config::ConfigError> = if g.key.is_empty() {
            ApiKeyStore::clear()
        } else {
            ApiKeyStore::set(&g.key)
        };
        match result {
            Ok(()) => {
                {
                    let mut guard = self.llm_config.write().unwrap();
                    guard.api_key = if g.key.is_empty() { None } else { Some(g.key) };
                }
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Ok(OkMsg {})),
                })
                .await
            }
            Err(e) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Error(ProtoError {
                        code: 500,
                        message: e.to_string(),
                    })),
                })
                .await
            }
        }
    }

    /// 后台预热 Rime 引擎（单引擎，daemon 启动时调用）：提前触发首次部署，
    /// 使首次候选查询免于等待部署（2-5 秒），避免用户误以为无反应。
    pub fn warmup_rime(&self) {
        // 预热同样走 spawn_blocking：首次部署 2-5s，不能占用 tokio worker
        let rime = Arc::clone(&self.rime);
        tokio::task::spawn_blocking(move || match Self::rime_query_sync(&rime, |_| Ok(())) {
            Ok(()) => log::info!("Rime 引擎预热完成"),
            Err(e) => log::warn!("Rime 引擎预热失败（首次查询时会重试）: {e}"),
        });
    }

    /// 惰性加载并串行执行 Rime 查询（无 await，锁不跨异步点）。
    /// Rime 引擎查询（同步 FFI + 互斥体，须在 spawn_blocking 内调用）。
    fn rime_query_sync<T>(
        rime: &Mutex<Option<RimeEngine>>,
        f: impl FnOnce(&RimeEngine) -> Result<T, verba_librime::RimeError>,
    ) -> Result<T, String> {
        let mut guard = rime.lock().unwrap();
        if guard.is_none() {
            let (dll, shared, user) = rime_paths();
            log::info!(
                "Rime 引擎加载: dll={} shared={} user={}",
                dll.display(),
                shared.display(),
                user.display()
            );
            let cfg = RimeConfig::load(&dll, &shared, &user);
            *guard = Some(RimeEngine::new(&cfg).map_err(|e| e.to_string())?);
        }
        f(guard.as_ref().expect("已加载")).map_err(|e| e.to_string())
    }

    /// 清洗一行 LLM 输出为候选并增量推送（去空行/编号前缀/与词库及已发候选去重）。
    async fn emit_candidate(
        &self,
        out: &Outbound,
        id: u64,
        g: &LlmCandidates,
        emitted: &mut Vec<String>,
        max: usize,
        raw: &str,
    ) -> Result<(), verba_ipc::IpcError> {
        let mut line = raw.trim();
        // 去常见编号前缀：`1. 你好` / `1、你好` / `1）你好`
        for pat in [".", "、", ")", "）", ":"] {
            if let Some(rest) = line
                .strip_prefix(|c: char| c.is_ascii_digit())
                .and_then(|_| line.split_once(pat))
            {
                line = rest.1.trim();
                break;
            }
        }
        if line.is_empty() || line.len() < 2 || line.is_ascii() {
            // 跳过空行 / 单字符 / 纯英文（拼音原样不算候选）
            return Ok(());
        }
        if emitted.contains(&line.to_owned()) || g.dictionary.iter().any(|d| d == line) {
            return Ok(());
        }
        if emitted.len() >= max {
            return Ok(());
        }
        let cand = line.to_owned();
        emitted.push(cand.clone());
        out.event(&StreamEvent {
            id,
            kind: Some(stream_event::Kind::Candidates(Candidates {
                pinyin: g.pinyin.clone(),
                candidates: vec![cand],
                done: false,
            })),
        })
        .await
    }
}

/// Rime 资源定位：环境变量优先，缺省取 daemon 同目录 `rime/` 下
/// `librime` 库、`data/`、`user_data/`（按平台：Windows `rime.dll` / macOS `librime.dylib`）。
fn rime_paths() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let from_env = || -> Option<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
        let d = std::env::var("VERBA_RIME_DLL")
            .or_else(|_| std::env::var("VERBA_RIME_DYLIB"))
            .ok()?;
        let s = std::env::var("VERBA_RIME_SHARED").ok()?;
        let u = std::env::var("VERBA_RIME_USER").ok()?;
        Some((d.into(), s.into(), u.into()))
    };
    if let Some(paths) = from_env() {
        return paths;
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_owned()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let rime_dir = exe_dir.join("rime");
    let lib_name = if cfg!(windows) {
        "rime.dll"
    } else if cfg!(target_os = "macos") {
        "librime.dylib"
    } else {
        "librime.so"
    };
    (
        rime_dir.join(lib_name),
        rime_dir.join("data"),
        rime_dir.join("user_data"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_exact_conn_priority() {
        // 同 id 两个并发注册（不同连接）：精确路径只取消本连接，不误杀
        let mut cancels = HashMap::new();
        let a = CancellationToken::new();
        let b = CancellationToken::new();
        cancels.insert((1, 2), a.clone());
        cancels.insert((2, 2), b.clone());
        let got = resolve_cancel(&mut cancels, 1, 2).expect("精确命中");
        got.cancel();
        assert!(a.is_cancelled());
        assert!(!b.is_cancelled());
        assert_eq!(cancels.len(), 1);
    }

    #[test]
    fn cancel_cross_conn_fallback() {
        // #27：连接 3 无注册（Windows 控制连接场景），fallback 按 id 命中 (7,2)
        let mut cancels = HashMap::new();
        let a = CancellationToken::new();
        cancels.insert((7, 2), a.clone());
        let got = resolve_cancel(&mut cancels, 3, 2).expect("fallback 命中");
        got.cancel();
        assert!(a.is_cancelled());
        assert!(cancels.is_empty());
    }

    #[test]
    fn cancel_miss_returns_none() {
        let mut cancels = HashMap::new();
        cancels.insert((7, 2), CancellationToken::new());
        assert!(resolve_cancel(&mut cancels, 3, 9).is_none());
        assert_eq!(cancels.len(), 1);
    }

    #[test]
    fn session_history_isolated_per_id() {
        // B4b：两个会话各自累积上下文，互不可见
        let mut store = SessionHistory::new();
        history_append(&mut store, 1, "u1".into(), "a1".into(), 4);
        history_append(&mut store, 2, "u2".into(), "a2".into(), 4);
        let s1 = history_snapshot(&store, 1, 4);
        let s2 = history_snapshot(&store, 2, 4);
        assert_eq!(
            s1,
            vec![
                ("user".to_owned(), "u1".to_owned()),
                ("assistant".to_owned(), "a1".to_owned())
            ]
        );
        assert_eq!(
            s2,
            vec![
                ("user".to_owned(), "u2".to_owned()),
                ("assistant".to_owned(), "a2".to_owned())
            ]
        );
        // 未注册会话为空
        assert!(history_snapshot(&store, 9, 4).is_empty());
    }

    #[test]
    fn session_history_trims_to_turn_limit() {
        // 截断按会话独立生效：会话 1 超限弹出最旧轮，会话 2 不受影响
        let mut store = SessionHistory::new();
        for i in 0..5 {
            history_append(&mut store, 1, format!("u{i}"), format!("a{i}"), 2);
            history_append(&mut store, 2, format!("x{i}"), format!("y{i}"), 2);
        }
        let s1 = history_snapshot(&store, 1, 2);
        // 只保留最近 2 轮（4 条）：u3/a3, u4/a4
        assert_eq!(s1.len(), 4);
        assert_eq!(s1[0].1, "u3");
        assert_eq!(s1[3].1, "a4");
        let s2 = history_snapshot(&store, 2, 2);
        assert_eq!(s2[0].1, "x3");
        assert_eq!(s2[3].1, "y4");
    }
}
