//! LLM provider：OpenAI 兼容 `POST {base}/chat/completions`，SSE 流式解析。

use std::pin::Pin;
use std::time::Duration;

use base64::Engine as _;
use eventsource_stream::Eventsource;
use futures_core::Stream;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde_json::json;
use thiserror::Error;

/// LLM 服务配置。
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// OpenAI 兼容基址（如 `https://api.deepseek.com/v1`）。
    pub base_url: String,
    /// API Key；为空时不带鉴权头（适配本地服务）。
    pub api_key: Option<String>,
    /// 模型名。
    pub model: String,
    /// 默认采样温度。
    pub temperature: f32,
    /// 默认最大生成 token 数。
    pub max_tokens: i32,
    /// 连接超时。
    pub connect_timeout: Duration,
}

impl LlmConfig {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            model: model.into(),
            temperature: 0.7,
            max_tokens: 1024,
            connect_timeout: Duration::from_secs(10),
        }
    }
}

/// 单次生成请求。
#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub prompt: String,
    pub system: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<i32>,
    /// 多模态图像（mime, 字节）；Some 时以 OpenAI image_url 发送。
    pub image: Option<(String, Vec<u8>)>,
    /// 历史消息（role, content），按序插在 system 之后、当前 prompt 之前。
    pub history: Vec<(String, String)>,
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("网络错误: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("SSE 流错误: {0}")]
    Stream(String),
    #[error("响应格式错误: {0}")]
    Format(String),
}

/// 图片请求失败时，把「模型/接口不接受图片输入」转成可执行提示。
///
/// OpenAI 兼容端点通常不暴露模型模态信息，无法在请求前可靠判断；这里只在
/// 已经携带图片且服务端返回客户端拒绝（400/422，或带 vision 关键词的 404）
/// 时兜底，并在提示里保留原始错误，避免把无关的 400 误判成「没有视觉能力」。
pub fn vision_error_hint(err: &LlmError, model: &str) -> Option<String> {
    let (reason, detail) = match err {
        LlmError::Http { status, body } if matches!(status, 400 | 422) => {
            (format!("HTTP {status}"), body.as_str())
        }
        LlmError::Http { status, body } if *status == 404 && mentions_vision(body) => {
            (format!("HTTP {status}"), body.as_str())
        }
        LlmError::Stream(msg) if mentions_vision(msg) => ("流式错误".to_owned(), msg.as_str()),
        _ => return None,
    };
    // 只有服务端文本明确提到 image/vision/多模态时才下“模型不支持视觉”的结论；
    // 其它 400/422（图片过大、格式错误、上下文/参数超限等）只提示图片请求被
    // 拒绝并附原始错误，避免把无关客户端错误误归因给模型能力。
    if mentions_vision(detail) {
        // 动作前置：macOS 候选面板是单列不换行、只显示前 40 字，必须保证
        // “换视觉模型 / 改用 //截图”落在安全宽度内；完整原始错误随后附上
        // （Windows 自绘浮层可完整显示，macOS 详情另见 daemon 日志）。
        Some(format!(
            "图片未识别：请换支持视觉的模型，或改用 `//截图` 走内置 OCR。当前模型 `{model}` 拒绝了图片输入（{reason}）。\n服务端返回：{detail}"
        ))
    } else {
        Some(format!(
            "图片请求失败：请检查图片格式/大小或上下文限制。模型 `{model}` 返回 {reason}。\n服务端返回：{detail}"
        ))
    }
}

fn mentions_vision(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["image", "vision", "multimodal", "图片", "视觉", "多模态"]
        .iter()
        .any(|kw| lower.contains(kw))
}

/// LLM 客户端。
#[derive(Debug, Clone)]
pub struct LlmClient {
    http: reqwest::Client,
}

