//! `verba-mac`：macOS 输入法可执行入口。
//!
//! 作为 IMK 输入法 `.app` 的 `CFBundleExecutable`，注册 `IMKServer` 并进入
//! AppKit 主循环；`CFBundleIdentifier` / `InputMethodServerControllerClass`
//! 等元数据由 `app/Info.plist` 提供。
//!
//! PKG postinstall 会通过 LaunchServices 以 `--register` 启动本二进制的一个
//! 短命实例：它调用同 bundle 的 `verba-register`，把退出码写入 postinstall
//! 指定的状态文件后退出，不会进入 IMK 主循环。

#[cfg(target_os = "macos")]
use std::io::Write;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
#[derive(Default)]
struct RegisterMode {
    status_path: Option<String>,
    log_path: Option<String>,
}

#[cfg(target_os = "macos")]
fn parse_register_mode(args: &[String]) -> Option<RegisterMode> {
    if !args.iter().any(|arg| arg == "--register") {
        return None;
    }
    let mut mode = RegisterMode::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--status" => {
                mode.status_path = args.get(i + 1).cloned();
                i += 2;
            }
            "--log" => {
                mode.log_path = args.get(i + 1).cloned();
                i += 2;
            }
            _ => i += 1,
        }
    }
    Some(mode)
}

#[cfg(target_os = "macos")]
fn write_register_log(path: Option<&str>, message: &str) {
    if let Some(path) = path {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{message}");
        }
    }
}

#[cfg(target_os = "macos")]
const REGISTER_HELPER_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(target_os = "macos")]
fn run_registration(log_path: Option<&str>) -> i32 {
    let helper = match std::env::current_exe() {
        Ok(exe) => exe.with_file_name("verba-register"),
        Err(e) => {
            let message = format!("错误: 无法定位 verba-register: {e}");
            eprintln!("{message}");
            write_register_log(log_path, &message);
            return 1;
        }
    };
    if !helper.is_file() {
        let message = format!(
            "错误: 找不到同 bundle 的 verba-register: {}",
            helper.display()
        );
        eprintln!("{message}");
        write_register_log(log_path, &message);
        return 1;
    }

    let mut command = Command::new(&helper);
    if let Some(path) = log_path {
        match std::fs::File::create(path) {
            Ok(file) => match file.try_clone() {
                Ok(stderr) => {
                    command.stdout(Stdio::from(file));
                    command.stderr(Stdio::from(stderr));
                }
                Err(e) => {
                    eprintln!("警告: 复制 helper 日志句柄失败: {e}");
                }
            },
            Err(e) => {
                eprintln!("警告: 创建 helper 日志文件 {path} 失败: {e}");
            }
        }
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let message = format!("错误: 启动 verba-register 失败: {e}");
            eprintln!("{message}");
            write_register_log(log_path, &message);
            return 1;
        }
    };
    let deadline = Instant::now() + REGISTER_HELPER_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code().unwrap_or(1),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let message = format!(
                    "错误: verba-register 超过 {} 秒未退出，已终止",
                    REGISTER_HELPER_TIMEOUT.as_secs()
                );
                eprintln!("{message}");
                write_register_log(log_path, &message);
                return 1;
            }
            Err(e) => {
                let message = format!("错误: 等待 verba-register 失败: {e}");
                eprintln!("{message}");
                write_register_log(log_path, &message);
                return 1;
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn run_register_mode() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--register-probe") {
        println!("verba-register-mode-supported");
        return Some(0);
    }
    let mode = parse_register_mode(&args)?;
    let status = run_registration(mode.log_path.as_deref());
    if let Some(path) = mode.status_path.as_deref() {
        if let Err(e) = std::fs::write(path, format!("{status}\n")) {
            let message = format!("错误: 写入注册状态 {path} 失败: {e}");
            eprintln!("{message}");
            write_register_log(mode.log_path.as_deref(), &message);
        }
    }
    Some(status)
}

fn main() {
    #[cfg(target_os = "macos")]
    {
        if let Some(status) = run_register_mode() {
            std::process::exit(status);
        }
        verba_ime_macos::run_imk_server();
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("verba-mac 仅支持 macOS");
        std::process::exit(1);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn parse_register_mode_extracts_status_and_log() {
        let args = vec![
            "--register".to_owned(),
            "--status".to_owned(),
            "/tmp/status".to_owned(),
            "--log".to_owned(),
            "/tmp/log".to_owned(),
        ];
        let mode = parse_register_mode(&args).expect("应识别 --register");
        assert_eq!(mode.status_path.as_deref(), Some("/tmp/status"));
        assert_eq!(mode.log_path.as_deref(), Some("/tmp/log"));
    }

    #[test]
    fn parse_register_mode_requires_register_flag() {
        let args = vec!["--status".to_owned(), "/tmp/status".to_owned()];
        assert!(parse_register_mode(&args).is_none());
    }
}
