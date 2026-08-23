//! OpenAI 兼容在线 TTS provider：`POST {base}/audio/speech`，返回 MP3 音频。
//!
//! 复用 LLM 远程通道（base_url + api_key），适用于 OpenAI TTS API，及提供 OpenAI
//! 兼容 audio/speech 端点的服务商。

use std::time::Duration;

use verba_ai::{TtsAudio, TtsProvider};

use crate::TtsError;

/// 默认 TTS 模型（OpenAI 兼容端点通用 tts-1）。
pub const DEFAULT_OPENAI_MODEL: &str = "tts-1";
/// 默认音色（OpenAI 标准音色名）。
pub const DEFAULT_OPENAI_VOICE: &str = "alloy";

/// OpenAI 兼容在线 TTS（真实联网，需 base_url + api_key）。
#[derive(Debug, Clone)]
pub struct OpenAiTts {
    base_url: String,
    api_key: Option<String>,
    model: String,
    voice: String,
    http: reqwest::Client,
}

impl OpenAiTts {
    /// 创建在线 TTS；model/voice 为空时回退默认。
    pub fn new(base_url: String, api_key: Option<String>, model: String, voice: String) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        let model = if model.is_empty() {
            DEFAULT_OPENAI_MODEL.to_owned()
        } else {
            model
        };
        let voice = if voice.is_empty() {
            DEFAULT_OPENAI_VOICE.to_owned()
        } else {
            voice
        };
        Self {
            base_url,
            api_key,
            model,
            voice,
            http,
        }
    }
}

impl TtsProvider for OpenAiTts {
    type Error = TtsError;

    async fn synthesize(&self, text: &str) -> Result<TtsAudio, TtsError> {
        if text.trim().is_empty() {
            return Err(TtsError::EmptyText);
        }
        let base = self
            .base_url
            .trim_end_matches(|c: char| c == char::from(47));
        let url = format!("{base}/audio/speech");
        let body = serde_json::json!({
            "model": self.model,
            "input": text,
            "voice": self.voice,
        });
        let mut request = self.http.post(&url).json(&body);
        if let Some(key) = self.api_key.as_deref().filter(|k| !k.is_empty()) {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|e| TtsError::Openai(format!("网络错误: {e}")))?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(TtsError::Openai(format!("HTTP {status}: {body}")));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| TtsError::Openai(format!("读取音频失败: {e}")))?;
        Ok(TtsAudio {
            format: "mp3",
            bytes: bytes.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn synthesizes_mp3_from_audio_speech() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let thread = std::thread::spawn(move || {
            if let Some(mut request) = server.incoming_requests().next() {
                assert_eq!(request.method(), &tiny_http::Method::Post);
                let ct = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Content-Type"))
                    .map(|h| h.value.as_str())
                    .unwrap_or_default();
                assert!(ct.starts_with("application/json"), "应 JSON 请求: {ct}");
                let mut body = Vec::new();
                let _ = request.as_reader().read_to_end(&mut body);
                assert!(!body.is_empty(), "JSON body 非空");
                let response = tiny_http::Response::from_data(b"ID3\x04fake-mp3".to_vec())
                    .with_header(
                        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"audio/mpeg"[..])
                            .unwrap(),
                    );
                let _ = request.respond(response);
            }
        });
        let tts = OpenAiTts::new(
            format!("http://127.0.0.1:{port}/v1"),
            Some("sk-test".into()),
            "tts-1".into(),
            "alloy".into(),
        );
        let audio = tts.synthesize("你好").await.unwrap();
        assert_eq!(audio.format, "mp3");
        assert_eq!(audio.bytes, b"ID3\x04fake-mp3".to_vec());
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn empty_text_rejected() {
        let tts = OpenAiTts::new(
            "http://127.0.0.1:9".into(),
            None,
            String::new(),
            String::new(),
        );
        assert!(matches!(
            tts.synthesize("  ").await,
            Err(TtsError::EmptyText)
        ));
    }

    #[test]
    fn defaults_filled() {
        let tts = OpenAiTts::new("http://x".into(), None, String::new(), String::new());
        assert_eq!(tts.model, DEFAULT_OPENAI_MODEL);
        assert_eq!(tts.voice, DEFAULT_OPENAI_VOICE);
    }
}
