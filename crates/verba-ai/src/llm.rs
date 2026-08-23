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
