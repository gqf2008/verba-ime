//! verba-daemon 可执行入口。

fn main() {
    if let Err(e) = verba_daemon::run_default() {
        eprintln!("daemon 启动失败: {e}");
        std::process::exit(1);
    }
}
