//! AaudioOutput — Android AAudio 原生音频输出实现
//!
//! 使用 Android NDK 的 AAudio C API 实现低延迟音频输出。
//! 仅在 `target_os = "android"` 上编译；其他平台此模块不可用。
//!
//! 与 `CpalOutput` 的对比：
//! - **延迟**: AAudio 原生支持低延迟模式（AAUDIO_PERFORMANCE_MODE_LOW_LATENCY）
//! - **音频焦点**: 可通过 `setAudioFocus` 管理（Android 8.0+）
//! - **兼容性**: AAudio 需要 Android 8.0 (API 26+)，与 Raptor 的 minSdk=24 一致

#[cfg(target_os = "android")]
mod aaudio_sys {
    //! Raw FFI bindings to Android AAudio C API
    //!
    //! These are defined in `<aaudio/AAudio.h>` in the Android NDK.
    //! Link with `-laaudio`.

    #![allow(non_camel_case_types, non_snake_case, dead_code)]

    use std::os::raw::c_void;

    pub type aaudio_result_t = i32;
    pub type aaudio_stream_state_t = i32;
    pub type aaudio_direction_t = i32;
    pub type aaudio_format_t = i32;
    pub type aaudio_performance_mode_t = i32;
    pub type aaudio_sharing_mode_t = i32;
    pub type aaudio_data_callback_result_t = i32;

    pub const AAUDIO_OK: aaudio_result_t = 0;
    pub const AAUDIO_ERROR_BASE: aaudio_result_t = -900;

    pub const AAUDIO_DIRECTION_OUTPUT: aaudio_direction_t = 0;

    pub const AAUDIO_FORMAT_PCM_I16: aaudio_format_t = 1;
    pub const AAUDIO_FORMAT_PCM_FLOAT: aaudio_format_t = 2;

    pub const AAUDIO_PERFORMANCE_MODE_LOW_LATENCY: aaudio_performance_mode_t = 12;
    pub const AAUDIO_PERFORMANCE_MODE_NONE: aaudio_performance_mode_t = 10;

    pub const AAUDIO_SHARING_MODE_SHARED: aaudio_sharing_mode_t = 0;

    pub const AAUDIO_STREAM_STATE_OPEN: aaudio_stream_state_t = 1;
    pub const AAUDIO_STREAM_STATE_STARTED: aaudio_stream_state_t = 4;
    pub const AAUDIO_STREAM_STATE_PAUSED: aaudio_stream_state_t = 5;
    pub const AAUDIO_STREAM_STATE_STOPPED: aaudio_stream_state_t = 7;
    pub const AAUDIO_STREAM_STATE_CLOSING: aaudio_stream_state_t = 8;
    pub const AAUDIO_STREAM_STATE_CLOSED: aaudio_stream_state_t = 9;

    pub const AAUDIO_CALLBACK_RESULT_CONTINUE: aaudio_data_callback_result_t = 0;
    pub const AAUDIO_CALLBACK_RESULT_STOP: aaudio_data_callback_result_t = 1;

    pub const AAUDIO_UNSPECIFIED: i32 = 0;

    /// AAudio data callback function type
    pub type AAudioStream_dataCallback = Option<
        unsafe extern "C" fn(
            stream: *mut AAudioStreamStruct,
            userData: *mut c_void,
            audioData: *mut c_void,
            numFrames: i32,
        ) -> aaudio_data_callback_result_t,
    >;

    /// AAudio error callback function type
    pub type AAudioStream_errorCallback = Option<
        unsafe extern "C" fn(
            stream: *mut AAudioStreamStruct,
            userData: *mut c_void,
            error: aaudio_result_t,
        ),
    >;

    /// Opaque stream struct
    #[repr(C)]
    pub struct AAudioStreamStruct {
        _opaque: [u8; 0],
    }

    /// Opaque builder struct
    #[repr(C)]
    pub struct AAudioStreamBuilderStruct {
        _opaque: [u8; 0],
    }

    pub type AAudioStream = AAudioStreamStruct;
    pub type AAudioStreamBuilder = AAudioStreamBuilderStruct;

