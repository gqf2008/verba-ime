//! Verba ASR 能力：语音 → 文字。
//!
//! provider 由 config `asr_provider` 选择：mock（确定性，开发/验收）；whisper.cpp 等真实
//! provider 后续接入。每个 provider 实现 `verba_ai::AsrProvider`，`AsrClient` 按配置分发。

#![forbid(unsafe_code)]

pub mod mock;

use std::str::FromStr;

use thiserror::Error;
use verba_ai::AsrProvider;

pub use mock::MockAsr;

/// ASR 错误。
#[derive(Debug, Error)]
pub enum AsrError {
    #[error("未知 ASR provider: {0}（当前支持 mock；whisper.cpp 开发中）")]
    UnknownProvider(String),
    #[error("音频为空")]
    EmptyAudio,
}

/// 已实现的 ASR provider。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsrProviderKind {
    /// 本地 mock：确定性文本（开发/验收）。
    Mock,
}

impl FromStr for AsrProviderKind {
    type Err = AsrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "" | "mock" => Ok(Self::Mock),
            other => Err(AsrError::UnknownProvider(other.to_owned())),
        }
    }
}

/// ASR 客户端：按配置 provider 分发转写请求。
#[derive(Debug, Clone)]
pub struct AsrClient {
    provider: AsrProviderKind,
}

impl AsrClient {
    /// 按配置创建（provider: mock|…）。
    pub fn from_config(provider: &str) -> Result<Self, AsrError> {
        Ok(Self {
            provider: provider.parse()?,
        })
    }

    /// 转写音频 → 文字。
    pub async fn transcribe(&self, audio: &[u8]) -> Result<String, AsrError> {
        if audio.is_empty() {
            return Err(AsrError::EmptyAudio);
        }
        match &self.provider {
            AsrProviderKind::Mock => MockAsr::new().transcribe(audio).await,
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
        let c = AsrClient::from_config("mock").unwrap();
        let a = c.transcribe(b"wav-1".as_slice()).await.unwrap();
        let b = c.transcribe(b"wav-1".as_slice()).await.unwrap();
        assert_eq!(a, b);
        assert!(a.contains("mock-asr"));
    }

    #[tokio::test]
    async fn client_rejects_empty() {
        let c = AsrClient::from_config("mock").unwrap();
        assert!(matches!(c.transcribe(&[]).await, Err(AsrError::EmptyAudio)));
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
        assert!(
            "whisper".parse::<AsrProviderKind>().is_err(),
            "whisper 未实现"
        );
    }

    #[test]
    fn client_unknown_provider_rejected() {
        assert!(matches!(
            AsrClient::from_config("whisper"),
            Err(AsrError::UnknownProvider(_))
        ));
    }
}
