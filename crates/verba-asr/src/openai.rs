//! OpenAI 兼容在线 ASR provider：`POST {base}/audio/transcriptions`。
//!
//! 复用 LLM 远程通道（base_url + api_key），multipart 上传音频，返回识别文本。
//! 适用于 OpenAI Whisper API，及提供 OpenAI 兼容 audio/transcriptions 端点的服务商。

use std::time::Duration;

use reqwest::multipart::{Form, Part};
use verba_ai::AsrProvider;

use crate::AsrError;

/// 默认 ASR 模型（OpenAI 兼容端点通用 whisper 模型名）。
pub const DEFAULT_ASR_MODEL: &str = "whisper-1";

/// OpenAI 兼容在线 ASR（真实联网，需 base_url + api_key）。
#[derive(Debug, Clone)]
pub struct OpenAiAsr {
    base_url: String,
    api_key: Option<String>,
    model: String,
    http: reqwest::Client,
}

impl OpenAiAsr {
    /// 创建在线 ASR；model 为空时回退默认 whisper-1。
    pub fn new(base_url: String, api_key: Option<String>, model: String) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        let model = if model.is_empty() {
            DEFAULT_ASR_MODEL.to_owned()
        } else {
            model
        };
        Self {
            base_url,
            api_key,
            model,
            http,
        }
    }
}

impl AsrProvider for OpenAiAsr {
    type Error = AsrError;

    async fn transcribe(&self, audio: &[u8]) -> Result<String, AsrError> {
        if audio.is_empty() {
            return Err(AsrError::EmptyAudio);
        }
        let base = self
            .base_url
            .trim_end_matches(|c: char| c == char::from(47));
        let url = format!("{base}/audio/transcriptions");
        let form = Form::new()
            .part(
                "file",
                Part::bytes(audio.to_vec())
                    .file_name("audio.wav")
                    .mime_str("audio/wav")
                    .map_err(|e| AsrError::Openai(format!("构造上传失败: {e}")))?,
            )
            .text("model", self.model.clone());
        let mut request = self.http.post(&url).multipart(form);
        if let Some(key) = self.api_key.as_deref().filter(|k| !k.is_empty()) {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|e| AsrError::Openai(format!("网络错误: {e}")))?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(AsrError::Openai(format!("HTTP {status}: {body}")));
        }
        let body = response
            .text()
            .await
            .map_err(|e| AsrError::Openai(format!("读取响应失败: {e}")))?;
        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| AsrError::Openai(format!("响应 JSON 解析失败: {e}")))?;
        let text = value
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AsrError::Openai(format!("响应缺少 text 字段: {body}")))?;
        Ok(text.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transcribes_via_multipart_upload() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let thread = std::thread::spawn(move || {
            if let Some(mut request) = server.incoming_requests().next() {
                assert_eq!(request.method(), &tiny_http::Method::Post);
                let content_type = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Content-Type"))
                    .map(|h| h.value.as_str())
                    .unwrap_or_default();
                assert!(
                    content_type.starts_with("multipart/form-data"),
                    "应 multipart 上传: {content_type}"
                );
                let mut body = Vec::new();
                request.as_reader().read_to_end(&mut body).unwrap();
                assert!(!body.is_empty(), "multipart body 非空");
                let response = tiny_http::Response::from_string(r#"{"text":"你好世界"}"#)
                    .with_header(
                        tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"application/json"[..],
                        )
                        .unwrap(),
                    );
                let _ = request.respond(response);
            }
        });
        let asr = OpenAiAsr::new(
            format!("http://127.0.0.1:{port}/v1"),
            Some("sk-test".into()),
            "whisper-1".into(),
        );
        let out = asr.transcribe(b"RIFF-fake-wav".as_slice()).await.unwrap();
        assert_eq!(out, "你好世界");
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn empty_audio_rejected() {
        let asr = OpenAiAsr::new("http://127.0.0.1:9".into(), None, String::new());
        assert!(matches!(
            asr.transcribe(&[]).await,
            Err(AsrError::EmptyAudio)
        ));
    }

    #[test]
    fn default_model_fallback() {
        let asr = OpenAiAsr::new("http://x".into(), None, String::new());
        assert_eq!(asr.model, DEFAULT_ASR_MODEL);
    }
}
