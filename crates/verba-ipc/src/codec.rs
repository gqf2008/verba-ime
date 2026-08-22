//! 帧编解码：`u32 LE 长度前缀 + payload`。

use std::io::{self, Read};

use tokio::io::{AsyncRead, AsyncReadExt};

/// 单帧最大字节数（图像等大载荷预留 64 MiB）。
pub const MAX_FRAME_SIZE: usize = 64 * 1024 * 1024;

/// 编码一帧。
pub fn encode_frame(payload: &[u8]) -> io::Result<Vec<u8>> {
    if payload.len() > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("帧过大: {} 字节", payload.len()),
        ));
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// 从阻塞 reader 读取一帧；连接关闭（EOF）时返回 UnexpectedEof。
pub fn read_frame<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("帧过大: {len} 字节"),
        ));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

/// 从 tokio reader 读取一帧；连接关闭（EOF）时返回 UnexpectedEof。
pub async fn read_frame_async<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("帧过大: {len} 字节"),
        ));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}