    // FFI function declarations
    extern "C" {
        pub fn AAudio_createStreamBuilder(
            builder: *mut *mut AAudioStreamBuilder,
        ) -> aaudio_result_t;

        pub fn AAudioStreamBuilder_delete(builder: *mut AAudioStreamBuilder);

        pub fn AAudioStreamBuilder_setDirection(
            builder: *mut AAudioStreamBuilder,
            direction: aaudio_direction_t,
        );

        pub fn AAudioStreamBuilder_setSampleRate(
            builder: *mut AAudioStreamBuilder,
            sampleRate: i32,
        );

        pub fn AAudioStreamBuilder_setChannelCount(
            builder: *mut AAudioStreamBuilder,
            channelCount: i32,
        );

        pub fn AAudioStreamBuilder_setSampleFormat(
            builder: *mut AAudioStreamBuilder,
            format: aaudio_format_t,
        );

        pub fn AAudioStreamBuilder_setPerformanceMode(
            builder: *mut AAudioStreamBuilder,
            mode: aaudio_performance_mode_t,
        );

        pub fn AAudioStreamBuilder_setSharingMode(
            builder: *mut AAudioStreamBuilder,
            sharingMode: aaudio_sharing_mode_t,
        );

        pub fn AAudioStreamBuilder_setDataCallback(
            builder: *mut AAudioStreamBuilder,
            callback: AAudioStream_dataCallback,
            userData: *mut c_void,
        );

        pub fn AAudioStreamBuilder_setErrorCallback(
            builder: *mut AAudioStreamBuilder,
            callback: AAudioStream_errorCallback,
            userData: *mut c_void,
        );

        pub fn AAudioStreamBuilder_openStream(
            builder: *mut AAudioStreamBuilder,
            stream: *mut *mut AAudioStream,
        ) -> aaudio_result_t;

        pub fn AAudioStream_requestStart(stream: *mut AAudioStream) -> aaudio_result_t;

        pub fn AAudioStream_requestPause(stream: *mut AAudioStream) -> aaudio_result_t;

        pub fn AAudioStream_requestStop(stream: *mut AAudioStream) -> aaudio_result_t;

        pub fn AAudioStream_close(stream: *mut AAudioStream) -> aaudio_result_t;

        pub fn AAudioStream_getState(stream: *mut AAudioStream) -> aaudio_stream_state_t;

        pub fn AAudioStream_getSampleRate(stream: *mut AAudioStream) -> i32;

        pub fn AAudioStream_getChannelCount(stream: *mut AAudioStream) -> i32;

        pub fn AAudioStream_getFramesPerBurst(stream: *mut AAudioStream) -> i32;

        pub fn AAudioStream_write(
            stream: *mut AAudioStream,
            buffer: *const c_void,
            numFrames: i32,
            timeoutNanoseconds: i64,
        ) -> i32; // returns frames written or negative error
    }
}

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;
use raptor_core::{RaptorError, Result};
use raptor_ffmpeg::AudioFrame;

use crate::cpal_output::AudioOutput;

/// AAudio ring buffer 容量上限：约 1 秒的立体声 48kHz 采样
const MAX_BUFFER_SAMPLES: usize = 48000 * 2;

/// AAudio 音频输出实现（仅 Android）
///
/// 使用 AAudio C API 的 push 模式（`AAudioStream_write`）输出音频。
/// 与 `CpalOutput` 的 pull 模式（回调）不同，push 模式由调用方主动写入数据。
///
/// 优势：
/// - 低延迟（`AAUDIO_PERFORMANCE_MODE_LOW_LATENCY`）
/// - 原生音频焦点管理
/// - 无需 cpal 中间层
#[cfg(target_os = "android")]
pub struct AaudioOutput {
    stream: Option<*mut aaudio_sys::AAudioStream>,
    buffer: Arc<Mutex<VecDeque<f32>>>,
    volume: Arc<std::sync::atomic::AtomicU32>,
    sample_rate: u32,
    device_rate: u32,
    channels: u32,
}

// SAFETY: AaudioOutput is only used from a dedicated audio thread.
// The AAudioStream pointer is not accessed concurrently.
#[cfg(target_os = "android")]
unsafe impl Send for AaudioOutput {}

#[cfg(target_os = "android")]
impl AaudioOutput {
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
}

