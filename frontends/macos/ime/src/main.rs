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

/// 探针：在**本 app 进程内**（由 LaunchServices 经 `open -n` 在用户会话里启动）调用
/// TextInputSources 的注册/启用，验证「输入法授权弹窗是否只在 GUI 进程里触发」。
///
/// 背景：CLI（verba-register）里调 TISEnableInputSource 不弹窗、返回 0 却无效——
/// 白名单与 HIToolbox 列表都写对、TIS 属性与 Apple 自家输入法完全一致，
/// TISSelectInputSource 返回 0 后系统仍不切换、不拉起进程、菜单也不列。
#[cfg(target_os = "macos")]
mod tis_probe {
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};
    use core_foundation::url::CFURL;
    use std::ffi::c_void;
    use std::time::Duration;

    const VERBA_SOURCE_ID: &str = "dev.verba.inputmethod.Verba";
    const VERBA_MODE_ID: &str = "dev.verba.inputmethod.Verba.Pinyin";

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn TISRegisterInputSource(location: *const c_void) -> i32;
        fn TISEnableInputSource(source: *const c_void) -> i32;
        fn TISCreateInputSourceList(
            properties: *const c_void,
            include_all_installed: bool,
        ) -> *const c_void;
        fn TISGetInputSourceProperty(source: *const c_void, key: *const c_void) -> *mut c_void;
        // key 必须用系统常量本身：TISGetInputSourceProperty 按指针比较 key。
        static kTISPropertyInputSourceID: *const c_void;
        static kTISPropertyInputSourceIsEnabled: *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: *const c_void) -> i64;
        fn CFArrayGetValueAtIndex(array: *const c_void, index: i64) -> *const c_void;
        fn CFBooleanGetValue(boolean: *const c_void) -> bool;
    }

    /// 该源此刻是否 enabled。
    fn source_enabled(want: &str) -> bool {
        let raw = unsafe { TISCreateInputSourceList(std::ptr::null(), true) };
        if raw.is_null() {
            return false;
        }
        let _owned = unsafe { CFArray::<*const c_void>::wrap_under_create_rule(raw as CFArrayRef) };
        let want_cf = CFString::new(want);
        for i in 0..unsafe { CFArrayGetCount(raw) } {
            let src = unsafe { CFArrayGetValueAtIndex(raw, i) };
            // SAFETY: src 来自上一行的数组元素，数组在本函数内保持存活。
            let id_prop = unsafe { TISGetInputSourceProperty(src, kTISPropertyInputSourceID) };
            if id_prop.is_null() {
                continue;
            }
            let id = unsafe { CFString::wrap_under_get_rule(id_prop as CFStringRef) };
            if id != want_cf {
                continue;
            }
            // SAFETY: 同上；enabled 属性是 CFBoolean。
            let enabled =
                unsafe { TISGetInputSourceProperty(src, kTISPropertyInputSourceIsEnabled) };
            return !enabled.is_null() && unsafe { CFBooleanGetValue(enabled) };
        }
        false
    }

    /// 按 ID 找源并 `TISEnableInputSource`；-1=没找到，-2=取不到列表。
    fn enable_by_id(want: &str) -> i32 {
        let raw = unsafe { TISCreateInputSourceList(std::ptr::null(), true) };
        if raw.is_null() {
            return -2;
        }
        let _owned = unsafe { CFArray::<*const c_void>::wrap_under_create_rule(raw as CFArrayRef) };
        let want_cf = CFString::new(want);
        for i in 0..unsafe { CFArrayGetCount(raw) } {
            let src = unsafe { CFArrayGetValueAtIndex(raw, i) };
            let id_prop = unsafe { TISGetInputSourceProperty(src, kTISPropertyInputSourceID) };
            if id_prop.is_null() {
                continue;
            }
            let id = unsafe { CFString::wrap_under_get_rule(id_prop as CFStringRef) };
            if id != want_cf {
                continue;
            }
            // SAFETY: src 元素在本函数内有效。
            return unsafe { TISEnableInputSource(src) };
        }
        -1
    }

    /// 跑探针，返回写进 register log 的报告。
    pub fn run(app: &std::path::Path) -> String {
        let mut out = String::new();
        let url = CFURL::from_path(app, true).expect("app 路径应可构造 CFURL");
        let url_ref = url.as_concrete_TypeRef() as *const c_void;
        let reg = unsafe { TISRegisterInputSource(url_ref) };
        out.push_str(&format!("[probe] 进程内 TISRegisterInputSource rc={reg}"));

        let parent_rc = enable_by_id(VERBA_SOURCE_ID);
        let mode_rc = enable_by_id(VERBA_MODE_ID);
        out.push_str(&format!(
            "\n[probe] 进程内 TISEnableInputSource 父源 rc={parent_rc} mode rc={mode_rc}"
        ));
        for i in 1..=5 {
            let p = source_enabled(VERBA_SOURCE_ID);
            let m = source_enabled(VERBA_MODE_ID);
            out.push_str(&format!("\n[probe] 轮询 {i}: parent={p} mode={m}"));
            if p && m {
                break;
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        out
    }
}

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
    // 探针（**默认不跑**，仅 `--register … --probe-tis` 时启用）：在 app 进程内尝试
    // 注册/启用输入源，用于继续排查 macOS 的输入法授权/菜单过滤。见
    // verba-macos-tis-in-process-probe 线程的结论：GUI 进程里调 TIS 会让选中项
    // 不再被弹回，但菜单仍不列、IMK 仍不拉起进程——需要下一轮继续查。
    let probe_enabled = std::env::args().any(|a| a == "--probe-tis");
    if probe_enabled {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(app) = exe
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
            {
                let report = tis_probe::run(app);
                for line in report.lines() {
                    write_register_log(log_path, line);
                }
                eprintln!("{report}");
            }
        }
    }
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
