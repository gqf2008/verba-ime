//! Verba OCR 能力：图片/截图 → 文字。
//!
//! provider 由 config `ocr_provider` 选择：mock（确定性，开发/验收）| windows（Windows.Media.Ocr
//! 本地识别，零下载）。每个 provider 实现 `verba_ai::OcrProvider`，`OcrClient` 按配置分发。

// 白名单 crate：Cargo.toml 放开 unsafe_code（仅 windows_media.rs 经 SAFETY 注释使用）。

pub mod mock;
pub mod rapid;

use std::str::FromStr;

use thiserror::Error;
use verba_ai::OcrProvider;

#[cfg(windows)]
mod windows_media;
#[cfg(windows)]
use windows_media::WindowsMediaOcr;

pub use mock::MockOcr;
pub use rapid::RapidOcr;

/// OCR 错误。
#[derive(Debug, Error)]
pub enum OcrError {
    #[error("未知 OCR provider: {0}（当前支持 mock|windows|rapid）")]
    UnknownProvider(String),
    #[error("图像为空")]
    EmptyImage,
    #[error("Windows OCR 失败: {0}")]
    Windows(String),
    #[error("RapidOCR 失败: {0}")]
    Rapid(String),
}

/// 已实现的 OCR provider。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrProviderKind {
    /// 本地 mock：确定性文本（开发/验收）。
    Mock,
    /// Windows.Media.Ocr 本地识别（仅 Windows）。
    WindowsMedia,
    /// RapidOCR（PaddleOCR + ONNXRuntime，经 Python 子进程）本地识别。
    Rapid,
}

impl FromStr for OcrProviderKind {
    type Err = OcrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "" | "mock" => Ok(Self::Mock),
            "windows" => Ok(Self::WindowsMedia),
            "rapid" => Ok(Self::Rapid),
            other => Err(OcrError::UnknownProvider(other.to_owned())),
        }
    }
}

/// OCR 客户端：按配置 provider 分发识别请求。
#[derive(Debug, Clone)]
pub struct OcrClient {
    provider: OcrProviderKind,
    rapid_python: Option<String>,
}

impl OcrClient {
    /// 按配置创建（provider: mock|windows）。
    pub fn from_config(provider: &str, rapid_python: &str) -> Result<Self, OcrError> {
        Ok(Self {
            provider: provider.parse()?,
            rapid_python: if rapid_python.is_empty() {
                None
            } else {
                Some(rapid_python.to_owned())
            },
        })
    }

    /// 识别图像 → 文字。
    pub async fn recognize(&self, image: &[u8]) -> Result<String, OcrError> {
        if image.is_empty() {
            return Err(OcrError::EmptyImage);
        }
        match &self.provider {
            OcrProviderKind::Mock => MockOcr::new().recognize(image).await,
            OcrProviderKind::Rapid => {
                let py = self.rapid_python.clone().unwrap_or_default();
                RapidOcr::with_python(py).recognize(image).await
            }
            OcrProviderKind::WindowsMedia => {
                #[cfg(windows)]
                {
                    WindowsMediaOcr::new().recognize(image).await
                }
                #[cfg(not(windows))]
                {
                    Err(OcrError::UnknownProvider(
                        "windows（仅 Windows 可用）".into(),
                    ))
                }
            }
        }
    }

    /// 当前 provider。
    pub fn provider(&self) -> &OcrProviderKind {
        &self.provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn client_deterministic_mock() {
        let c = OcrClient::from_config("mock", "").unwrap();
        let a = c.recognize(b"png-1".as_slice()).await.unwrap();
        let b = c.recognize(b"png-1".as_slice()).await.unwrap();
        assert_eq!(a, b);
        assert!(a.contains("mock-ocr"));
    }

    #[tokio::test]
    async fn client_rejects_empty() {
        let c = OcrClient::from_config("mock", "").unwrap();
        assert!(matches!(c.recognize(&[]).await, Err(OcrError::EmptyImage)));
    }

    #[test]
    fn provider_parsing() {
        assert_eq!(
            "mock".parse::<OcrProviderKind>().unwrap(),
            OcrProviderKind::Mock
        );
        assert_eq!(
            "".parse::<OcrProviderKind>().unwrap(),
            OcrProviderKind::Mock
        );
        assert_eq!(
            "windows".parse::<OcrProviderKind>().unwrap(),
            OcrProviderKind::WindowsMedia
        );
        assert_eq!(
            "rapid".parse::<OcrProviderKind>().unwrap(),
            OcrProviderKind::Rapid
        );
        assert!("bogus".parse::<OcrProviderKind>().is_err());
    }

    #[test]
    fn client_unknown_provider_rejected() {
        assert!(matches!(
            OcrClient::from_config("bogus", ""),
            Err(OcrError::UnknownProvider(_))
        ));
    }
}
