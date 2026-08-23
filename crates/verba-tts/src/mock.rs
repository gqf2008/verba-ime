//! Mock TTS provider：确定性 WAV 合成（无需网络/模型，供开发与验收）。

use verba_ai::{TtsAudio, TtsProvider};

use crate::{wav, TtsError};

/// 本地 mock 合成器：按文本长度生成固定时长 440Hz 提示音 WAV。
#[derive(Debug, Clone)]
pub struct MockTts {
    /// 采样率（Hz）。
    pub sample_rate: u32,
    /// 每个字符的语音时长（秒）。
    pub secs_per_char: f32,
}

impl MockTts {
    pub fn new() -> Self {
        Self {
            sample_rate: 16_000,
            secs_per_char: 0.25,
        }
    }
}

impl Default for MockTts {
    fn default() -> Self {
        Self::new()
    }
}

impl TtsProvider for MockTts {
    type Error = TtsError;

    async fn synthesize(&self, text: &str) -> Result<TtsAudio, TtsError> {
        let chars = text.chars().count().max(1);
        let duration = 0.2 + self.secs_per_char * chars as f32;
        let n = (duration * self.sample_rate as f32) as usize;
        let mut samples = Vec::with_capacity(n);
        let mut phase = 0.0f32;
        for _ in 0..n {
            let v = phase.sin() * 0.2;
            samples.push((v * i16::MAX as f32) as i16);
            phase += std::f32::consts::TAU * 440.0 / self.sample_rate as f32;
        }
        Ok(TtsAudio {
            format: "wav",
            bytes: wav::pcm16_mono_wav(&samples, self.sample_rate),
        })
    }
}
