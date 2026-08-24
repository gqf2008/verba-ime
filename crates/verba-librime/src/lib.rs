//! 中文引擎（Rime/librime）封装：daemon 内动态加载 librime，查询拼音/五笔候选。
//!
//! - Windows / macOS（及可加载 librime 的 Unix）：用 `libloading` 动态加载
//!   （Windows `rime.dll` / macOS `librime.dylib`），RimeInitialize + 部署 +
//!   `RimeSimulateKeySequence` + `RimeGetContext` 取候选列表。
//! - 其它平台：stub（`Unsupported`），供 CI 编译通过。
//!
//! 线程安全：内部 C 状态由 librime 自身同步，本 crate 额外用 `Mutex` 串行化
//! （见 daemon 用法）；`RimeEngine` 为 `Send + Sync`。

#[cfg(any(windows, target_os = "macos"))]
pub mod platform;

#[cfg(not(any(windows, target_os = "macos")))]
mod stub;

use std::path::Path;
use thiserror::Error;

/// Rime 候选（词库候选 + 注释，如五笔编码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RimeCandidate {
    pub text: String,
    pub comment: String,
}

/// 方案信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RimeSchema {
    pub schema_id: String,
    pub name: String,
}

#[derive(Debug, Error)]
pub enum RimeError {
    #[error("当前平台未实现 Rime 加载")]
    Unsupported,
    #[error("加载 librime 失败: {0}")]
    Load(String),
    #[error("Rime 初始化失败: {0}")]
    Init(String),
    #[error("Rime 部署失败: {0}")]
    Deploy(String),
    #[error("Rime 输入处理失败: {0}")]
    Input(String),
}

/// Rime 引擎：加载后保持初始化状态，可重复查询。
#[cfg(any(windows, target_os = "macos"))]
pub type RimeEngine = platform::RimeEngine;

#[cfg(not(any(windows, target_os = "macos")))]
pub type RimeEngine = stub::RimeEngine;

/// 构造引擎的输入参数。
#[derive(Debug, Clone)]
pub struct RimeConfig {
    /// librime 库路径（Windows `rime.dll` / macOS `librime.dylib`）。
    pub dll_path: std::path::PathBuf,
    /// 共享数据目录（schema/dict yaml）。
    pub shared_data_dir: std::path::PathBuf,
    /// 用户数据目录（部署产物/用户词典，需可写）。
    pub user_data_dir: std::path::PathBuf,
}

impl RimeConfig {
    pub fn load(dll: &Path, shared: &Path, user: &Path) -> Self {
        Self {
            dll_path: dll.to_owned(),
            shared_data_dir: shared.to_owned(),
            user_data_dir: user.to_owned(),
        }
    }
}
