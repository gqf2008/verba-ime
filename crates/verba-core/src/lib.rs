//! Verba 核心引擎：输入状态机与领域模型。
//!
//! 当前为 M0 骨架，随路线图逐步填充（模式状态机、composition 缓冲、
//! 候选模型、命令路由等）。

#![forbid(unsafe_code)]

/// 当前核心版本。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 输入模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 普通输入：英文直输、标点、快捷指令。
    Normal,
    /// 语音输入（ASR）。
    Voice,
    /// 截图 / 图片 OCR。
    Ocr,
    /// AI 模式（LLM）。
    Ai,
}

impl Mode {
    /// 该模式下是否允许普通按键直接上屏（ASCII 字符）。
    pub fn accepts_direct_input(self) -> bool {
        matches!(self, Self::Normal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_mode_accepts_direct_input() {
        assert!(Mode::Normal.accepts_direct_input());
    }

    #[test]
    fn non_normal_modes_block_direct_input() {
        for mode in [Mode::Voice, Mode::Ocr, Mode::Ai] {
            assert!(!mode.accepts_direct_input());
        }
    }

    #[test]
    fn version_is_semver_compatible() {
        assert!(VERSION.split('.').count() >= 2);
    }
}
