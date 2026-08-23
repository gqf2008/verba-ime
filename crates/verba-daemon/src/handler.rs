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

pub struct DaemonHandler {
    mgr: ConfigManager,
    config: Arc<RwLock<Config>>,
    llm_config: Arc<RwLock<LlmConfig>>,
    llm: LlmClient,
    cancels: Mutex<HashMap<u64, CancellationToken>>,
    /// 可选 Rime 引擎（config 引擎=rime 时惰性加载；串行化访问）。
    rime: Mutex<Option<RimeEngine>>,
    /// AI 多轮上下文（role, content）；config ai_context_turns>0 时使用。
    history: Mutex<VecDeque<(String, String)>>,
}

impl DaemonHandler {
    pub fn new(mgr: ConfigManager, config: Config, llm_config: LlmConfig, llm: LlmClient) -> Self {
        Self {
            mgr,
            config: Arc::new(RwLock::new(config)),
            llm_config: Arc::new(RwLock::new(llm_config)),
            llm,
            cancels: Mutex::new(HashMap::new()),
            rime: Mutex::new(None),
            history: Mutex::new(VecDeque::new()),
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
            Some(request::Kind::RimeCandidates(g)) => self.handle_rime_candidates(id, g, out).await,
            Some(request::Kind::TtsSynthesize(g)) => self.handle_tts_synthesize(id, g, out).await,
            Some(request::Kind::OcrRecognize(g)) => self.handle_ocr_recognize(id, g, out).await,
            Some(request::Kind::AsrTranscribe(g)) => self.handle_asr_transcribe(id, g, out).await,
            Some(request::Kind::ApiKeySet(g)) => self.handle_api_key_set(id, g, out).await,
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

        let (mut llm_cfg, config_system) = self.llm_snapshot();
        let LlmGenerate {
            prompt,
            system,
            temperature,
            max_tokens,
            stream: _,
            image,
            image_mime,
        } = g;
        let image = image.map(|data| {
            let mime = image_mime.clone().unwrap_or_else(|| "image/png".to_owned());
            (mime, data)
        });
        let user_prompt = prompt.clone();
        let context_turns = self.config.read().unwrap().ai_context_turns.max(0) as usize;
        let trimmed = user_prompt.trim();
        if trimmed == "重置" || trimmed == "reset" {
            self.history.lock().unwrap().clear();
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
        let has_image = image.is_some();
        let mut history = Vec::new();
        if !has_image && context_turns > 0 {
            let guard = self.history.lock().unwrap();
            let start = guard.len().saturating_sub(context_turns * 2);
            for (role, content) in guard.iter().skip(start) {
                history.push((role.clone(), content.clone()));
            }
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
                            kind: Some(stream_event::Kind::Final(Final {
                                text: final_text.clone(),
                            })),
                        })
                        .await;
                    // 记录这一轮（文本 AI），供下一轮上下文；图像请求不入历史。
                    if !has_image && context_turns > 0 {
                        let mut guard = self.history.lock().unwrap();
                        guard.push_back(("user".to_owned(), user_prompt.clone()));
                        guard.push_back(("assistant".to_owned(), final_text.clone()));
                        let max = context_turns * 2;
                        while guard.len() > max {
                            guard.pop_front();
                        }
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

        self.cancels.lock().unwrap().remove(&id);
        Ok(())
    }

    /// Rime 引擎候选（config 引擎=rime）：一次性推送 `Candidates` 事件（done=true）。
    async fn handle_rime_candidates(
        &self,
        id: u64,
        g: verba_protos::RimeCandidates,
        out: Outbound,
    ) -> Result<(), verba_ipc::IpcError> {
        let engine = self.config.read().unwrap().engine.clone();
        if engine != "rime" {
            out.response(&Response {
                id,
                kind: Some(response::Kind::Error(ProtoError {
                    code: 400,
                    message: "中文引擎未启用 rime（verba-cli config set engine=rime 后重试）"
                        .into(),
                })),
            })
            .await?;
            return Ok(());
        }
        let max = (g.max_candidates as usize).clamp(1, 27);
        let schema = if g.schema.is_empty() {
            "luna_pinyin_simp"
        } else {
            g.schema.as_str()
        };
        match self.rime_query(|e| e.candidates(&g.input, schema, max)) {
            Ok(cands) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Ok(OkMsg {})),
                })
                .await?;
                out.event(&StreamEvent {
                    id,
                    kind: Some(stream_event::Kind::Candidates(Candidates {
                        pinyin: g.input.clone(),
                        candidates: cands.into_iter().map(|c| c.text).collect(),
                        done: true,
                    })),
                })
                .await
            }
            Err(e) => {
                out.response(&Response {
                    id,
                    kind: Some(response::Kind::Error(ProtoError {
                        code: 502,
                        message: e,
                    })),
                })
                .await
            }
        }
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
        let (provider, rapid_python) = {
            let cfg = self.config.read().unwrap();
            (cfg.ocr_provider.clone(), cfg.ocr_rapid_python.clone())
        };
        let client = match verba_ocr::OcrClient::from_config(&provider, &rapid_python) {
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

    /// 后台预热 Rime 引擎（engine=rime 时由 daemon 启动调用）：提前触发首次部署，
    /// 使首次候选查询免于等待部署（2-5 秒），避免用户误以为无反应。
    pub fn warmup_rime(&self) {
        let engine = self.config.read().unwrap().engine.clone();
        if engine != "rime" {
            return;
        }
        match self.rime_query(|_| Ok(())) {
            Ok(()) => log::info!("Rime 引擎预热完成"),
            Err(e) => log::warn!("Rime 引擎预热失败（首次查询时会重试）: {e}"),
        }
    }

    /// 惰性加载并串行执行 Rime 查询（无 await，锁不跨异步点）。
    fn rime_query<T>(
        &self,
        f: impl FnOnce(&RimeEngine) -> Result<T, verba_librime::RimeError>,
    ) -> Result<T, String> {
        let mut guard = self.rime.lock().unwrap();
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
/// `rime.dll`、`data/`、`user_data/`。
fn rime_paths() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let from_env = || -> Option<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
        let d = std::env::var("VERBA_RIME_DLL").ok()?;
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
    (
        rime_dir.join("rime.dll"),
        rime_dir.join("data"),
        rime_dir.join("user_data"),
    )
}
