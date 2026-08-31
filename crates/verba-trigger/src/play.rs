//! 音频播放：rodio 解码并播放（MP3 / WAV）。
//!
//! 阻塞式：播放完成或出错才返回，便于 CLI 与后续 TSF 后台线程使用。

use std::io::Cursor;

use rodio::{Decoder, OutputStream, Sink};

use crate::TriggerError;

/// 播放音频字节，阻塞至播放结束（或出错）。
pub fn play_audio(bytes: &[u8]) -> Result<(), TriggerError> {
    if bytes.is_empty() {
        return Err(TriggerError::Play("音频为空".into()));
    }
    let (_stream, handle) = OutputStream::try_default()
        .map_err(|e| TriggerError::Play(format!("打开音频设备失败: {e}")))?;
    let sink =
        Sink::try_new(&handle).map_err(|e| TriggerError::Play(format!("创建播放器失败: {e}")))?;
    let source = Decoder::new(Cursor::new(bytes.to_vec()))
        .map_err(|e| TriggerError::Play(format!("解码音频失败: {e}")))?;
    sink.append(source);
    sink.sleep_until_end();
    Ok(())
}
