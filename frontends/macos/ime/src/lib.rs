//! Verba macOS IMK 前端（全 Rust）。
//!
//! - [`MacIme`]：连接 daemon 的句柄（健康检查 / C ABI 桥复用）。
//! - [`ipc`]：daemon 定位与拉起（与 Windows 前端 `ipc.rs` 对齐）。
//! - [`imk`]：IMK 输入控制器（objc2 + objc2-input-method-kit），
//!   拼音组合 / 候选 / 上屏 + LLM 流式（`//` AI 模式）；引擎全在 verba-core。

mod ffi;
pub mod ipc;

use verba_ipc::VerbaClient;

/// macOS 前端句柄：持有到 daemon 的连接。
pub struct MacIme {
    client: VerbaClient,
}

impl MacIme {
    /// 连接 daemon（Unix socket / 命名管道，跨平台复用 verba-ipc）。
    pub fn connect() -> std::io::Result<Self> {
        let client = VerbaClient::connect().map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(Self { client })
    }

    /// 健康检查。
    pub fn ping(&mut self) -> std::io::Result<String> {
        self.client
            .ping()
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// 请求提交文本（C ABI 桥侧仅记录；真正上屏由 IMK 控制器经 client 完成）。
    pub fn request_commit(&self, text: &str) {
        log::info!("[mac-imk] 请求提交文本: chars={}", text.chars().count());
    }
}

/// macOS 下加载 IMK 输入控制器子类。
#[cfg(target_os = "macos")]
pub mod imk;

/// 引导加载 IMK 局：非 macOS 为 no-op（仅供其它平台构建时占位）。
pub fn init_imk() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        imk::register();
    }
    Ok(())
}

/// 启动 IMK 服务并进入 AppKit 主循环（仅 macOS，供 `verba-mac` 可执行入口调用）。
#[cfg(target_os = "macos")]
pub fn run_imk_server() -> ! {
    imk::run_server()
}
