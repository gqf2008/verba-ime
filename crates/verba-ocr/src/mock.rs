//! Mock OCR provider：基于图像字节的确定性文本（无需模型，供开发与验收）。

use verba_ai::OcrProvider;

use crate::OcrError;

/// 本地 mock 识别器：确定性输出，同图同文（验证 IPC 链路字节透传）。
#[derive(Debug, Clone, Copy, Default)]
pub struct MockOcr;

impl MockOcr {
    pub fn new() -> Self {
        Self
    }
}

/// FNV-1a 64 位哈希（图像内容指纹）。
fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl OcrProvider for MockOcr {
    type Error = OcrError;

    async fn recognize(&self, image: &[u8]) -> Result<String, OcrError> {
        if image.is_empty() {
            return Err(OcrError::EmptyImage);
        }
        let hash = fnv1a64(image);
        Ok(format!("[mock-ocr] bytes={} hash={hash:016x}", image.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_deterministic() {
        let img = b"fake-png-bytes-123".to_vec();
        let a = MockOcr::new().recognize(&img).await.unwrap();
        let b = MockOcr::new().recognize(&img).await.unwrap();
        assert_eq!(a, b);
        assert!(a.contains("mock-ocr"));
        assert!(a.contains(&format!("bytes={}", img.len())), "应含字节数");
    }

    #[tokio::test]
    async fn mock_differs_by_content() {
        let a = MockOcr::new().recognize(b"img-A".as_slice()).await.unwrap();
        let b = MockOcr::new().recognize(b"img-B".as_slice()).await.unwrap();
        assert_ne!(a, b, "不同图像应产出不同文本");
    }

    #[tokio::test]
    async fn mock_rejects_empty() {
        assert!(matches!(
            MockOcr::new().recognize(&[]).await,
            Err(OcrError::EmptyImage)
        ));
    }
}
