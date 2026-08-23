//! Verba macOS IMK 前端（全 Rust）。
//!
//! `MacIme` 连接 daemon；IMK 输入控制器由 `imk`（objc2 + objc2-input-method-kit）子类化定义；
//! `ffi` 提供 C ABI 供其它宿主调用。引擎/状态机全部在 verba-core / daemon，前端只做薄壳。

mod ffi;

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

    /// 请求提交文本（Swift IMK 侧负责真正插入编辑上下文；此处仅记录）。
    pub fn request_commit(&self, text: &str) {
        log::info!("[mac-imk] 请求提交文本: chars={}", text.chars().count());
    }
}

/// macOS 下加载 IMK 输入控制器子类。
#[cfg(target_os = "macos")]
mod imk;

/// 引导加载 IMK 局：非 macOS 为 no-op（仅供其它平台构建时占位）。
pub fn init_imk() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        imk::register();
    }
    Ok(())
}
