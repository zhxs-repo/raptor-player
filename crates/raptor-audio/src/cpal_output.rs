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
    /// 用于通知 cpal 回调停止读取数据
    is_stopped: Arc<std::sync::atomic::AtomicBool>,
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
            is_stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        let is_stopped = self.is_stopped.clone();

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                    // 如果已停止，直接填充静音
                    if is_stopped.load(std::sync::atomic::Ordering::Acquire) {
                        data.fill(0.0);
                        return;
                    }
                    
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
        
        // 第一步：设置停止标志，通知 cpal 回调不再访问 buffer
        // 使用 SeqCst 确保在所有平台上都有最强的顺序保证
        self.is_stopped.store(true, std::sync::atomic::Ordering::SeqCst);
        
        // 第二步：暂停并丢弃流
        if let Some(stream) = self.stream.take() {
            // 暂停流，阻止新的回调
            if let Err(e) = stream.pause() {
                tracing::warn!("Failed to pause audio stream: {}", e);
            }
            
            // 关键：等待足够的时间确保所有正在执行的回调完成
            // Windows 上 cpal 使用 WASAPI，回调可能在 pause() 后仍会执行一次
            // 等待 100ms 确保所有回调完成（典型音频缓冲区大小为 10-50ms）
            std::thread::sleep(std::time::Duration::from_millis(100));
            
            // stream 在此处被 drop，WASAPI 流被销毁
            drop(stream);
            
            // 再次短暂等待，确保 WASAPI 完全释放资源
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        
        // 第三步：清空 buffer，确保没有残留数据
        // 此时已无回调访问 buffer，可以安全清空
        self.buffer.lock().clear();
        
        tracing::debug!("CpalOutput::drop completed");
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
        assert!(!output.is_stopped.load(std::sync::atomic::Ordering::Acquire));
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
}
