//! Mock ASR provider：基于音频字节的确定性文本（无需模型，供开发与验收）。

use verba_ai::AsrProvider;

use crate::AsrError;

/// 本地 mock 转写器：确定性输出，同音频同文（验证 IPC 链路字节透传）。
#[derive(Debug, Clone, Copy, Default)]
pub struct MockAsr;

impl MockAsr {
    pub fn new() -> Self {
        Self
    }
}

/// FNV-1a 64 位哈希（音频内容指纹）。
fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl AsrProvider for MockAsr {
    type Error = AsrError;

    async fn transcribe(&self, audio: &[u8]) -> Result<String, AsrError> {
        if audio.is_empty() {
            return Err(AsrError::EmptyAudio);
        }
        let hash = fnv1a64(audio);
        Ok(format!("[mock-asr] bytes={} hash={hash:016x}", audio.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_deterministic() {
        let audio = b"fake-wav-bytes".to_vec();
        let a = MockAsr::new().transcribe(&audio).await.unwrap();
        let b = MockAsr::new().transcribe(&audio).await.unwrap();
        assert_eq!(a, b);
        assert!(a.contains("mock-asr"));
        assert!(a.contains(&format!("bytes={}", audio.len())));
    }

    #[tokio::test]
    async fn mock_differs_by_content() {
        let a = MockAsr::new()
            .transcribe(b"audio-A".as_slice())
            .await
            .unwrap();
        let b = MockAsr::new()
            .transcribe(b"audio-B".as_slice())
            .await
            .unwrap();
        assert_ne!(a, b, "不同音频应产出不同文本");
    }

    #[tokio::test]
    async fn mock_rejects_empty() {
        assert!(matches!(
            MockAsr::new().transcribe(&[]).await,
            Err(AsrError::EmptyAudio)
        ));
    }
}
