//! IPC 错误类型。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("Protobuf 解码错误: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("Protobuf 编码错误: {0}")]
    Encode(#[from] prost::EncodeError),
    #[error("请求超时")]
    Timeout,
    #[error("连接已关闭")]
    ConnectionClosed,
    #[error("协议错误: {0}")]
    Protocol(String),
    #[error("服务端返回错误: code={code} message={message}")]
    Server { code: i32, message: String },
}

/// 读取/写入超时（秒）。
pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
