//! verba-daemon 可执行入口。
//!
//! release 构建为 GUI 子系统（不弹控制台）：daemon 由输入法/安装器以无窗口方式
//! 拉起，日志经 TeeLog 落盘 verba-daemon.log；debug 构建保留控制台，便于
//! `verba-cli daemon` 前台调试。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(e) = verba_daemon::run_default() {
        // release（GUI 子系统）下无控制台句柄：eprintln! 会 panic 且信息丢失，
        // 仅 debug 构建打印；日志已由 TeeLog 落盘 verba-daemon.log（日志初始化
        // 之前的失败如 VerbaDirs::locate，只能靠退出码 1 观测，开发期由 debug 输出）。
        if cfg!(debug_assertions) {
            eprintln!("daemon 启动失败: {e}");
        }
        std::process::exit(1);
    }
}