#[cfg(target_os = "android")]
impl Default for AaudioOutput {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "android")]
impl AudioOutput for AaudioOutput {
    fn init(&mut self, sample_rate: u32, channels: u32) -> Result<()> {
        self.sample_rate = sample_rate;
        self.channels = channels;
        self.device_rate = sample_rate; // AAudio uses requested rate

        tracing::info!("AaudioOutput::init: {}Hz {}ch", sample_rate, channels);

        unsafe {
            let mut builder: *mut aaudio_sys::AAudioStreamBuilder = std::ptr::null_mut();
            let result = aaudio_sys::AAudio_createStreamBuilder(&mut builder);
            if result != aaudio_sys::AAUDIO_OK {
                return Err(RaptorError::Audio(format!(
                    "AAudio_createStreamBuilder failed: {}",
                    result
                )));
            }

            aaudio_sys::AAudioStreamBuilder_setDirection(
                builder,
                aaudio_sys::AAUDIO_DIRECTION_OUTPUT,
            );
            aaudio_sys::AAudioStreamBuilder_setSampleRate(builder, sample_rate as i32);
            aaudio_sys::AAudioStreamBuilder_setChannelCount(builder, channels as i32);
            aaudio_sys::AAudioStreamBuilder_setSampleFormat(
                builder,
                aaudio_sys::AAUDIO_FORMAT_PCM_FLOAT,
            );
            aaudio_sys::AAudioStreamBuilder_setPerformanceMode(
                builder,
                aaudio_sys::AAUDIO_PERFORMANCE_MODE_LOW_LATENCY,
            );
            aaudio_sys::AAudioStreamBuilder_setSharingMode(
                builder,
                aaudio_sys::AAUDIO_SHARING_MODE_SHARED,
            );

            let mut stream: *mut aaudio_sys::AAudioStream = std::ptr::null_mut();
            let result = aaudio_sys::AAudioStreamBuilder_openStream(builder, &mut stream);

            // 清理 builder（无论成功失败）
            aaudio_sys::AAudioStreamBuilder_delete(builder);

            if result != aaudio_sys::AAUDIO_OK || stream.is_null() {
                return Err(RaptorError::Audio(format!(
                    "AAudioStreamBuilder_openStream failed: {}",
                    result
                )));
            }

            // 获取实际采样率（可能与请求的不同）
            let actual_rate = aaudio_sys::AAudioStream_getSampleRate(stream);
            if actual_rate > 0 {
                self.device_rate = actual_rate as u32;
                tracing::info!(
                    "AaudioOutput: actual sample rate = {}Hz (requested {}Hz)",
                    actual_rate,
                    sample_rate
                );
            }

            // 启动流
            let result = aaudio_sys::AAudioStream_requestStart(stream);
            if result != aaudio_sys::AAUDIO_OK {
                aaudio_sys::AAudioStream_close(stream);
                return Err(RaptorError::Audio(format!(
                    "AAudioStream_requestStart failed: {}",
                    result
                )));
            }

            self.stream = Some(stream);
            tracing::info!("AaudioOutput: stream started");
        }

        Ok(())
    }

