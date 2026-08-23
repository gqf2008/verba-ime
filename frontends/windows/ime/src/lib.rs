//! Verba Windows TSF 文本服务（输入法 DLL）。
//!
//! 结构（薄壳 + 共享核心 + daemon）：
//! - [`dll`]：COM 导出（DllGetClassObject / DllRegisterServer / ...）
//! - [`class_factory`]：IClassFactory，创建 TextService
//! - [`text_service`]：TSF 文本服务（按键/组合/候选 + 与 daemon 的 LLM 流式）
//! - [`reg`]：注册表与 TSF 类别注册
//! - [`ipc`]：daemon 连接管理
//!
//! 本 crate 独立于根 workspace（仅 Windows 目标）。

mod candidate_window;
pub mod capture;
mod class_factory;
mod dll;
mod edit_session;
mod filelog;
mod guids;
pub mod ipc;
pub mod play;
pub mod record;
pub mod reg;
pub mod selection;
pub mod text_service;

pub use dll::{DllGetClassObject, DllRegisterServer, DllUnregisterServer};
pub use guids::{CLSID_VERBA_TEXT_SERVICE, PROFILE_VERBA};

/// 触发能力（截图 / 录音 / 播放 / daemon 分发）错误。
#[derive(Debug)]
pub enum TriggerError {
    /// 截图失败。
    Capture(String),
    /// 录音失败。
    Record(String),
    /// 音频播放失败。
    Play(String),
    /// daemon 调用失败。
    Daemon(String),
}

impl std::fmt::Display for TriggerError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            TriggerError::Capture(m) => write!(f, "截图失败: {m}"),
            TriggerError::Record(m) => write!(f, "录音失败: {m}"),
            TriggerError::Play(m) => write!(f, "播放失败: {m}"),
            TriggerError::Daemon(m) => write!(f, "daemon 调用失败: {m}"),
        }
    }
}

impl std::error::Error for TriggerError {}
