//! Verba ASR 能力：语音 → 文字。
//!
//! provider 由 config `asr_provider` 选择：mock（确定性，开发/验收）| openai（OpenAI 兼容
//! 在线转写，audio/transcriptions）。每个 provider 实现 `verba_ai::AsrProvider`，
//! `AsrClient` 按配置分发。

#![forbid(unsafe_code)]

pub mod mock;
pub mod openai;

use std::str::FromStr;

use thiserror::Error;
use verba_ai::AsrProvider;

pub use mock::MockAsr;
pub use openai::{OpenAiAsr, DEFAULT_ASR_MODEL};

/// ASR 错误。
#[derive(Debug, Error)]
pub enum AsrError {
    #[error("未知 ASR provider: {0}（当前支持 mock|openai）")]
    UnknownProvider(String),
    #[error("音频为空")]
    EmptyAudio,
    #[error("在线 ASR 错误: {0}")]
    Openai(String),
}

/// 已实现的 ASR provider。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsrProviderKind {
    /// 本地 mock：确定性文本（开发/验收）。
    Mock,
    /// OpenAI 兼容在线转写（真实联网，audio/transcriptions）。
    Openai,
}

impl FromStr for AsrProviderKind {
    type Err = AsrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "" | "mock" => Ok(Self::Mock),
            "openai" => Ok(Self::Openai),
            other => Err(AsrError::UnknownProvider(other.to_owned())),
        }
    }
}

/// ASR 客户端：按配置 provider 分发转写请求。
#[derive(Debug, Clone)]
pub struct AsrClient {
    provider: AsrProviderKind,
    base_url: String,
    api_key: Option<String>,
    model: String,
}

impl AsrClient {
    /// 按配置创建（provider: mock|openai；base_url/api_key/model 供在线 provider 使用，mock 忽略）。
    pub fn from_config(
        provider: &str,
        base_url: &str,
        api_key: Option<&str>,
        model: &str,
    ) -> Result<Self, AsrError> {
        Ok(Self {
            provider: provider.parse()?,
            base_url: base_url.to_owned(),
            api_key: api_key.map(str::to_owned),
            model: model.to_owned(),
        })
    }

    /// 转写音频 → 文字。
    pub async fn transcribe(&self, audio: &[u8]) -> Result<String, AsrError> {
        if audio.is_empty() {
            return Err(AsrError::EmptyAudio);
        }
        match &self.provider {
            AsrProviderKind::Mock => MockAsr::new().transcribe(audio).await,
            AsrProviderKind::Openai => {
                OpenAiAsr::new(
                    self.base_url.clone(),
                    self.api_key.clone(),
                    self.model.clone(),
                )
                .transcribe(audio)
                .await
            }
        }
    }

    /// 当前 provider。
    pub fn provider(&self) -> &AsrProviderKind {
        &self.provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn client_deterministic_mock() {
        let c = AsrClient::from_config("mock", "", None, "").unwrap();
        let a = c.transcribe(b"wav-1".as_slice()).await.unwrap();
        let b = c.transcribe(b"wav-1".as_slice()).await.unwrap();
        assert_eq!(a, b);
        assert!(a.contains("mock-asr"));
    }

    #[tokio::test]
    async fn client_rejects_empty() {
        let c = AsrClient::from_config("mock", "", None, "").unwrap();
        assert!(matches!(c.transcribe(&[]).await, Err(AsrError::EmptyAudio)));
    }

    #[tokio::test]
    async fn client_openai_roundtrip() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let thread = std::thread::spawn(move || {
            if let Some(mut request) = server.incoming_requests().next() {
                let mut body = Vec::new();
                let _ = request.as_reader().read_to_end(&mut body);
                let response = tiny_http::Response::from_string(r#"{"text":"你是谁"}"#)
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
        let c = AsrClient::from_config(
            "openai",
            &format!("http://127.0.0.1:{port}/v1"),
            Some("sk-test"),
            "whisper-1",
        )
        .unwrap();
        let text = c.transcribe(b"RIFF-wav".as_slice()).await.unwrap();
        assert_eq!(text, "你是谁");
        thread.join().unwrap();
    }

    #[test]
    fn provider_parsing() {
        assert_eq!(
            "mock".parse::<AsrProviderKind>().unwrap(),
            AsrProviderKind::Mock
        );
        assert_eq!(
            "".parse::<AsrProviderKind>().unwrap(),
            AsrProviderKind::Mock
        );
        assert_eq!(
            "openai".parse::<AsrProviderKind>().unwrap(),
            AsrProviderKind::Openai
        );
        assert!(
            "whisper".parse::<AsrProviderKind>().is_err(),
            "whisper 未实现"
        );
    }

    #[test]
    fn client_unknown_provider_rejected() {
        assert!(matches!(
            AsrClient::from_config("whisper", "", None, ""),
            Err(AsrError::UnknownProvider(_))
        ));
    }
}
