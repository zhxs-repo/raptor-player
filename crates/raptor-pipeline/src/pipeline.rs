use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{bounded, Sender};
use parking_lot::Mutex;
use raptor_audio::{AudioOutput, CpalOutput};
use raptor_core::{AudioInfo, RaptorEvent, VideoInfo};
use raptor_ffmpeg::{
    AudioDecoder, Demuxer, FfmpegAudioDecoder, FfmpegDemuxer, FfmpegVideoDecoder, VideoDecoder,
};
use raptor_render::VideoOutput;

use crate::avsync::AVSync;
use crate::decode::{audio_decode_loop, video_decode_loop};
use crate::demux::demux_loop;
use crate::output::{audio_output_loop, render_loop};

/// Seek 请求
pub struct SeekRequest {
    pub target: f64,
    pub from: f64,
}

/// Pipeline 句柄集合 — FFI 层持有的额外引用
pub struct PipelineHandles {
    #[allow(dead_code)]
    _placeholder: (),
}

/// Pipeline — 管理所有播放线程和共享状态
pub struct Pipeline {
    pub avsync: Arc<AVSync>,
    pub seek_request: Arc<Mutex<Option<SeekRequest>>>,
    pub seek_generation: Arc<AtomicU64>,
    pub position_us: Arc<AtomicU64>,
    pub duration_us: Arc<AtomicU64>,
    pub demux_complete: Arc<AtomicBool>,
    pub shutdown: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
    pub volume: Arc<AtomicU32>,
    pub event_tx: Sender<RaptorEvent>,
    thread_handles: Vec<std::thread::JoinHandle<()>>,
}

impl Pipeline {
    pub fn new(event_tx: Sender<RaptorEvent>) -> Self {
        Self {
            avsync: Arc::new(AVSync::new()),
            seek_request: Arc::new(Mutex::new(None)),
            seek_generation: Arc::new(AtomicU64::new(0)),
            position_us: Arc::new(AtomicU64::new(0)),
            duration_us: Arc::new(AtomicU64::new(0)),
            demux_complete: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            volume: Arc::new(AtomicU32::new(100)),
            event_tx,
            thread_handles: Vec::new(),
        }
    }

    /// 获取当前播放位置（秒）
    pub fn current_position_secs(&self) -> f64 {
        self.position_us.load(Ordering::Acquire) as f64 / 1_000_000.0
    }

    /// 暂停 pipeline
    pub fn pause(&self) {
        self.paused.store(true, Ordering::Release);
        tracing::info!("pipeline: paused");
    }

    /// 恢复 pipeline
    pub fn resume(&self) {
        let current_pos = self.current_position_secs();
        self.avsync.reset(current_pos);
        self.paused.store(false, Ordering::Release);
        tracing::info!("pipeline: resumed at {:.3}s", current_pos);
    }

    /// 检查是否暂停
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    /// 设置音量 (0-100)
    pub fn set_volume(&self, vol: u8) {
        self.volume.store(vol as u32, Ordering::Relaxed);
    }

    /// 获取音量 (0-100)
    pub fn get_volume(&self) -> u32 {
        self.volume.load(Ordering::Relaxed)
    }