impl LlmClient {
    pub fn new() -> Result<Self, LlmError> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self { http })
    }

    /// 列出模型（GET /models，OpenAI 兼容；当前 provider 仅 DeepSeek）。
    /// 需 API Key（Bearer 鉴权）——未配置时返回格式错误。
    pub async fn list_models(&self, cfg: &LlmConfig) -> Result<Vec<String>, LlmError> {
        let url = format!("{}/models", cfg.base_url.trim_end_matches('/'));
        let mut req = self.http.get(&url);
        if let Some(key) = &cfg.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(LlmError::Http {
                status: status.as_u16(),
                body,
            });
        }
        #[derive(serde::Deserialize)]
        struct ModelsResp {
            data: Vec<ModelItem>,
        }
        #[derive(serde::Deserialize)]
        struct ModelItem {
            id: String,
        }
        let parsed: ModelsResp = serde_json::from_str(&body)
            .map_err(|e| LlmError::Format(format!("模型列表解析失败: {e}")))?;
        Ok(parsed.data.into_iter().map(|m| m.id).collect())
    }

    /// 发起流式生成，返回内容增量流。
    pub async fn stream(
        &self,
        cfg: &LlmConfig,
        req: &LlmRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String, LlmError>> + Send>>, LlmError> {
        let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
        let mut messages = Vec::new();
        if let Some(system) = req.system.as_deref().filter(|s| !s.is_empty()) {
            messages.push(json!({ "role": "system", "content": system }));
        }
        for (role, content) in &req.history {
            messages.push(json!({ "role": role, "content": content }));
        }
        if let Some((mime, data)) = &req.image {
            let b64 = base64::engine::general_purpose::STANDARD.encode(data);
            messages.push(json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": req.prompt},
                    {"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{b64}")}}
                ]
            }));
        } else {
            messages.push(json!({ "role": "user", "content": req.prompt }));
        }

        let body = json!({
            "model": cfg.model,
            "messages": messages,
            "temperature": req.temperature.unwrap_or(cfg.temperature),
            "max_tokens": req.max_tokens.unwrap_or(cfg.max_tokens),
            "stream": true,
        });

        let mut request = self.http.post(&url).json(&body);
        if let Some(key) = cfg.api_key.as_deref().filter(|k| !k.is_empty()) {
            request = request.bearer_auth(key);
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::Http { status, body });
        }

        let byte_stream = response.bytes_stream();
        let stream = byte_stream.eventsource().filter_map(|ev| async move {
            match ev {
                Ok(ev) => match parse_sse(&ev.data) {
                    Ok(Some(text)) => Some(Ok(text)),
                    Ok(None) => None, // [DONE] 或空行
                    Err(e) => Some(Err(e)),
                },
                Err(e) => Some(Err(LlmError::Stream(e.to_string()))),
            }
        });
        Ok(Box::pin(stream))
    }
}