    fn write(&mut self, frame: &AudioFrame) -> Result<()> {
        let mut buf = self.buffer.lock();

        // 推入采样到 ring buffer
        if self.sample_rate == self.device_rate || self.device_rate == 0 {
            buf.extend(frame.samples.iter());
        } else {
            // 线性重采样
            let channels = self.channels as usize;
            let ratio = self.device_rate as f64 / self.sample_rate as f64;
            let src_frames = frame.samples.len() / channels;
            let out_frames = (src_frames as f64 * ratio).ceil() as usize;

            for i in 0..out_frames {
                let src_pos = i as f64 / ratio;
                let src_idx = src_pos.floor() as usize;
                let frac = (src_pos - src_idx as f64) as f32;

                for ch in 0..channels {
                    let s0 = frame
                        .samples
                        .get(src_idx * channels + ch)
                        .copied()
                        .unwrap_or(0.0);
                    let s1 = frame
                        .samples
                        .get((src_idx + 1) * channels + ch)
                        .copied()
                        .unwrap_or(0.0);
                    buf.push_back(s0 + (s1 - s0) * frac);
                }
            }
        }

        // Ring buffer 上限保护
        while buf.len() > MAX_BUFFER_SAMPLES {
            buf.pop_front();
        }

        // 从 buffer 写入 AAudio stream：
        // 带超时写入并只消费成功写入的部分，未写完的采样留在 buffer 等下轮，
        // 避免旧实现"全量 pop + timeout=0 非阻塞写"造成的数据丢弃与周期性静音
        if let Some(stream) = self.stream {
            let channels = self.channels as usize;
            let vol = f32::from_bits(self.volume.load(std::sync::atomic::Ordering::Relaxed));

            let burst =
                unsafe { aaudio_sys::AAudioStream_getFramesPerBurst(stream) }.max(1) as usize;
            let max_frames = burst * 4;
            let writable_frames = (buf.len() / channels).min(max_frames);

            if writable_frames > 0 {
                let sample_count = writable_frames * channels;
                let mut scratch: Vec<f32> = Vec::with_capacity(sample_count);
                for s in buf.iter().take(sample_count) {
                    scratch.push(*s * vol);
                }

                // 100ms 超时：阻塞至写完或超时，partial write 由残余保留兜底
                let written = unsafe {
                    aaudio_sys::AAudioStream_write(
                        stream,
                        scratch.as_ptr() as *const std::ffi::c_void,
                        writable_frames as i32,
                        100_000_000,
                    )
                };
                if written > 0 {
                    for _ in 0..(written as usize).min(writable_frames) * channels {
                        buf.pop_front();
                    }
                } else if written < 0 {
                    tracing::warn!("AaudioOutput: AAudioStream_write error: {written}");
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

    /// 本地 ring buffer 装得下这一帧（重采样后的设备采样数）吗
    fn accepts(&self, frame: &AudioFrame) -> bool {
        let channels = self.channels.max(1) as usize;
        let src_frames = frame.samples.len() / channels;
        let device_samples = if self.sample_rate > 0 && self.device_rate > self.sample_rate {
            (src_frames as f64 * (self.device_rate as f64 / self.sample_rate as f64)).ceil()
                as usize
                * channels
        } else {
            src_frames * channels
        };
        self.buffer.lock().len() + device_samples <= MAX_BUFFER_SAMPLES
    }

    /// 冻结设备消费：不暂停的话缓冲里的旧采样会在"已暂停"期间继续放完
    fn pause(&mut self) -> Result<()> {
        if let Some(stream) = self.stream {
            let result = unsafe { aaudio_sys::AAudioStream_requestPause(stream) };
            if result != aaudio_sys::AAUDIO_OK {
                return Err(RaptorError::Audio(format!(
                    "AAudioStream_requestPause failed: {result}"
                )));
            }
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if let Some(stream) = self.stream {
            let result = unsafe { aaudio_sys::AAudioStream_requestStart(stream) };
            if result != aaudio_sys::AAUDIO_OK {
                return Err(RaptorError::Audio(format!(
                    "AAudioStream_requestStart failed: {result}"
                )));
            }
        }
        Ok(())
    }

    /// 丢弃未播采样（seek / flush 语义）
    ///
    /// AAudio 没有独立的 flush 入口：`requestStop` 会同步停流并丢弃设备
    /// 内部队列里尚未播放的采样，随后 `requestStart` 复位供继续使用。
    /// 本地 ring buffer 同样要清空——两层里存的都是旧位置的采样。
    fn flush(&mut self) {
        self.buffer.lock().clear();
        if let Some(stream) = self.stream {
            unsafe {
                let stop = aaudio_sys::AAudioStream_requestStop(stream);
                if stop != aaudio_sys::AAUDIO_OK {
                    tracing::warn!("AaudioOutput: flush requestStop failed: {stop}");
                }
                let start = aaudio_sys::AAudioStream_requestStart(stream);
                if start != aaudio_sys::AAUDIO_OK {
                    tracing::warn!("AaudioOutput: flush requestStart failed: {start}");
                }
            }
        }
    }

    /// EOF 排空判定：本地缓冲是否还有未写入设备的采样
    fn has_pending_audio(&self) -> bool {
        !self.buffer.lock().is_empty()
    }
}

#[cfg(target_os = "android")]
impl Drop for AaudioOutput {
    fn drop(&mut self) {
        tracing::info!("AaudioOutput::drop");
        if let Some(stream) = self.stream.take() {
            unsafe {
                aaudio_sys::AAudioStream_requestStop(stream);
                aaudio_sys::AAudioStream_close(stream);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "android")]
    use super::*;

    #[test]
    #[cfg(target_os = "android")]
    fn test_aaudio_output_new() {
        let output = AaudioOutput::new();
        assert!((output.volume() - 1.0).abs() < 0.001);
        assert_eq!(output.sample_rate, 0);
        assert!(output.stream.is_none());
    }

    #[test]
    #[cfg(target_os = "android")]
    fn test_aaudio_set_volume() {
        let mut output = AaudioOutput::new();
        output.set_volume(0.5);
        assert!((output.volume() - 0.5).abs() < 0.001);
        output.set_volume(1.5);
        assert!((output.volume() - 1.0).abs() < 0.001);
    }
}
