//! Verba 核心引擎：输入状态机与领域模型。
//!
//! M1 范围：普通直输 + `//` AI 触发 + 流式结果上屏的组合状态机，
//! 全部逻辑可离线单测，平台前端只做「状态 → 系统动作」的映射。

#![forbid(unsafe_code)]

pub mod machine;

pub use machine::{Action, CompositionMachine, MachineState, Mode};

/// 当前核心版本。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// AI 触发前缀（用户连续输入两个 `/` 进入 AI 提示词模式）。
pub const AI_TRIGGER: &str = "//";
