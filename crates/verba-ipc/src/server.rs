//! tokio 异步 IPC 服务端（daemon 使用）。
//!
//! 注意：Windows 命名管道上不要用 `split()` 拆分读写（易导致连接被提前关闭），
//! 这里统一用 `&Stream` 并发读写（与 interprocess 官方示例一致）。

use std::sync::Arc;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::tokio::Stream as TokioStream;
use interprocess::local_socket::traits::tokio::Listener as _;
use interprocess::local_socket::{GenericFilePath, ListenerOptions};
use prost::Message;
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
///
/// `conn_id`：服务端为每条连接分配的全局唯一标识。daemon 侧以连接为维度的
/// 状态（如取消注册表）必须用 `(conn_id, req_id)` 键控——请求 id 是每连接从
/// 1 自增的，全局键会跨连接互踩（架构审查 P1-1）。
#[async_trait::async_trait]
pub trait RequestHandler: Send + Sync + 'static {
    async fn handle(&self, conn_id: u64, req: Request, out: Outbound);
}

/// 启动服务端，持续接受连接。
pub async fn serve(name: &str, handler: Arc<dyn RequestHandler>) -> Result<(), IpcError> {
    log::info!("IPC 服务启动: {name}");
    // GenericFilePath：Unix 原样作为 UDS 路径（daemon 侧目录已 chmod 0700）；
    // Windows 把 `\\.\pipe\` 前缀映射为命名管道（per-user + token 名称隔离）。
    let ns_name = name.to_fs_name::<GenericFilePath>()?;
    // stale socket 自愈（架构审查 P1-4）：异常退出（Ctrl-C/SIGKILL 不触发 drop
    // 清理）残留文件 → 下次 bind 得 EADDRINUSE。先探测：connect 成功即视为已有
    // daemon（保守拒绝，避免无超时 ping 挂死）；否则仅当残留的是 socket 文件时
    // unlink 再 bind（不误删普通文件）。
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if crate::client::VerbaClient::connect_named(name, crate::ConnectWait::Nonblocking).is_ok()
        {
            return Err(IpcError::Protocol("已有 daemon 实例在运行".into()));
        }
        if std::fs::symlink_metadata(name)
            .map(|m| m.file_type().is_socket())
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(name);
        }
    }
    let listener = ListenerOptions::new().name(ns_name).create_tokio()?;
    // 连接级全局唯一 id（取消表等以连接为维度的状态键控用）
    let next_conn = std::sync::atomic::AtomicU64::new(1);
    loop {
        match listener.accept().await {
            Ok(stream) => {
                let handler = Arc::clone(&handler);
                let conn_id = next_conn.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(conn_id, stream, handler).await {
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
    conn_id: u64,
    stream: TokioStream,
    handler: Arc<dyn RequestHandler>,
) -> Result<(), IpcError> {
    let stream = Arc::new(stream);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(256);
    // 在途请求计数：读循环的空闲超时只回收「真正闲着」的连接——LLM 流式生成
    // 期间客户端发完 llm_start 后只是被动等事件、不再发任何帧，若按纯空闲
    // 判定会在 >60s 的长生成中途掐断连接（客户端只见「连接中断」，v0.1 无此
    // 超时故为回归）。有在途 handler 时跳过本次回收，再续一个超时窗口等待。
    let pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));

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
        // 读帧 idle 超时（架构审查 P2-3）：慢/死客户端挂连接不回收会堆积
        // reader/writer 任务；60s 无请求且无在途流式处理即断开。
        let payload = match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            read_frame_async(&mut conn),
        )
        .await
        {
            Ok(Ok(p)) => p,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Ok(Err(e)) => return Err(IpcError::Io(e)),
            Err(_) => {
                use std::sync::atomic::Ordering;
                if pending.load(Ordering::SeqCst) > 0 {
                    // 有在途流式处理（如长时间 LLM 生成）：不回收，续期再等。
                    continue;
                }
                log::debug!("连接读空闲超时，回收");
                break;
            }
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
        let pending = Arc::clone(&pending);
        tokio::spawn(async move {
            pending.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            handler.handle(conn_id, req, out).await;
            pending.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    }

    drop(out_tx);
    let _ = writer.await;
    Ok(())
}
