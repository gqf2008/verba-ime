//! 连接 daemon：不在运行则拉起同目录 verba-daemon 并退避重连。
//!
//! 自 bin（verba-trigger.rs）迁入 lib：悬浮按钮的编排（float.rs）运行在
//! 库内 worker 线程，与 bin 子命令共用同一条「握手失败 → 拉起同目录
//! daemon → 退避重连」路径（原 ensure_daemon 的跨平台版）。

use std::process::Command;
use std::time::Duration;

use verba_ipc::VerbaClient;

use crate::TriggerError;

/// 连接 daemon；不在运行则拉起同目录 verba-daemon 并退避重连。
pub fn connect_daemon() -> Result<VerbaClient, TriggerError> {
    if let Ok(c) = VerbaClient::connect_verified() {
        return Ok(c);
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_owned()))
        .ok_or_else(|| TriggerError::Daemon("无法定位自身目录".into()))?;
    let daemon = exe_dir.join(if cfg!(windows) {
        "verba-daemon.exe"
    } else {
        "verba-daemon"
    });
    if !daemon.is_file() {
        return Err(TriggerError::Daemon(format!(
            "未找到 daemon（{}），无法自动拉起",
            daemon.display()
        )));
    }
    let mut cmd = Command::new(&daemon);
    // stdio 全部落 null：daemon 常驻不退出，若继承本进程 stdout 管道写端，
    // 调用方的 .output() 将永不 EOF（独立审查 NOTE——结果静默丢失根因）。
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：daemon 为控制台子系统构建（debug），防闪窗
        cmd.creation_flags(0x08000000);
    }
    cmd.spawn()
        .map_err(|e| TriggerError::Daemon(format!("拉起 daemon 失败: {e}")))?;
    // 退避重连：daemon 启动 + 首次部署预热期间 socket 就绪需要时间
    for attempt in 0..20 {
        std::thread::sleep(Duration::from_millis(if attempt < 10 { 150 } else { 400 }));
        if let Ok(c) = VerbaClient::connect_verified() {
            return Ok(c);
        }
    }
    Err(TriggerError::Daemon("daemon 拉起后连接失败".into()))
}
