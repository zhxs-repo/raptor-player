use std::collections::VecDeque;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
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
    volume: Arc<std::sync::atomic::AtomicU32>, // 用 atomic bits 存 f32
    sample_rate: u32,      // 源采样率（FFmpeg 输出）
    device_rate: u32,      // 设备采样率（cpal 实际播放）
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
            volume: Arc::new(std::sync::atomic::AtomicU32::new(1.0f32.to_bits())),
            sample_rate: 0,
            device_rate: 0,
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

        // 查询设备默认输出配置，使用设备支持的格式
        let supported_config = device
            .default_output_config()
            .map_err(|e| RaptorError::Audio(format!("get default output config: {e}")))?;

        let device_sample_rate = supported_config.sample_rate().0;
        let device_channels = supported_config.channels();
        let device_format = supported_config.sample_format();

        tracing::info!(
            "CpalOutput::init: source={}Hz {}ch, device={}Hz {}ch {:?}, device_name={}",
            sample_rate,
            channels,
            device_sample_rate,
            device_channels,
            device_format,
            device.name().unwrap_or_default()
        );

        self.device_rate = device_sample_rate;

        let config: cpal::StreamConfig = supported_config.into();
        let buffer = self.buffer.clone();
        let volume = self.volume.clone();

        // 根据设备支持的采样格式构建流
        let stream = match device_format {
            SampleFormat::F32 => device.build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let vol = f32::from_bits(volume.load(std::sync::atomic::Ordering::Relaxed));
                    let mut buf = buffer.lock();
                    for sample in data.iter_mut() {
                        *sample = buf.pop_front().unwrap_or(0.0) * vol;
                    }
                },
                move |err| tracing::error!("cpal output stream error: {}", err),
                None,
            ),
            SampleFormat::I16 => device.build_output_stream(
                &config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    let vol = f32::from_bits(volume.load(std::sync::atomic::Ordering::Relaxed));
                    let mut buf = buffer.lock();
                    for sample in data.iter_mut() {
                        let s = buf.pop_front().unwrap_or(0.0) * vol;
                        *sample = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                    }
                },
                move |err| tracing::error!("cpal output stream error: {}", err),
                None,
            ),
            _ => {
                return Err(RaptorError::Audio(format!(
                    "unsupported device sample format: {:?}",
                    device_format
                )));
            }
        }
        .map_err(|e| RaptorError::Audio(format!("build output stream: {e}")))?;

        stream
            .play()
            .map_err(|e| RaptorError::Audio(format!("play stream: {e}")))?;

        self.stream = Some(stream);
        Ok(())
    }

    fn write(&mut self, frame: &AudioFrame) -> Result<()> {
        let mut buf = self.buffer.lock();
        if self.sample_rate == self.device_rate || self.device_rate == 0 {
            // 采样率匹配，直接推入
            buf.extend(frame.samples.iter());
        } else {
            // 线性重采样：source_rate → device_rate
            let channels = self.channels as usize;
            let ratio = self.device_rate as f64 / self.sample_rate as f64;
            let src_frames = frame.samples.len() / channels;
            let out_frames = (src_frames as f64 * ratio).ceil() as usize;

            for i in 0..out_frames {
                let src_pos = i as f64 / ratio;
                let src_idx = src_pos.floor() as usize;
                let frac = (src_pos - src_idx as f64) as f32;

                for ch in 0..channels {
                    let s0 = frame.samples.get(src_idx * channels + ch).copied().unwrap_or(0.0);
                    let s1 = frame
                        .samples
                        .get((src_idx + 1) * channels + ch)
                        .copied()
                        .unwrap_or(0.0);
                    buf.push_back(s0 + (s1 - s0) * frac);
                }
            }
        }
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) {
        let v = volume.clamp(0.0, 1.0);
        self.volume
            .store(v.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }

    fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(std::sync::atomic::Ordering::Relaxed))
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
        assert!((output.volume() - 1.0).abs() < 0.001);
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
