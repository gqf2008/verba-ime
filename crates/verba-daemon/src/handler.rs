//! IPC 请求处理器。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
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
    Config as ConfigMsg, Error as ProtoError, Final, LlmCandidates, LlmGenerate, ModelList,
    Ok as OkMsg, Pong, Response, StreamEvent,
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
    /// 架构审查会话维度）；config ai_context_turns>0 时使用。LRU 有界（MAX_AI_SESSIONS）。
    history: Mutex<SessionHistory>,
    /// 最近若干条 OCR 结果（供 `//上次OCR` / `//OCR <序号>` 复用）。
    ocr_history: Mutex<VecDeque<String>>,
}

/// 取消解析：精确键 `(conn_id, id)` 优先（P1-1 同连接隔离语义），查不到时按
/// id 全局 fallback（#27：Windows 前端取消走独立控制连接，流注册在 worker
/// 连接——键控隔离使取消永远查不到）。本地 IPC 已按用户隔离（B1）。
/// fallback 仅在 id **唯一匹配**时启用：请求 id 每连接从 1 自增（前端 ping
/// 占用 id=1，首条流恒为 2），并发流同 id 是常态——歧义时任意挑选会误杀
/// 其它前端的在途流（复审 V5），此时放弃取消并告警。
///
/// 注意（诚实的语义）：歧义时取消是**尽力而为**的——服务端流会继续生成直到
/// 完成（燃烧 token），但前端靠 stream epoch 过滤丢弃迟到事件，视觉/状态仍
/// 正确。这是「宁可漏取消、不可误杀他流」的取舍；调用方目前**不**重试，
/// 故歧义即静默不取消（仅一条 warn 日志）。消除歧义需请求 id 全局唯一化
/// （前端改用全局序号而非每连接自增），属后续重构。
fn resolve_cancel(
    cancels: &mut HashMap<(u64, u64), CancellationToken>,
    conn_id: u64,
    id: u64,
) -> Option<CancellationToken> {
    cancels.remove(&(conn_id, id)).or_else(|| {
        let mut matches = cancels.keys().filter(|k| k.1 == id);
        let key = match (matches.next(), matches.next()) {
            (Some(&k), None) => k,
            (Some(_), Some(_)) => {
                log::warn!("取消请求 id={id} 跨连接 fallback 匹配歧义（多个同 id 在途流），放弃");
                return None;
            }
            _ => return None,
        };
        cancels.remove(&key)
    })
}

/// 单会话历史：(role, content) 轮次队列 + 最近使用序号（LRU 逐出依据）。
struct SessionEntry {
    turns: VecDeque<(String, String)>,
    /// 单调递增的使用序号：每次 append 更新；超出会话上限时逐出最小值（最久未用）。
    last_used: u64,
}

/// 会话历史存储：`session_id` → 该会话历史。多会话（多输入上下文）按 session_id
/// 隔离，互不串上下文（架构审查会话维度 B4b）。
type SessionHistory = HashMap<u64, SessionEntry>;

/// AI 会话数上限：超出时按 LRU 逐出最久未用会话。前端每输入上下文/文本域烧一个
/// 新 session_id（macOS 每 IMK 控制器一个），会话表若无界会随 uptime 累积孤儿
/// 条目（复审 MEDIUM）。256 为经验值：远超并发活跃会话数，单会话占用又极小。
const MAX_AI_SESSIONS: usize = 256;

/// 历史使用序号源：每次 append 取一个递增值作为该会话的 LRU 序号。
static HISTORY_TICK: AtomicU64 = AtomicU64::new(1);

/// 读取某会话最近 `context_turns` 轮上下文（按时间序），供拼入 LLM 请求。
/// 会话不存在时返回空。`session_id == 0` 为旧客户端默认共享槽，按槽位 0 读取。
fn history_snapshot(
    store: &SessionHistory,
    session_id: u64,
    context_turns: usize,
) -> Vec<(String, String)> {
    let mut history = Vec::new();
    if let Some(entry) = store.get(&session_id) {
        let start = entry.turns.len().saturating_sub(context_turns * 2);
        for (role, content) in entry.turns.iter().skip(start) {
            history.push((role.clone(), content.clone()));
        }
    }
    history
}

