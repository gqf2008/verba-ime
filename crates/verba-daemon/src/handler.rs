//! IPC 请求处理器。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;
use verba_ai::{LlmClient, LlmConfig, LlmRequest};
use verba_config::{Config, ConfigManager};
use verba_core::VERSION;
use verba_ipc::server::{Outbound, RequestHandler};
use verba_protos::{
    request, response, stream_event, Candidates, Chunk, Config as ConfigMsg, Error as ProtoError,
    Final, LlmCandidates, Ok as OkMsg, Pong, Response, StreamEvent,
};

/// 默认 AI 系统提示词（用户未配置时使用）。
const DEFAULT_AI_SYSTEM: &str =
    "你是一个输入法里的 AI 助手。回答应简洁、直接，以可上屏的文本输出，不要使用 Markdown。";

/// 候选融合系统提示词：只输出候选本身，便于按行解析。
const CANDIDATE_SYSTEM: &str = "你是输入法智能候选生成器。根据用户输入的拼音串生成中文候选。只输出候选本身，每行一个；不要编号、不要序号、不要标点、不要任何解释或前后缀。";

/// 候选融合单次请求上限。
const CANDIDATE_MAX: usize = 6;

pub struct DaemonHandler {
    mgr: ConfigManager,
    config: Arc<RwLock<Config>>,
    llm_config: Arc<RwLock<LlmConfig>>,
    llm: LlmClient,
    cancels: Mutex<HashMap<u64, CancellationToken>>,
}

impl DaemonHandler {
    pub fn new(mgr: ConfigManager, config: Config, llm_config: LlmConfig, llm: LlmClient) -> Self {
        Self {
            mgr,
            config: Arc::new(RwLock::new(config)),
            llm_config: Arc::new(RwLock::new(llm_config)),
            llm,
            cancels: Mutex::new(HashMap::new()),
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
    async fn handle(&self, req: verba_protos::Request, out: Outbound) {
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
            Some(request::Kind::LlmGenerate(g)) => self.handle_llm_generate(id, g, out).await,
            Some(request::Kind::LlmCandidates(g)) => self.handle_llm_candidates(id, g, out).await,
            Some(request::Kind::LlmCancel(_)) => {
                let token = self.cancels.lock().unwrap().remove(&id);
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
        id: u64,
        g: verba_protos::LlmGenerate,
        out: Outbound,
    ) -> Result<(), verba_ipc::IpcError> {
        let token = CancellationToken::new();
        self.cancels.lock().unwrap().insert(id, token.clone());

        out.response(&Response {
            id,
            kind: Some(response::Kind::Ok(OkMsg {})),
        })
        .await?;

        let (llm_cfg, config_system) = self.llm_snapshot();
        let system = g
            .system
            .filter(|s| !s.is_empty())
            .or_else(|| (!config_system.is_empty()).then_some(config_system))
            .or_else(|| Some(DEFAULT_AI_SYSTEM.to_owned()));

        let req = LlmRequest {
            prompt: g.prompt,
            system,
            temperature: g.temperature,
            max_tokens: g.max_tokens,
        };

        match self.llm.stream(&llm_cfg, &req).await {
            Ok(mut stream) => {
                let mut failed = false;
                let mut final_text = String::new();
                tokio::select! {
                    _ = token.cancelled() => {
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
                if !failed {
                    let _ = out
                        .event(&StreamEvent {
                            id,
                            kind: Some(stream_event::Kind::Final(Final { text: final_text })),
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

        self.cancels.lock().unwrap().remove(&id);
        Ok(())
    }

    /// 候选融合：请求 LLM 为拼音补充候选，按行流式解析并增量推送
    /// `Candidates` 事件（去重 + 去编号），结束（含取消）时补发 `done=true`。
    async fn handle_llm_candidates(
        &self,
        id: u64,
        g: verba_protos::LlmCandidates,
        out: Outbound,
    ) -> Result<(), verba_ipc::IpcError> {
        let token = CancellationToken::new();
        self.cancels.lock().unwrap().insert(id, token.clone());

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

        self.cancels.lock().unwrap().remove(&id);
        Ok(())
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