    /// 启动 pipeline — 创建 demux/decode/render/audio 线程
    pub fn start(
        &mut self,
        url: &str,
        renderer: Box<dyn VideoOutput>,
        duration_secs: f64,
        video_info: Option<VideoInfo>,
        audio_info: Option<AudioInfo>,
    ) -> raptor_core::Result<()> {
        tracing::info!(
            "Pipeline::start: url={}, duration={:.2}s",
            url,
            duration_secs
        );

        // 设置 duration
        self.duration_us
            .store((duration_secs * 1_000_000.0) as u64, Ordering::Release);

        // 重置状态
        self.shutdown.store(false, Ordering::Release);
        self.demux_complete.store(false, Ordering::Release);
        self.seek_generation.store(0, Ordering::Release);
        self.position_us.store(0, Ordering::Release);

        // 创建 Demuxer 并打开文件
        let mut demuxer: Box<dyn Demuxer> = Box::new(FfmpegDemuxer::new());
        demuxer.open(url)?;

        let info = demuxer
            .info()
            .cloned()
            .ok_or_else(|| raptor_core::RaptorError::Demux("no media info".into()))?;

        // 获取 codec contexts
        let video_codec_ctx = demuxer.take_video_codec_context();
        let audio_codec_ctx = demuxer.take_audio_codec_context();

        // 创建 channels
        let (video_pkt_tx, video_pkt_rx) = bounded::<raptor_ffmpeg::Packet>(512);
        let (audio_pkt_tx, audio_pkt_rx) = bounded::<raptor_ffmpeg::Packet>(1024);
        let (video_frame_tx, video_frame_rx) = bounded::<raptor_ffmpeg::VideoFrame>(32);
        let (audio_frame_tx, audio_frame_rx) = bounded::<raptor_ffmpeg::AudioFrame>(64);

        let pipeline = Arc::new(Pipeline {
            avsync: self.avsync.clone(),
            seek_request: self.seek_request.clone(),
            seek_generation: self.seek_generation.clone(),
            position_us: self.position_us.clone(),
            duration_us: self.duration_us.clone(),
            demux_complete: self.demux_complete.clone(),
            shutdown: self.shutdown.clone(),
            paused: self.paused.clone(),
            volume: self.volume.clone(),
            event_tx: self.event_tx.clone(),
            thread_handles: Vec::new(),
        });

        // 1. Demux 线程
        let p = pipeline.clone();
        let h = std::thread::Builder::new()
            .name("raptor-demux".into())
            .spawn(move || {
                Self::run_thread(|| demux_loop(p, demuxer, video_pkt_tx, audio_pkt_tx))
                    .unwrap_or_else(|e| tracing::error!("demux thread error: {}", e));
            })
            .map_err(|e| raptor_core::RaptorError::Internal(format!("spawn demux: {e}")))?;
        self.thread_handles.push(h);

        // 2. Video decode 线程
        if video_codec_ctx.is_some() || video_info.is_some() {
            let video_decoder: Box<dyn VideoDecoder> = if let Some(ctx) = video_codec_ctx {
                Box::new(FfmpegVideoDecoder::from_stream_context(ctx)?)
            } else {
                Box::new(FfmpegVideoDecoder::new())
            };
            let p = pipeline.clone();
            let h = std::thread::Builder::new()
                .name("raptor-vdecode".into())
                .spawn(move || {
                    Self::run_thread(|| {
                        video_decode_loop(p, video_decoder, video_pkt_rx, video_frame_tx)
                    })
                    .unwrap_or_else(|e| tracing::error!("video decode thread error: {}", e));
                })
                .map_err(|e| raptor_core::RaptorError::Internal(format!("spawn vdecode: {e}")))?;
            self.thread_handles.push(h);
        }

        // 3. Audio decode 线程
        if audio_codec_ctx.is_some() || audio_info.is_some() {
            let audio_decoder: Box<dyn AudioDecoder> = if let Some(ctx) = audio_codec_ctx {
                Box::new(FfmpegAudioDecoder::from_stream_context(ctx)?)
            } else {
                Box::new(FfmpegAudioDecoder::new())
            };
            let p = pipeline.clone();
            let h = std::thread::Builder::new()
                .name("raptor-adecode".into())
                .spawn(move || {
                    Self::run_thread(|| {
                        audio_decode_loop(p, audio_decoder, audio_pkt_rx, audio_frame_tx)
                    })
                    .unwrap_or_else(|e| tracing::error!("audio decode thread error: {}", e));
                })
                .map_err(|e| raptor_core::RaptorError::Internal(format!("spawn adecode: {e}")))?;
            self.thread_handles.push(h);
        }

        // 4. Render 线程
        {
            let p = pipeline.clone();
            let event_tx = self.event_tx.clone();
            let h = std::thread::Builder::new()
                .name("raptor-render".into())
                .spawn(move || {
                    Self::run_thread(|| {
                        render_loop(p, renderer, video_frame_rx, event_tx, duration_secs)
                    })
                    .unwrap_or_else(|e| tracing::error!("render thread error: {}", e));
                })
                .map_err(|e| raptor_core::RaptorError::Internal(format!("spawn render: {e}")))?;
            self.thread_handles.push(h);
        }

        // 5. Audio output 线程
        {
            let mut audio_output: Box<dyn AudioOutput> = Box::new(CpalOutput::new());
            if let Some(ref ai) = audio_info {
                audio_output.init(ai.sample_rate, ai.channels)?;
            } else if info.sample_rate > 0 {
                audio_output.init(info.sample_rate, info.channels)?;
            }
            let p = pipeline.clone();
            let h = std::thread::Builder::new()
                .name("raptor-audio".into())
                .spawn(move || {
                    Self::run_thread(|| audio_output_loop(p, audio_output, audio_frame_rx))
                        .unwrap_or_else(|e| tracing::error!("audio thread error: {}", e));
                })
                .map_err(|e| raptor_core::RaptorError::Internal(format!("spawn audio: {e}")))?;
            self.thread_handles.push(h);
        }

        tracing::info!("Pipeline started: {} threads", self.thread_handles.len());
        Ok(())
    }

    /// 停止 pipeline — 等待所有线程退出
    pub fn stop(&mut self) {
        tracing::info!("Pipeline::stop");
        self.shutdown.store(true, Ordering::Release);

        // 取出 thread handles
        let handles: Vec<_> = self.thread_handles.drain(..).collect();
        for handle in handles {
            let _ = handle.join();
        }
        tracing::info!("Pipeline::stop: all threads joined");
    }

    /// 线程运行包装器 — 捕获 panic
    fn run_thread<F>(f: F) -> raptor_core::Result<()>
    where
        F: FnOnce() -> raptor_core::Result<()>,
    {
        match std::panic::catch_unwind(AssertUnwindSafe(f)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(panic_payload) => {
                let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                tracing::error!("thread panic: {}", msg);
                Err(raptor_core::RaptorError::Internal(format!(
                    "thread panic: {}",
                    msg
                )))
            }
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.stop();
    }
}