/// 追加一轮 (user, assistant) 到某会话，按 `context_turns` 截断到上限，并刷新
/// 其 LRU 序号（`tick` 为单调递增源）。插入后若会话总数超限，逐出最久未用会话。
fn history_append(
    store: &mut SessionHistory,
    session_id: u64,
    user: String,
    assistant: String,
    context_turns: usize,
    tick: u64,
) {
    let entry = store.entry(session_id).or_insert_with(|| SessionEntry {
        turns: VecDeque::new(),
        last_used: 0,
    });
    entry.turns.push_back(("user".to_owned(), user));
    entry.turns.push_back(("assistant".to_owned(), assistant));
    let max = context_turns * 2;
    while entry.turns.len() > max {
        entry.turns.pop_front();
    }
    entry.last_used = tick;

    // LRU 逐出：会话数超限时移除最久未用者（不含刚插入的本会话——其 tick 最大）。
    if store.len() > MAX_AI_SESSIONS {
        if let Some(&oldest) = store
            .iter()
            .min_by_key(|(_, e)| e.last_used)
            .map(|(id, _)| id)
        {
            store.remove(&oldest);
        }
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
        // 不在 Drop 里 unwrap：锁中毒（或 panic  unwind 中再 panic）会把
        // 单个请求的失败升级为整个 daemon 进程 abort（复审 V10）。
        if let Ok(mut cancels) = self.cancels.lock() {
            cancels.remove(&self.key);
        }
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
            Some(request::Kind::ListModels(_)) => self.handle_list_models(id, out).await,
            Some(request::Kind::RimeInstallExtra(_)) => {
                self.handle_rime_install_extra(id, out).await
            }
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
                .map(|h| h.turns.len() / 2)
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
                            HISTORY_TICK.fetch_add(1, Ordering::Relaxed),
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
    async fn handle_list_models(
        &self,
        id: u64,
        out: Outbound,
    ) -> Result<(), verba_ipc::IpcError> {
        let (cfg, _) = self.llm_snapshot();
        if cfg.api_key.is_none() || cfg.api_key.as_deref().unwrap_or_default().is_empty() {
            out.response(&Response {
                id,
                kind: Some(response::Kind::Error(ProtoError {
                    code: 400,
                    message: "未配置 API Key（设置面板输入）".into(),
                })),
            })
            .await?;
            return Ok(());
        }
        let client = verba_ai::LlmClient::new().map_err(|e| verba_ipc::IpcError::Server {
            code: 500,
            message: e.to_string(),
        })?;
        match client.list_models(&cfg).await {
            Ok(models) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::ModelList(ModelList { models })),
                })
                .await?;
            }
            Err(e) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Error(ProtoError {
                        code: 502,
                        message: e.to_string(),
                    })),
                })
                .await?;
            }
        }
        Ok(())
    }

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
        // 30s 而非 10s：超时窗口包含互斥体排队——启动预热/首次部署持锁期间
        // （文档值 2-5s，冷 CI 更久）所有查询串行等待，10s 会对健康引擎误报
        // 504 并级联（复审 V9）。
        let cands = match tokio::time::timeout(std::time::Duration::from_secs(30), task).await {
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

    /// 安装生僻字扩展（issue #48）：把内嵌的 Verba 补充词条合入用户 Rime 目录
    /// 的 `custom_phrase.txt`，然后重置缓存引擎——下一次查询的惰性 `new()` 会
    /// 重新走 maintenance 部署（检测词条变化并重编译词典），无需新增部署 API。
    /// 合并语义与 `scripts/fetch-rime-vendor.sh` 的 vendor 注入同构（复用其管线）。
    async fn handle_rime_install_extra(
        &self,
        id: u64,
        out: Outbound,
    ) -> Result<(), verba_ipc::IpcError> {
        let (_, _, user_dir) = rime_paths();
        // 文件写入与引擎锁无关，走 spawn_blocking 不阻塞 worker；落盘成功后才
        // 拿锁重置（持锁窗口极小，锁内无 await）。
        let task = tokio::task::spawn_blocking(move || -> Result<(usize, bool), String> {
            std::fs::create_dir_all(&user_dir).map_err(|e| format!("用户 Rime 目录不可用: {e}"))?;
            merge_extra_phrases(&user_dir.join("custom_phrase.txt"))
                .map_err(|e| format!("词条文件读写失败: {e}"))
        });
        let (appended, created) =
            match tokio::time::timeout(std::time::Duration::from_secs(10), task).await {
                Ok(Ok(Ok(r))) => r,
                Ok(Ok(Err(e))) => {
                    out.response(&Response {
                        id,
                        kind: Some(response::Kind::Error(ProtoError {
                            code: 500,
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
                            message: "生僻字安装任务异常".into(),
                        })),
                    })
                    .await?;
                    return Ok(());
                }
                Err(_) => {
                    // 超时：后台任务继续运行，词条可能已落盘但引擎未重置
                    // （同 handle_rime_candidates 的 504 约定）；合并幂等，
                    // 用户重试点击即可补齐重置，不会写坏文件。
                    out.response(&Response {
                        id,
                        kind: Some(response::Kind::Error(ProtoError {
                            code: 504,
                            message: "生僻字安装超时".into(),
                        })),
                    })
                    .await?;
                    return Ok(());
                }
            };
        log::info!(
            "生僻字扩展已安装: 追加 {appended} 行{}",
            if created {
                "（新建词条文件）"
            } else {
                ""
            }
        );
        // 重置缓存引擎：下一次查询重新部署生效。中毒自愈同 rime_query_sync。
        {
            let mut guard = self.rime.lock().unwrap_or_else(|p| p.into_inner());
            *guard = None;
        }
        out.response(&Response {
            id,
            kind: Some(response::Kind::Ok(OkMsg {})),
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
        // 锁中毒自愈：FFI 路径一旦 panic 会毒化互斥体，unwrap 会让之后所有
        // 查询永远 500 直到重启 daemon（复审 V10）；into_inner 夺回引擎继续用。
        let mut guard = rime.lock().unwrap_or_else(|p| p.into_inner());
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

/// Rime 资源定位：环境变量优先，缺省取 daemon 同目录 `rime/` 下的
/// `librime` 库与 `data/`（按平台：Windows `rime.dll` / macOS `librime.dylib`）。
/// `user_data` 默认落**用户数据目录**（可写）：安装态下 exe 同目录在
/// `C:\Program Files\Verba` / `Verba.app` 包内——标准用户不可写（首次部署
/// create_dir_all 失败 → 永远 502），macOS 管理员可写又会改动已签名 bundle
/// 破坏 seal（复审 V1）。`VERBA_RIME_*` 环境变量仍可整体覆盖三要素。
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
    // 用户数据目录优先；定位失败（如 HOME 缺失）才退回 exe 同目录旧行为。
    let user_dir = verba_config::VerbaDirs::locate()
        .map(|d| {
            let dir = d.data_dir().join("rime");
            // 升级迁移（v0.2.0 起 user_data 从 exe 同目录迁到用户数据目录）：
            // 0.1.x 在 exe 旁积累的词库/自造词在首启时一次性复制过来，防止
            // 静默丢失（复审发现）；仅当新目录不存在时执行，失败不阻塞
            // （首次部署会以空目录继续）。
            migrate_legacy_user_data(&exe_dir, &dir);
            dir
        })
        .unwrap_or_else(|_| rime_dir.join("user_data"));
    (rime_dir.join(lib_name), rime_dir.join("data"), user_dir)
}

/// 升级迁移：把旧位置的 `rime/user_data`（exe 同目录，0.1.x 布局）复制到
/// 新的用户数据目录位置。新目录已存在（已迁移/已初始化）则跳过；复制失败
/// 仅告警——Rime 会以空 user_data 重新部署，build 可重建，不阻塞启动。
fn migrate_legacy_user_data(exe_dir: &std::path::Path, new_user_dir: &std::path::Path) {
    let old = exe_dir.join("rime").join("user_data");
    if !old.is_dir() || new_user_dir.exists() {
        return;
    }
    // copy_dir_all 自带 create_dir_all(to)，无需外层再建目录。
    match copy_dir_all(&old, new_user_dir) {
        Ok(()) => log::info!(
            "Rime user_data 已从旧位置迁移: {} -> {}",
            old.display(),
            new_user_dir.display()
        ),
        Err(e) => log::warn!(
            "Rime user_data 迁移失败（继续使用空目录，构建可重建）: {}: {e}",
            old.display()
        ),
    }
}

fn copy_dir_all(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Verba 补充词条（scripts/rime-extra/custom_phrase.txt，编译期内嵌，随 daemon 分发）。
const RIME_EXTRA_PHRASES: &str = include_str!("../../../scripts/rime-extra/custom_phrase.txt");

/// 把内嵌的 Verba 补充词条合入目标 `custom_phrase.txt`。
///
/// 语义与 `scripts/fetch-rime-vendor.sh` 的 vendor 合并同构（issue #48「复用
/// 其管线」）：目标文件不存在则整体写入内嵌文件（含 yaml 头）；存在则只补
/// 缺失的**词条行**——含制表符才计入（天然跳过注释/空行/yaml 头，不产生重复
/// 文档头）、逐行精确匹配（容 \r\n 文件）、幂等且不覆盖用户自建词条。
/// 返回（追加行数, 是否新建文件）。
fn merge_extra_phrases(path: &std::path::Path) -> std::io::Result<(usize, bool)> {
    if !path.exists() {
        std::fs::write(path, RIME_EXTRA_PHRASES)?;
        let entries = RIME_EXTRA_PHRASES
            .lines()
            .filter(|l| l.contains('\t'))
            .count();
        return Ok((entries, true));
    }
    let mut body = std::fs::read_to_string(path)?;
    // 脚本先补尾换行防末行粘连（[ -n "$(tail -c 1)" ] && printf '\n'），此处同。
    if !body.ends_with('\n') {
        body.push('\n');
    }
    let mut appended = 0;
    for line in RIME_EXTRA_PHRASES.lines() {
        if line.is_empty() || line.starts_with('#') || !line.contains('\t') {
            continue;
        }
        if body.lines().any(|l| l.trim_end_matches('\r') == line) {
            continue;
        }
        body.push_str(line);
        body.push('\n');
        appended += 1;
    }
    if appended > 0 {
        std::fs::write(path, body)?;
    }
    Ok((appended, false))
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
    fn cancel_fallback_ambiguous_id_aborts() {
        // 复审 V5：两个不同连接注册相同 req id（请求 id 每连接从 1 自增，
        // 并发流同 id 是常态），fallback 不得任意挑选受害者——歧义时放弃，
        // 两条流均不受误伤。
        let mut cancels = HashMap::new();
        let a = CancellationToken::new();
        let b = CancellationToken::new();
        cancels.insert((1, 2), a.clone());
        cancels.insert((2, 2), b.clone());
        assert!(resolve_cancel(&mut cancels, 3, 2).is_none());
        assert!(!a.is_cancelled());
        assert!(!b.is_cancelled());
        assert_eq!(cancels.len(), 2);
    }

    #[test]
    fn session_history_isolated_per_id() {
        // B4b：两个会话各自累积上下文，互不可见
        let mut store = SessionHistory::new();
        history_append(&mut store, 1, "u1".into(), "a1".into(), 4, 1);
        history_append(&mut store, 2, "u2".into(), "a2".into(), 4, 2);
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
            history_append(&mut store, 1, format!("u{i}"), format!("a{i}"), 2, i);
            history_append(&mut store, 2, format!("x{i}"), format!("y{i}"), 2, i);
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

    #[test]
    fn session_history_lru_eviction_bounds_map() {
        // 复审 MEDIUM：会话数超 MAX_AI_SESSIONS 时逐出最久未用（tick 最小）会话，
        // 表大小有界，防孤儿会话随 uptime 无界累积。
        let mut store = SessionHistory::new();
        // 填满上限：session 1..=MAX_AI_SESSIONS，tick 递增（1 最旧）。
        for id in 1..=(MAX_AI_SESSIONS as u64) {
            history_append(&mut store, id, format!("u{id}"), format!("a{id}"), 2, id);
        }
        assert_eq!(store.len(), MAX_AI_SESSIONS);
        // 再插入一个新会话（tick 最大）：应逐出最旧的 session 1，表大小仍为上界。
        history_append(&mut store, 9999, "new".into(), "new".into(), 2, 10_000);
        assert_eq!(store.len(), MAX_AI_SESSIONS);
        assert!(
            history_snapshot(&store, 1, 2).is_empty(),
            "最久未用会话被逐出"
        );
        assert!(!history_snapshot(&store, 9999, 2).is_empty(), "新会话保留");
        // 次旧的 session 2 仍在（只逐出一个）。
        assert!(!history_snapshot(&store, 2, 2).is_empty());
    }

    /// 构造一个唯一的临时目录（测试结束由调用方自行清理）。
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "verba-mig-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn migrate_legacy_user_data_copies_tree() {
        // 复审发现的缺口：迁移函数此前无任何测试。锁定「目录树整体搬迁 +
        // 嵌套子目录 + 新目录已存在时跳过」三个行为。
        let base = temp_dir("copy");
        let exe_dir = base.join("app");
        let old = exe_dir.join("rime").join("user_data");
        std::fs::create_dir_all(old.join("opencc")).unwrap();
        std::fs::write(old.join("default.custom.yaml"), "patch:\n").unwrap();
        std::fs::write(old.join("opencc").join("t2s.txt"), "中→忠\n").unwrap();
        let new_user = base.join("userdata");

        migrate_legacy_user_data(&exe_dir, &new_user);
        assert_eq!(
            std::fs::read_to_string(new_user.join("default.custom.yaml")).unwrap(),
            "patch:\n"
        );
        assert_eq!(
            std::fs::read_to_string(new_user.join("opencc").join("t2s.txt")).unwrap(),
            "中→忠\n",
            "嵌套子目录应一并复制"
        );

        // 已迁移（新目录存在）：再次执行不得改动新目录内容。
        std::fs::write(new_user.join("marker"), "kept").unwrap();
        migrate_legacy_user_data(&exe_dir, &new_user);
        assert_eq!(
            std::fs::read_to_string(new_user.join("marker")).unwrap(),
            "kept"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn migrate_legacy_user_data_missing_old_is_noop() {
        let base = temp_dir("noop");
        let exe_dir = base.join("app"); // 不存在 rime/user_data
        let new_user = base.join("userdata");
        migrate_legacy_user_data(&exe_dir, &new_user);
        assert!(!new_user.exists(), "旧位置缺失时应完全不触碰新目录");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn merge_extra_phrases_creates_file_verbatim_and_idempotent() {
        let base = temp_dir("extra-new");
        let target = base.join("custom_phrase.txt");
        let (appended, created) = merge_extra_phrases(&target).unwrap();
        assert!(created, "目标不存在应整体新建");
        assert!(
            appended >= 2,
            "内嵌词条应至少 2 行（biang 两形）: {appended}"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            RIME_EXTRA_PHRASES,
            "新建文件应为内嵌文件原样（含 yaml 头）"
        );
        // 幂等：二次合并不再追加
        let (again, created2) = merge_extra_phrases(&target).unwrap();
        assert!(!created2);
        assert_eq!(again, 0);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn merge_extra_phrases_appends_only_missing_entry_lines() {
        // 已存在带自身 yaml 头与部分词条的文件（含 \r\n 行模拟 Windows 产物）：
        // 只补缺失词条行——不重复文档头、不重复已有词条、不覆盖用户内容。
        let base = temp_dir("extra-merge");
        let target = base.join("custom_phrase.txt");
        std::fs::write(
            &target,
            "---\nname: custom_phrase\nversion: \"2020.01\"\nsort: by_weight\nuse_preset_vocabulary: true\n\n𰻝\tbiang\r\nuser\tzi ding ci\n",
        )
        .unwrap();
        let (appended, created) = merge_extra_phrases(&target).unwrap();
        assert!(!created);
        let out = std::fs::read_to_string(&target).unwrap();
        assert_eq!(out.matches("---\n").count(), 1, "yaml 文档头不得重复");
        assert_eq!(
            out.matches("𰻝\tbiang").count(),
            1,
            "已有词条（含 \\r\\n）不得重复追加"
        );
        assert_eq!(out.matches("𰻞\tbiang").count(), 1, "缺失词条应补上");
        assert!(out.contains("user\tzi ding ci"), "用户自建词条不得被覆盖");
        assert_eq!(appended, 1);
        // 幂等
        let (again, _) = merge_extra_phrases(&target).unwrap();
        assert_eq!(again, 0);
        let _ = std::fs::remove_dir_all(&base);
    }
}
