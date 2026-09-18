//! macOS 前端与 daemon 的连接管理（与 Windows 前端 `ipc.rs` 对齐）。

use std::path::{Path, PathBuf};

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

/// 设置面板 .app 在 bundle 内的相对位置（`Contents/Library/Verba Settings.app`）。
pub const SETTINGS_APP_REL: &str = "Library/Verba Settings.app";

/// 由输入法可执行文件路径推出设置 app 路径（纯函数，便于单测）：
/// `…/Contents/MacOS/verba-mac` → `…/Contents/Library/Verba Settings.app`。
fn settings_app_from_exe(exe: &Path) -> Option<PathBuf> {
    let contents = exe.parent()?.parent()?;
    Some(contents.join(SETTINGS_APP_REL))
}

/// 定位设置面板 **.app bundle**：`VERBA_SETTINGS_APP` 或打包布局下的嵌套 app。
///
/// 为什么优先用 `.app` 而不是裸二进制：菜单里 `Command::spawn()` 直接起二进制
/// **不走 LaunchServices**——窗口不起到前台、还会开出多个实例（2026-09-18 真机
/// 实测：设置窗口一直躲在其它窗口后面）。拿到 `.app` 后由调用方 `/usr/bin/open`
/// 打开，即「已运行则激活、不新开实例」。
pub fn settings_app_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VERBA_SETTINGS_APP") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let candidate = settings_app_from_exe(&exe)?;
    candidate.exists().then_some(candidate)
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
    // connect_verified 内含验活握手（架构审查 P0-1）：连接成功不代表对端是
    // 真实 daemon；能回 Pong 的才信任。socket 已移入用户私有目录（0700），
    // 此检查为纵深防御。
    VerbaClient::connect_verified()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_app_from_exe_uses_library_layout() {
        let exe = Path::new("/Applications/Verba.app/Contents/MacOS/verba-mac");
        assert_eq!(
            settings_app_from_exe(exe).as_deref(),
            Some(Path::new(
                "/Applications/Verba.app/Contents/Library/Verba Settings.app"
            )),
            "设置 app 固定在 Contents/Library/Verba Settings.app"
        );
        // 非标准布局（裸二进制）也算得出来，但 exists() 会挡住它
        assert!(settings_app_from_exe(Path::new("/tmp/verba-mac")).is_some());
    }
}
