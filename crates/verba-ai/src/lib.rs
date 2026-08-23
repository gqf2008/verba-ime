//! Verba AI 能力层。
//!
//! M1 实现 LLM（远程，OpenAI 兼容，SSE 流式）；OCR/ASR/TTS 的 trait
//! 先定义占位，M3 落地实现。

#![forbid(unsafe_code)]

pub mod llm;
pub mod traits;

pub use llm::{LlmClient, LlmConfig, LlmError, LlmRequest};
pub use traits::{AsrProvider, OcrProvider, TtsAudio, TtsProvider};
