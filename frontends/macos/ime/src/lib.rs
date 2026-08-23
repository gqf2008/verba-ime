//! Verba macOS IMK 前端（Rust 引擎壳）。
//!
//! 真实 IMK 的按键捕获与文本插入由 Swift/ObjC 薄壳完成；本 crate 提供 Rust 侧的
//! daemon 连接与状态机复用（verba-ipc / verba-core），供 Swift IMK 通过 FFI / 子进程调用。
//! 为便于 CI 编译验证，当前仅依赖跨平台 crates，不直接依赖 macOS 框架。

use verba_ipc::VerbaClient;

/// macOS 前端句柄：持有到 daemon 的连接。
pub struct MacIme {
    client: VerbaClient,
}

impl MacIme {
    /// 连接 daemon（Unix socket / 命名管道，跨平台复用 verba-ipc）。
    pub fn connect() -> std::io::Result<Self> {
        let client =
            VerbaClient::connect().map_err(|e| std::io::Error::other(e.to_string()))?;
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

/// macOS 平台特有初始化（真实 IMK 注册）。非 macOS 为 no-op。
#[cfg(target_os = "macos")]
pub fn init_imk() -> Result<(), String> {
    // TODO(ci): 在 macOS 真机/CI 上接入 InputMethodKit（NSInputMethodController 等）。
    // 当前仅占位，保证 crate 可在 macOS 编译。
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn init_imk() -> Result<(), String> {
    Ok(())
}
