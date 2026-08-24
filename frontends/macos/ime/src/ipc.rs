//! macOS 前端与 daemon 的连接管理（与 Windows 前端 `ipc.rs` 对齐）。

use std::path::PathBuf;

use verba_ipc::{IpcError, VerbaClient};

/// 定位 daemon 可执行文件：
/// 1) 环境变量 VERBA_DAEMON_PATH
/// 2) 本可执行文件同目录 verba-daemon（.app 打包默认布局）
pub fn daemon_exe_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VERBA_DAEMON_PATH") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let candidate = exe.parent().map(|d| d.join("verba-daemon"));
        if let Some(c) = candidate {
            if c.exists() {
                return Some(c);
            }
        }
    }
    None
}

/// 定位设置面板可执行文件：VERBA_SETTINGS_PATH 或本可执行文件同目录 verba-settings。
pub fn settings_exe_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VERBA_SETTINGS_PATH") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            let candidate = d.join("verba-settings");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

/// 尝试连接 daemon。
pub fn try_connect() -> Result<VerbaClient, IpcError> {
    VerbaClient::connect()
}

/// 确保 daemon 运行并返回连接（带重试）。
///
/// daemon 为单实例：先试连；失败则按定位规则拉起，随后退避重试（
/// macOS 无「管道不存在立即报错」问题，统一走 50×100ms 重试即可）。
pub fn ensure_daemon() -> Result<VerbaClient, IpcError> {
    if let Ok(client) = try_connect() {
        return Ok(client);
    }
    if let Some(path) = daemon_exe_path() {
        log::info!("[mac-imk] 启动 daemon: {}", path.display());
        let _ = std::process::Command::new(&path).spawn();
    }
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(client) = try_connect() {
            return Ok(client);
        }
    }
    Err(IpcError::ConnectionClosed)
}
