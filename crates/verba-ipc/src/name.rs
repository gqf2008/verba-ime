//! 本地 socket 命名与平台分支。
//!
//! 安全背景（架构审查 P0-1）：`GenericNamespaced` 在 macOS 落到写死的
//! `/tmp/{name}`（全局共享、粘滞位目录），Linux 为 abstract socket
//! （无 inode 权限，任何用户可连），Windows 为机器级全局管道
//! `\\.\pipe\{name}`——均可被其他用户预占（永久 DoS）或假冒 daemon
//! 窃取 API key / 提示词 / 截图 / 录音。因此：
//!
//! - Unix：socket 放**用户数据目录**（macOS `~/Library/Application Support/Verba`、
//!   Linux `~/.local/share/verba`），daemon 启动时创建目录并 chmod 0700，
//!   跨用户不可见不可连；命名用 `FilesystemUdSocket`（完整路径）。
//! - Windows：管道名带用户名后缀 `verba-ime-{USERNAME}`（per-user 隔离）。

use std::path::PathBuf;

/// 默认 socket 规格：
/// - Unix：用户数据目录下 socket 的完整路径（`GenericFilePath` 原样使用）
/// - Windows：`\\.\pipe\verba-ime-{USERNAME}`（per-user 命名管道）
pub fn default_socket_spec() -> String {
    #[cfg(unix)]
    {
        socket_path().display().to_string()
    }
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME")
            .unwrap_or_else(|_| "default".into())
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>();
        format!(r"\\.\pipe\verba-ime-{user}")
    }
}

/// Unix 下用户数据目录中的 socket 完整路径（与 [`crate::name::default_socket_spec`] 对应）。
#[cfg(unix)]
pub fn socket_path() -> PathBuf {
    let data_dir = directories::ProjectDirs::from("dev", "verba", "Verba")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("verba-ime"));
    data_dir.join("verba-ipc.sock")
}
