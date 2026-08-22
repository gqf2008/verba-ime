//! AI provider 统一抽象（M1 仅 LlmProvider 落地）。

use std::pin::Pin;

use futures_core::Stream;
use std::future::Future;

/// 装箱的异步流：`Ok(T)` 元素或 `Err(E)` 终止。
pub type BoxedStream<T, E> = Pin<Box<dyn Stream<Item = Result<T, E>> + Send>>;

/// OCR（图片/截图 → 文字）。M3 实现。
pub trait OcrProvider: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;
    fn recognize(&self, image: &[u8]) -> impl Future<Output = Result<String, Self::Error>> + Send;
}

/// ASR（语音 → 文字）。M3 实现。
pub trait AsrProvider: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;
    fn transcribe(&self, audio: &[u8]) -> impl Future<Output = Result<String, Self::Error>> + Send;
}

/// TTS（文字 → 语音）。M4 实现。
pub trait TtsProvider: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;
    fn speak(&self, text: &str) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// LLM（远程，流式）。
pub trait LlmProvider: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;
    fn stream(
        &self,
        request: &crate::LlmRequest,
        config: &crate::LlmConfig,
    ) -> impl Future<Output = Result<BoxedStream<String, Self::Error>, Self::Error>> + Send;
}
