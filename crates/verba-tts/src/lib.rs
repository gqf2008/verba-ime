//! Verba TTS 能力：文字 → 语音音频。
//!
//! M4 落地：provider 由 config `tts_provider` 选择（当前 mock；edge/Piper/系统 TTS 后续）。
//! 每个 provider 实现 `verba_ai::TtsProvider`，`TtsClient` 按配置分发并统一返回音频字节。

#![forbid(unsafe_code)]

pub mod mock;
pub mod wav;

use std::str::FromStr;

use thiserror::Error;
use verba_ai::{TtsAudio, TtsProvider};

pub use mock::MockTts;

/// TTS 错误。
#[derive(Debug, Error)]
pub enum TtsError {
    #[error("未知 TTS provider: {0}（当前支持 mock）")]
    UnknownProvider(String),
    #[error("文本为空")]
    EmptyText,
}

/// 已实现的 TTS provider。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TtsProviderKind {
    /// 本地 mock：确定性 WAV 合成（开发/验收）。
    Mock,
}

impl FromStr for TtsProviderKind {
    type Err = TtsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "" | "mock" => Ok(Self::Mock),
            other => Err(TtsError::UnknownProvider(other.to_owned())),
        }
    }
}

/// TTS 客户端：按配置 provider 分发合成请求。
#[derive(Debug, Clone)]
pub struct TtsClient {
    provider: TtsProviderKind,
    voice: String,
}

impl TtsClient {
    /// 按配置创建（provider: mock|…；voice 为语音名，mock 忽略）。
    pub fn from_config(provider: &str, voice: &str) -> Result<Self, TtsError> {
        Ok(Self {
            provider: provider.parse()?,
            voice: voice.to_owned(),
        })
    }

    /// 合成文本 → 音频字节。
    pub async fn synthesize(&self, text: &str) -> Result<TtsAudio, TtsError> {
        if text.trim().is_empty() {
            return Err(TtsError::EmptyText);
        }
        match &self.provider {
            TtsProviderKind::Mock => MockTts::new().synthesize(text).await,
        }
    }

    /// 当前语音名（mock 忽略）。
    pub fn voice(&self) -> &str {
        &self.voice
    }

    /// 当前 provider。
    pub fn provider(&self) -> &TtsProviderKind {
        &self.provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use verba_ai::TtsProvider;

    #[tokio::test]
    async fn mock_produces_valid_wav() {
        let audio = MockTts::new().synthesize("你好").await.unwrap();
        assert_eq!(audio.format, "wav");
        assert_eq!(&audio.bytes[0..4], b"RIFF");
        assert_eq!(&audio.bytes[8..12], b"WAVE");
        assert_eq!(&audio.bytes[36..40], b"data");
        let data_len = u32::from_le_bytes(audio.bytes[40..44].try_into().unwrap()) as usize;
        assert_eq!(audio.bytes.len(), 44 + data_len);
        assert!(data_len > 0, "mock 音频应非空");
    }

    #[tokio::test]
    async fn mock_duration_scales_with_text() {
        let a = MockTts::new().synthesize("你").await.unwrap();
        let b = MockTts::new().synthesize("你好世界").await.unwrap();
        assert!(b.bytes.len() > a.bytes.len(), "更长文本应合成更长音频");
    }

    #[tokio::test]
    async fn mock_deterministic() {
        let a = MockTts::new().synthesize("你好").await.unwrap();
        let b = MockTts::new().synthesize("你好").await.unwrap();
        assert_eq!(a.bytes, b.bytes, "同输入应产出相同字节");
    }

    #[test]
    fn provider_parsing() {
        assert_eq!(
            "mock".parse::<TtsProviderKind>().unwrap(),
            TtsProviderKind::Mock
        );
        assert_eq!(
            "".parse::<TtsProviderKind>().unwrap(),
            TtsProviderKind::Mock
        );
        assert!("edge".parse::<TtsProviderKind>().is_err(), "edge 未实现");
    }

    #[tokio::test]
    async fn client_rejects_empty_text() {
        let c = TtsClient::from_config("mock", "").unwrap();
        assert!(matches!(c.synthesize("  ").await, Err(TtsError::EmptyText)));
        assert!(matches!(
            TtsClient::from_config("edge", ""),
            Err(TtsError::UnknownProvider(_))
        ));
    }
}
