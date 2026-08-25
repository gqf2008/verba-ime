//! tokio 异步 IPC 服务端（daemon 使用）。
//!
//! 注意：Windows 命名管道上不要用 `split()` 拆分读写（易导致连接被提前关闭），
//! 这里统一用 `&Stream` 并发读写（与 interprocess 官方示例一致）。

use std::sync::Arc;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::tokio::Stream as TokioStream;
use interprocess::local_socket::traits::tokio::Listener as _;
use interprocess::local_socket::{GenericFilePath, ListenerOptions};use prost::Message;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use verba_protos::{Request, Response, StreamEvent};

use crate::codec::{encode_frame, read_frame_async};
use crate::error::IpcError;

/// 连接写通道：所有响应与事件都经它顺序写出。
#[derive(Clone, Debug)]
pub struct Outbound {
    tx: mpsc::Sender<Vec<u8>>,
}

impl Outbound {
    /// 发送响应帧。
    pub async fn response(&self, resp: &Response) -> Result<(), IpcError> {
        let mut buf = Vec::new();
        resp.encode(&mut buf)?;
        self.send_frame(&buf).await
    }

    /// 发送流式事件帧。
    pub async fn event(&self, evt: &StreamEvent) -> Result<(), IpcError> {
        let mut buf = Vec::new();
        evt.encode(&mut buf)?;
        self.send_frame(&buf).await
    }

    async fn send_frame(&self, payload: &[u8]) -> Result<(), IpcError> {
        let frame = encode_frame(payload)?;
        self.tx
            .send(frame)
            .await
            .map_err(|_| IpcError::ConnectionClosed)
    }
}

/// 请求处理器：daemon 实现该 trait 处理每个请求，可向 `Outbound` 推送响应与事件。
#[async_trait::async_trait]
pub trait RequestHandler: Send + Sync + 'static {
    async fn handle(&self, req: Request, out: Outbound);
}

/// 启动服务端，持续接受连接。
pub async fn serve(name: &str, handler: Arc<dyn RequestHandler>) -> Result<(), IpcError> {
    log::info!("IPC 服务启动: {name}");
    // GenericFilePath：Unix 原样作为 UDS 路径（daemon 侧目录已 chmod 0700）；
    // Windows 把 `\\.\pipe\` 前缀映射为命名管道（per-user 名称隔离）。
    let name = name.to_fs_name::<GenericFilePath>()?;
    let listener = ListenerOptions::new().name(name).create_tokio()?;
    loop {
        match listener.accept().await {
            Ok(stream) => {
                let handler = Arc::clone(&handler);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, handler).await {
                        log::debug!("连接处理结束: {e}");
                    }
                });
            }
            Err(e) => {
                log::warn!("accept 失败: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}

async fn handle_connection(
    stream: TokioStream,
    handler: Arc<dyn RequestHandler>,
) -> Result<(), IpcError> {
    let stream = Arc::new(stream);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(256);

    let writer_stream = Arc::clone(&stream);
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            let mut conn = &*writer_stream;
            if conn.write_all(&frame).await.is_err() {
                break;
            }
        }
    });

    loop {
        let mut conn = &*stream;
        let payload = match read_frame_async(&mut conn).await {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(IpcError::Io(e)),
        };
        let req = match Request::decode(payload.as_slice()) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("请求解码失败: {e}");
                break;
            }
        };
        let out = Outbound { tx: out_tx.clone() };
        let handler = Arc::clone(&handler);
        tokio::spawn(async move {
            handler.handle(req, out).await;
        });
    }

    drop(out_tx);
    let _ = writer.await;
    Ok(())
}
