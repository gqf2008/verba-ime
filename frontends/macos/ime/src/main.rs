//! `verba-mac`：macOS 输入法可执行入口。
//!
//! 作为 IMK 输入法 `.app` 的 `CFBundleExecutable`，注册 `IMKServer` 并进入
//! AppKit 主循环；`CFBundleIdentifier` / `InputMethodServerControllerClass`
//! 等元数据由 `app/Info.plist` 提供。

fn main() {
    #[cfg(target_os = "macos")]
    {
        verba_ime_macos::run_imk_server();
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("verba-mac 仅支持 macOS");
        std::process::exit(1);
    }
}
