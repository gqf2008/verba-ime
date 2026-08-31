//! 麦克风录音：cpal 采集 → WAV 字节。
//!
//! 使用设备原生采样率 / 声道数；f32/i16/u16 样本统一转 i16 PCM（16bit）。
//! 后续 whisper.cpp 需要 16kHz 单声道时，在 ASR provider 侧做重采样，
//! 这里保留原始信息不丢。

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

use crate::TriggerError;

/// 录制指定秒数的麦克风输入，返回 WAV 字节。
pub fn record_seconds(seconds: f32) -> Result<Vec<u8>, TriggerError> {
    if seconds <= 0.0 {
        return Err(TriggerError::Record("时长必须大于 0".into()));
    }
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| TriggerError::Record("无默认输入设备".into()))?;
    let supported = device
        .default_input_config()
        .map_err(|e| TriggerError::Record(format!("取默认输入配置失败: {e}")))?;
    let config: StreamConfig = supported.clone().into();
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;

    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build_stream::<f32>(&device, &config, Arc::clone(&samples)),
        SampleFormat::I16 => build_stream::<i16>(&device, &config, Arc::clone(&samples)),
        SampleFormat::U16 => build_stream::<u16>(&device, &config, Arc::clone(&samples)),
        other => {
            return Err(TriggerError::Record(format!("不支持的采样格式: {other:?}")));
        }
    }
    .map_err(|e| TriggerError::Record(format!("建立录音流失败: {e}")))?;

    stream
        .play()
        .map_err(|e| TriggerError::Record(format!("开始录音失败: {e}")))?;
    std::thread::sleep(std::time::Duration::from_secs_f32(seconds));
    drop(stream);

    let data = samples.lock().unwrap().clone();
    if data.is_empty() {
        return Err(TriggerError::Record("未采集到任何音频样本".into()));
    }
    Ok(encode_wav(sample_rate, channels, &data))
}

/// 以目标样本类型建立输入流，样本统一转 f32 收集。
fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    sink: Arc<Mutex<Vec<f32>>>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample + FromSample<f32>,
    f32: FromSample<T>,
{
    let err_fn = |e| log::error!("录音流错误: {e}");
    device.build_input_stream(
        config,
        move |data: &[T], _| {
            let mut guard = sink.lock().unwrap();
            for s in data.iter() {
                guard.push(s.to_sample::<f32>());
            }
        },
        err_fn,
        None,
    )
}

/// f32 样本 → 16bit PCM WAV（RIFF + fmt + data）。
fn encode_wav(sample_rate: u32, channels: usize, samples: &[f32]) -> Vec<u8> {
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32768.0) as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    let data_len = pcm.len() as u32;
    let byte_rate = sample_rate * (channels as u32) * 2;
    let block_align = (channels as u16) * 2;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36u32 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(channels as u16).to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&pcm);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_layout() {
        let wav = encode_wav(16000, 1, &[0.0, 0.5, -0.5, 1.0]);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 8);
        // 16bit 单声道 16kHz
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16000);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        assert_eq!(wav.len(), 44 + 8);
    }

    #[test]
    fn wav_clamps_and_roundtrips() {
        let wav = encode_wav(8000, 2, &[2.0, -2.0]);
        assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 32767);
        assert_eq!(i16::from_le_bytes(wav[46..48].try_into().unwrap()), -32768);
    }
}