/// 解析一条 SSE 数据。`Ok(None)` 表示流结束（`[DONE]`）或忽略。
fn parse_sse(data: &str) -> Result<Option<String>, LlmError> {
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| LlmError::Format(format!("JSON 解析失败: {e}")))?;
    // 部分 OpenAI 兼容端点以 HTTP 200 + SSE error payload 报错（例如模型不接受
    // image_url）。此前只取 choices[0].delta.content，会把这类错误当空流吞掉，
    // 最终发空 Final；这里显式转成流错误，让上层能做能力提示。
    if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
        let msg = err
            .get("message")
            .and_then(serde_json::Value::as_str)
            .filter(|m| !m.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| err.to_string());
        return Err(LlmError::Stream(format!("服务端流式错误: {msg}")));
    }
    // choices 缺失 / null / 空数组都视为“没有正常增量”；此时若有顶层
    // message，按流错误处理，避免 {"message":"...","choices":[]} 被吞成空 Final。
    let choices_empty = value
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .is_none_or(Vec::is_empty);
    if choices_empty {
        if let Some(msg) = value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .filter(|m| !m.is_empty())
        {
            return Err(LlmError::Stream(format!("服务端流式错误: {msg}")));
        }
    }
    let content = value
        .pointer("/choices/0/delta/content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if content.is_empty() {
        Ok(None)
    } else {
        Ok(Some(content.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stream_parses_sse_from_mock_server() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let thread = std::thread::spawn(move || {
            if let Some(request) = server.incoming_requests().next() {
                let body = concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n\n",
                    "data: [DONE]\n\n"
                );
                let header =
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/event-stream"[..])
                        .unwrap();
                let response = tiny_http::Response::from_string(body).with_header(header);
                let _ = request.respond(response);
            }
        });

        let cfg = LlmConfig::new(
            format!("http://127.0.0.1:{port}/v1"),
            Some("sk-test".into()),
            "test-model",
        );
        let client = LlmClient::new().unwrap();
        let req = LlmRequest {
            prompt: "你好".into(),
            system: None,
            temperature: None,
            max_tokens: None,
            image: None,
            history: Vec::new(),
        };
        let mut stream = client.stream(&cfg, &req).await.unwrap();
        let mut out = String::new();
        while let Some(chunk) = stream.next().await {
            out.push_str(&chunk.unwrap());
        }
        assert_eq!(out, "你好");
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn stream_sends_history_messages() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let cap = captured.clone();
        let thread = std::thread::spawn(move || {
            if let Some(mut request) = server.incoming_requests().next() {
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                *cap.lock().unwrap() = body;
                let header =
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/event-stream"[..])
                        .unwrap();
                let response =
                    tiny_http::Response::from_string("data: [DONE]\n\n").with_header(header);
                let _ = request.respond(response);
            }
        });

        let cfg = LlmConfig::new(
            format!("http://127.0.0.1:{port}/v1"),
            Some("sk-test".into()),
            "test-model",
        );
        let client = LlmClient::new().unwrap();
        let req = LlmRequest {
            prompt: "这是第二句".into(),
            system: None,
            temperature: None,
            max_tokens: None,
            image: None,
            history: vec![
                ("user".into(), "这是第一句".into()),
                ("assistant".into(), "好的".into()),
            ],
        };
        let mut stream = client.stream(&cfg, &req).await.unwrap();
        while stream.next().await.is_some() {}
        thread.join().unwrap();
        let body = captured.lock().unwrap().clone();
        assert!(body.contains("这是第一句"), "应包含历史 user");
        assert!(body.contains("这是第二句"), "应包含当前 prompt");
        assert!(body.contains("\"assistant\""), "应包含 assistant 历史");
    }

    #[tokio::test]
    async fn stream_sends_vision_image_as_data_url() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let cap = captured.clone();
        let thread = std::thread::spawn(move || {
            if let Some(mut request) = server.incoming_requests().next() {
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                *cap.lock().unwrap() = body;
                let header =
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/event-stream"[..])
                        .unwrap();
                let response =
                    tiny_http::Response::from_string("data: [DONE]\n\n").with_header(header);
                let _ = request.respond(response);
            }
        });

        let cfg = LlmConfig::new(
            format!("http://127.0.0.1:{port}/v1"),
            Some("sk-test".into()),
            "vision-model",
        );
        let client = LlmClient::new().unwrap();
        let req = LlmRequest {
            prompt: "看图".into(),
            system: None,
            temperature: None,
            max_tokens: None,
            image: Some(("image/png".into(), b"\x89PNG".to_vec())),
            history: Vec::new(),
        };
        let mut stream = client.stream(&cfg, &req).await.unwrap();
        while stream.next().await.is_some() {}
        thread.join().unwrap();
        let body = captured.lock().unwrap().clone();
        assert!(body.contains("image_url"), "应包含 image_url");
        assert!(body.contains("data:image/png;base64,"), "应包含 data URL");
        assert!(
            body.contains("\"model\":\"vision-model\""),
            "应使用指定模型"
        );
    }
    #[test]
    fn vision_error_hint_maps_client_rejection() {
        let err = LlmError::Http {
            status: 400,
            body: "model does not support image input".into(),
        };
        let hint = vision_error_hint(&err, "deepseek-flash").unwrap();
        assert!(hint.contains("deepseek-flash"));
        assert!(hint.contains("//截图"));
        assert!(hint.contains("model does not support image input"));
        assert!(
            hint.chars().take(40).collect::<String>().contains("//截图"),
            "动作必须在 macOS 40 字安全宽度内: {hint}"
        );
    }

    #[test]
    fn vision_error_hint_does_not_misattribute_unrelated_client_errors() {
        let err = LlmError::Http {
            status: 422,
            body: "temperature must be between 0 and 2".into(),
        };
        let hint = vision_error_hint(&err, "m").unwrap();
        assert!(hint.contains("图片请求失败"));
        assert!(!hint.contains("图片未识别"));
        assert!(hint.contains("temperature must be between 0 and 2"));
    }

    #[test]
    fn vision_error_hint_maps_stream_vision_rejection() {
        let err = LlmError::Stream("unexpected content type image_url".into());
        let hint = vision_error_hint(&err, "text-only").unwrap();
        assert!(hint.contains("text-only"));
        assert!(hint.contains("图片输入"));
    }

    #[test]
    fn vision_error_hint_ignores_auth_server_and_generic_errors() {
        let auth = LlmError::Http {
            status: 401,
            body: "invalid api key".into(),
        };
        let server = LlmError::Http {
            status: 500,
            body: "internal error".into(),
        };
        let reset = LlmError::Stream("connection reset".into());
        let not_found = LlmError::Http {
            status: 404,
            body: "model not found".into(),
        };
        assert!(vision_error_hint(&auth, "m").is_none());
        assert!(vision_error_hint(&server, "m").is_none());
        assert!(vision_error_hint(&reset, "m").is_none());
        assert!(vision_error_hint(&not_found, "m").is_none());
    }

    #[test]
    fn parse_sse_surfaces_error_payload() {
        let err =
            parse_sse(r#"{"error":{"message":"model does not support image input"}}"#).unwrap_err();
        match err {
            LlmError::Stream(msg) => assert!(msg.contains("does not support image input")),
            other => panic!("期望 Stream 错误，得到 {other:?}"),
        }
        let msg = parse_sse(r#"{"message":"invalid image_url"}"#).unwrap_err();
        assert!(matches!(msg, LlmError::Stream(_)));

        // choices 为 [] / null 时，顶层 message 仍是错误（不能吞成空 Final）。
        for payload in [
            r#"{"message":"model does not support image input","choices":[]}"#,
            r#"{"message":"model does not support image input","choices":null}"#,
        ] {
            match parse_sse(payload).unwrap_err() {
                LlmError::Stream(msg) => assert!(msg.contains("does not support image input")),
                other => panic!("期望 Stream 错误，得到 {other:?}"),
            }
        }

        // error object 没有 message 时，保留完整 error JSON 作为详情。
        match parse_sse(r#"{"error":{"code":"invalid_image"}}"#).unwrap_err() {
            LlmError::Stream(msg) => assert!(msg.contains("invalid_image")),
            other => panic!("期望 Stream 错误，得到 {other:?}"),
        }

        // 正常 chunk 携带 "error": null 或仅 usage 时不得误判为错误。
        assert_eq!(
            parse_sse(r#"{"error":null,"choices":[{"delta":{"content":"你好"}}]}"#).unwrap(),
            Some("你好".into())
        );
        assert_eq!(
            parse_sse(r#"{"choices":[],"usage":{"total_tokens":1}}"#).unwrap(),
            None
        );
    }

    #[test]
    fn parse_sse_handles_done_and_junk() {
        assert_eq!(parse_sse("[DONE]").unwrap(), None);
        assert_eq!(parse_sse("").unwrap(), None);
        assert_eq!(
            parse_sse(r#"{"choices":[{"delta":{"content":"Hi"}}]}"#).unwrap(),
            Some("Hi".into())
        );
        assert!(parse_sse("not json").is_err());
    }
}
