//! 前端与 daemon 的连接管理。

use std::path::PathBuf;

use verba_ipc::{IpcError, VerbaClient};

/// 定位 daemon 可执行文件：
/// 1) 环境变量 VERBA_DAEMON_PATH
/// 2) DLL 同目录 verba-daemon.exe（开发/安装默认）
pub fn daemon_exe_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VERBA_DAEMON_PATH") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(dll) = crate::reg::dll_path() {
        let candidate = dll.with_file_name("verba-daemon.exe");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// 尝试连接 daemon。
pub fn try_connect() -> Result<VerbaClient, IpcError> {
    VerbaClient::connect()
}

/// 确保 daemon 运行并返回连接（带重试）。
pub fn ensure_daemon() -> Result<VerbaClient, IpcError> {
    if let Ok(client) = try_connect() {
        return Ok(client);
    }
    if let Some(path) = daemon_exe_path() {
        log::info!("启动 daemon: {}", path.display());
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
