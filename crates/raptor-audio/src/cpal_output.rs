use std::collections::VecDeque;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use raptor_core::{RaptorError, Result};
use raptor_ffmpeg::AudioFrame;

/// AudioOutput trait — 音频输出抽象
pub trait AudioOutput: Send {
    /// 初始化音频输出
    fn init(&mut self, sample_rate: u32, channels: u32) -> Result<()>;

    /// 写入音频帧
    fn write(&mut self, frame: &AudioFrame) -> Result<()>;

    /// 设置音量（0.0 ~ 1.0）
    fn set_volume(&mut self, volume: f32);

    /// 获取当前音量
    fn volume(&self) -> f32;
}

/// cpal 音频输出实现
///
/// 使用共享 `VecDeque<f32>` 作为 ring buffer，
/// audio_output_loop 写入，cpal 回调读取。
pub struct CpalOutput {
    stream: Option<cpal::Stream>,
    buffer: Arc<Mutex<VecDeque<f32>>>,
    volume: f32,
    sample_rate: u32,
    channels: u32,
}

// SAFETY: CpalOutput is only used from a dedicated audio thread.
// The cpal::Stream is not accessed concurrently.
unsafe impl Send for CpalOutput {}

impl CpalOutput {
    pub fn new() -> Self {
        Self {
            stream: None,
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            volume: 1.0,
            sample_rate: 0,
            channels: 0,
        }
    }

    /// 获取共享 buffer 引用（供 audio_output_loop 直接写入）
    pub fn buffer(&self) -> Arc<Mutex<VecDeque<f32>>> {
        self.buffer.clone()
    }
}

impl Default for CpalOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioOutput for CpalOutput {
    fn init(&mut self, sample_rate: u32, channels: u32) -> Result<()> {
        self.sample_rate = sample_rate;
        self.channels = channels;

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| RaptorError::Audio("no default audio output device".into()))?;

        let config = cpal::StreamConfig {
            channels: channels as u16,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        tracing::info!(
            "CpalOutput::init: {}Hz, {}ch, device={}",
            sample_rate,
            channels,
            device.name().unwrap_or_default()
        );

        let buffer = self.buffer.clone();
        let volume = self.volume;

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                    let mut buf = buffer.lock();
                    for sample in data.iter_mut() {
                        if let Some(s) = buf.pop_front() {
                            *sample = s * volume;
                        } else {
                            *sample = 0.0;
                        }
                    }
                },
                move |err| {
                    tracing::error!("cpal output stream error: {}", err);
                },
                None,
            )
            .map_err(|e| RaptorError::Audio(format!("build output stream: {e}")))?;

        stream
            .play()
            .map_err(|e| RaptorError::Audio(format!("play stream: {e}")))?;

        self.stream = Some(stream);
        Ok(())
    }

    fn write(&mut self, frame: &AudioFrame) -> Result<()> {
        let mut buf = self.buffer.lock();
        buf.extend(frame.samples.iter());
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    fn volume(&self) -> f32 {
        self.volume
    }
}

impl Drop for CpalOutput {
    fn drop(&mut self) {
        tracing::info!("CpalOutput::drop");
        if let Some(stream) = self.stream.take() {
            let _ = stream.pause();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpal_output_new() {
        let output = CpalOutput::new();
        assert_eq!(output.volume, 1.0);
        assert_eq!(output.sample_rate, 0);
        assert!(output.stream.is_none());
    }

    #[test]
    fn test_set_volume() {
        let mut output = CpalOutput::new();
        output.set_volume(0.5);
        assert!((output.volume() - 0.5).abs() < 0.001);
        output.set_volume(1.5);
        assert!((output.volume() - 1.0).abs() < 0.001);
        output.set_volume(-0.1);
        assert!((output.volume() - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_apply_volume() {
        let mut samples = vec![0.5f32, 1.0, -0.5];
        crate::apply_volume(&mut samples, 0.5);
        assert!((samples[0] - 0.25).abs() < 0.001);
        assert!((samples[1] - 0.5).abs() < 0.001);
        assert!((samples[2] - (-0.25)).abs() < 0.001);
    }
}
