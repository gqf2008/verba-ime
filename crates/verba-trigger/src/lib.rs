//! Verba 跨平台触发能力库：选区截图、屏幕截取、录音、播放。
//!
//! 设计约束（issue #82，用户架构指令）：所有能力跨平台单实现——选区 UI 用
//! winit 单代码库、截屏用 xcap（内部封装 Win32 GDI / macOS CoreGraphics /
//! Linux X11）、录音/播放用 cpal/rodio；平台差异只允许出现在「库内部后端」
//! 与「薄单位适配层」（macOS 全局坐标为点、Windows 为物理像素）。
//! 文本提交等真平台限制由各前端自行接线，不在本库范围。

pub mod bmp;
pub mod capture;
pub mod play;
pub mod record;
pub mod selection;

/// 触发能力错误。
#[derive(Debug)]
pub enum TriggerError {
    Capture(String),
    Record(String),
    Play(String),
    Daemon(String),
}

impl std::fmt::Display for TriggerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TriggerError::Capture(m) => write!(f, "截屏错误: {m}"),
            TriggerError::Record(m) => write!(f, "录音错误: {m}"),
            TriggerError::Play(m) => write!(f, "播放错误: {m}"),
            TriggerError::Daemon(m) => write!(f, "daemon 错误: {m}"),
        }
    }
}

impl std::error::Error for TriggerError {}
