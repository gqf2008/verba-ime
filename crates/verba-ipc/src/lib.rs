//! Verba IPC：帧编解码与 local_socket 传输（Windows 命名管道 / Unix Domain Socket）。
//!
//! - [`codec`]：u32 LE 长度前缀分帧。
//! - [`client`]：阻塞式客户端（供 TSF 前端与 CLI 使用）。
//! - [`server`]：tokio 异步服务端（供 daemon 使用）。

#![forbid(unsafe_code)]

pub mod client;
pub mod codec;
pub mod error;
pub mod server;

pub use client::{ConnectWait, VerbaClient, DEFAULT_SOCKET_NAME};
pub use error::IpcError;
