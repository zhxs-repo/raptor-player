//! Raptor C ABI 导出层
//!
//! 所有 `#[no_mangle] pub extern "C"` 函数集中在此 crate。
//! Flutter/Dart 通过 dart:ffi 调用这些函数，以 JSON 字符串交换数据。

// FFI 边界函数接收 raw pointer 参数是 C ABI 要求，由调用方保证有效性
#![allow(clippy::not_unsafe_ptr_arg_deref)]

/// C ABI panic 守卫：捕获闭包内 panic，避免 unwind 跨越 extern "C" 边界（UB/abort）
macro_rules! cabi {
    ($name:expr, $fallback:expr, $body:expr) => {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe($body)) {
            Ok(v) => v,
            Err(_) => {
                tracing::error!("{}: panic caught at FFI boundary", $name);
                $fallback
            }
        }
    };
}

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Surface 生命周期命令的确认超时（FFI → render_loop → 渲染线程）
///
/// 必须大于 raptor-render 内部的 ack 超时（2s），否则内层还没判定失败外层就先超时。
const SURFACE_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

use parking_lot::Mutex;
use raptor_core::{
    Command, CommandResult, DefaultPropertyStore, ErrorCode, MediaInfo, PlayerEvent, PlayerState,
    PropertyStore, PropertyValue, RaptorError, RaptorEvent, SeekMode,
};
use raptor_danmaku::{DanmakuConfig, DanmakuEngine};
use raptor_ffmpeg::{Demuxer, FfmpegDemuxer};
use raptor_pipeline::{Pipeline, PipelineHandles, RendererCmd};
use raptor_render::{ExternalRenderer, SharedOverlay, SurfaceHandle, VideoOutput, WgpuRenderer};
use raptor_subtitle::{SubtitleConfig, SubtitleEngine};

/// 播放器核心 — 持有状态机 + pipeline + 属性存储
pub struct Player {
    state: Mutex<PlayerState>,
    properties: Arc<DefaultPropertyStore>,
    event_tx: tokio::sync::mpsc::UnboundedSender<RaptorEvent>,
    pipeline: Mutex<Option<Arc<Pipeline>>>,
    pipeline_handles: Mutex<Option<PipelineHandles>>,
    /// 位置上报后台线程（定期将 pipeline position 写入 property store）
    position_reporter: Mutex<Option<JoinHandle<()>>>,
    /// 字幕引擎（SharedOverlay 包装，与渲染线程共享）
    subtitle_engine: Mutex<Option<Arc<Mutex<SubtitleEngine>>>>,
    /// 弹幕引擎（SharedOverlay 包装，与渲染线程共享）
    danmaku_engine: Mutex<Option<Arc<Mutex<DanmakuEngine>>>>,
    /// 待使用的 Surface 句柄（set_surface 在 load_file 之前调用时暂存）
    pending_surface: Mutex<Option<SurfaceHandle>>,
    /// 渲染器命令发送端（FFI 层 → render_loop，用于运行时 Surface 管理）
    renderer_cmd_tx: Mutex<Option<crossbeam_channel::Sender<RendererCmd>>>,
}

impl Player {
    pub fn new(event_tx: tokio::sync::mpsc::UnboundedSender<RaptorEvent>) -> Self {
        let properties = Arc::new(DefaultPropertyStore::new(event_tx.clone()));
        Self {
            state: Mutex::new(PlayerState::Idle),
            properties,
            event_tx,
            pipeline: Mutex::new(None),
            pipeline_handles: Mutex::new(None),
            position_reporter: Mutex::new(None),
            subtitle_engine: Mutex::new(None),
            danmaku_engine: Mutex::new(None),
            pending_surface: Mutex::new(None),
            renderer_cmd_tx: Mutex::new(None),
        }
    }

    /// 处理命令
    pub fn dispatch_command(&self, cmd: Command) -> raptor_core::Result<CommandResult> {
        match cmd {
            Command::LoadFile { url } => self.load_file(&url),
            Command::Play => self.play(),
            Command::Pause => self.pause(),
            Command::TogglePause => self.toggle_pause(),
            Command::Stop => self.stop(),
            Command::Seek { target, mode } => self.seek(target, mode),
            Command::SetVolume { volume } => {
                self.properties
                    .set("volume", PropertyValue::Int(volume as i64));
                if let Some(pipeline) = self.pipeline.lock().as_ref() {
                    pipeline.set_volume(volume);
                }
                Ok(CommandResult::Empty)
            }
            Command::LoadSubtitle { path } => self.load_subtitle(&path),
            Command::ToggleSubtitle => self.toggle_subtitle(),
            Command::LoadDanmaku { path } => self.load_danmaku(&path),
            Command::ToggleDanmaku => self.toggle_danmaku(),
            Command::SetDanmakuOpacity { opacity } => {
                if let Some(engine) = self.danmaku_engine.lock().as_ref() {
                    let shared = engine.lock().shared_state();
                    let mut state = shared.lock();
                    // opacity 是 0-100 的 u8，转换为 0.0-1.0
                    state.opacity = opacity as f32 / 100.0;
                    self.properties
                        .set("danmaku_opacity", PropertyValue::Int(opacity as i64));
                }
                Ok(CommandResult::Empty)
            }
            Command::Quit => {
                self.stop_pipeline();
                let _ = self.event_tx.send(RaptorEvent::End);
                Ok(CommandResult::Empty)
            }
        }
    }

    fn load_file(&self, url: &str) -> raptor_core::Result<CommandResult> {
        let prev_state = {
            let state = self.state.lock();
            state
                .can_transition_to(&PlayerEvent::Load {
                    url: url.to_string(),
                })
                .map_err(RaptorError::InvalidState)?;
            state.clone()
        };

        // 先停止旧 pipeline
        self.stop_pipeline();

        // 文件存在性预检查 — 提供明确的 FileNotFound 错误
        if !std::path::Path::new(url).exists() {
            // 状态回退到可重新加载的状态
            *self.state.lock() = if matches!(prev_state, PlayerState::Idle) {
                PlayerState::Idle
            } else {
                PlayerState::Stopped
            };
            return Err(RaptorError::FileNotFound(url.to_string()));
        }

        // 转为 Loading；后续任何失败统一回滚为 Stopped，避免 handle 卡死在 Loading
        *self.state.lock() = PlayerState::Loading;
        let result = (|| -> raptor_core::Result<CommandResult> {
            // 打开文件获取信息
            let mut demuxer = FfmpegDemuxer::new();
            demuxer.open(url)?;
            let info = demuxer
                .info()
                .cloned()
                .ok_or_else(|| raptor_core::RaptorError::Demux("no media info".into()))?;

            let video_info = if info.video_stream_index.is_some() {
                Some(raptor_core::VideoInfo {
                    width: info.width,
                    height: info.height,
                    codec: format!("{:?}", info.video_codec_id),
                    fps: info.fps,
                })
            } else {
                None
            };

            let audio_info = if info.audio_stream_index.is_some() {
                Some(raptor_core::AudioInfo {
                    codec: format!("{:?}", info.audio_codec_id),
                    channels: info.channels,
                    sample_rate: info.sample_rate,
                })
            } else {
                None
            };

            let duration = info.duration;

            // 创建 renderer（检查是否有预设的外部 Surface）
            let mut renderer: Box<dyn VideoOutput> =
                if let Some(surface) = self.pending_surface.lock().take() {
                    let mut ext = ExternalRenderer::new();
                    ext.init_with_surface(surface).map_err(|e| {
                        raptor_core::RaptorError::Render(format!("init external renderer: {e}"))
                    })?;
                    Box::new(ext)
                } else {
                    let mut r = WgpuRenderer::new();
                    if let Some(ref vi) = video_info {
                        r.init(vi.width, vi.height)?;
                    }
                    Box::new(r)
                };

            // 创建字幕和弹幕引擎，并通过 SharedOverlay 包装后设置到渲染器
            let subtitle_engine =
                Arc::new(Mutex::new(SubtitleEngine::new(SubtitleConfig::default())));
            let danmaku_engine = Arc::new(Mutex::new(DanmakuEngine::new(DanmakuConfig::default())));

            // 自动加载系统字体（供字幕和弹幕光栅化文本使用）
            if let Some(font_data) = raptor_subtitle::load_system_font() {
                subtitle_engine.lock().set_font(font_data.clone());
                danmaku_engine.lock().set_font(font_data);
                tracing::info!("load_file: system font loaded for subtitle/danmaku engines");
            }

            // 在 renderer 被移入 pipeline 之前设置 overlays
            let overlays: Vec<Box<dyn raptor_render::Overlay>> = vec![
                Box::new(SharedOverlay::new(subtitle_engine.clone())),
                Box::new(SharedOverlay::new(danmaku_engine.clone())),
            ];
            renderer.set_overlays(overlays);

            // 存储引擎引用（供后续 LoadSubtitle / LoadDanmaku 命令使用）
            *self.subtitle_engine.lock() = Some(subtitle_engine);
            *self.danmaku_engine.lock() = Some(danmaku_engine);

            // 包装 renderer 为共享引用（render_loop 拥有所有权，FFI 层通过 channel 发送命令）
            let shared_renderer: Arc<Mutex<Box<dyn VideoOutput>>> = Arc::new(Mutex::new(renderer));

            // 创建渲染器命令 channel（FFI 层 → render_loop）
            let (renderer_cmd_tx, renderer_cmd_rx) = crossbeam_channel::bounded::<RendererCmd>(16);

            // 创建 pipeline（暂停状态）— 将 demuxer 传递给 pipeline，避免重复打开文件
            let (crossbeam_tx, crossbeam_rx) = crossbeam_channel::bounded(64);
            let mut pipeline = Pipeline::new(crossbeam_tx);
            pipeline.pause(); // 创建后先暂停，等 Play 命令再恢复
            tracing::info!("load_file: calling pipeline.start()...");
            match pipeline.start(
                url,
                Box::new(demuxer),
                shared_renderer,
                duration,
                video_info.clone(),
                audio_info.clone(),
                self.subtitle_engine.lock().clone(),
                Some(renderer_cmd_rx),
            ) {
                Ok(()) => tracing::info!("load_file: pipeline.start() returned Ok"),
                Err(e) => {
                    tracing::error!("load_file: pipeline.start() FAILED: {}", e);
                    // 状态回退，允许重新加载
                    *self.state.lock() = PlayerState::Stopped;
                    return Err(e);
                }
            }
            let pipeline = Arc::new(pipeline);

            *self.pipeline.lock() = Some(pipeline.clone());

            // 存储渲染器命令发送端（供后续 set_surface/detach_surface 使用）
            *self.renderer_cmd_tx.lock() = Some(renderer_cmd_tx);

            // 启动位置上报线程
            self.start_position_reporter(pipeline.clone());

            // 转发 crossbeam 事件到 tokio channel
            let event_tx = self.event_tx.clone();
            std::thread::Builder::new()
                .name("raptor-evt-fwd".into())
                .spawn(move || {
                    while let Ok(event) = crossbeam_rx.recv() {
                        if event_tx.send(event).is_err() {
                            break;
                        }
                    }
                })
                .ok();

            // 状态转换 → Ready
            *self.state.lock() = PlayerState::Ready;

            // 初始化 position 属性
            self.properties.set("position", PropertyValue::Float(0.0));

            // 发送 FileLoaded 事件
            let _ = self.event_tx.send(RaptorEvent::FileLoaded {
                duration,
                video: video_info.clone(),
                audio: audio_info.clone(),
            });

            tracing::info!("load_file: ready, duration={:.2}s", duration);
            Ok(CommandResult::MediaInfo(MediaInfo {
                duration,
                video: video_info,
                audio: audio_info,
            }))
        })();
        if result.is_err() {
            *self.state.lock() = PlayerState::Stopped;
            self.stop_pipeline();
        }
        result
    }

    fn play(&self) -> raptor_core::Result<CommandResult> {
        let state = self.state.lock();
        state
            .can_transition_to(&PlayerEvent::Play)
            .map_err(RaptorError::InvalidState)?;
        drop(state);

        // 恢复 pipeline
        if let Some(pipeline) = self.pipeline.lock().as_ref() {
            pipeline.resume();
        }

        *self.state.lock() = PlayerState::Playing;
        let _ = self.event_tx.send(RaptorEvent::PlaybackRestart);
        Ok(CommandResult::Empty)
    }

    fn pause(&self) -> raptor_core::Result<CommandResult> {
        let state = self.state.lock();
        state
            .can_transition_to(&PlayerEvent::Pause)
            .map_err(RaptorError::InvalidState)?;
        drop(state);

        // 读取当前位置
        let pos = self
            .pipeline
            .lock()
            .as_ref()
            .map(|p| p.current_position_secs())
            .unwrap_or(0.0);

        // 暂停 pipeline
        if let Some(pipeline) = self.pipeline.lock().as_ref() {
            pipeline.pause();
        }

        *self.state.lock() = PlayerState::Paused;
        self.properties.set("position", PropertyValue::Float(pos));
        Ok(CommandResult::Empty)
    }

    fn toggle_pause(&self) -> raptor_core::Result<CommandResult> {
        let state = self.state.lock().clone();
        match state {
            PlayerState::Playing => self.pause(),
            PlayerState::Paused => self.play(),
            _ => Err(raptor_core::RaptorError::InvalidState(format!(
                "cannot toggle from {:?}",
                state
            ))),
        }
    }

    fn stop(&self) -> raptor_core::Result<CommandResult> {
        let state = self.state.lock();
        state
            .can_transition_to(&PlayerEvent::Stop)
            .map_err(RaptorError::InvalidState)?;
        drop(state);

        self.stop_pipeline();
        *self.state.lock() = PlayerState::Stopped;
        Ok(CommandResult::Empty)
    }

    fn seek(&self, target: f64, _mode: SeekMode) -> raptor_core::Result<CommandResult> {
        // 参数校验：target 不能为负数
        if target < 0.0 {
            return Err(RaptorError::InvalidArgument(format!(
                "seek target must be non-negative, got {:.3}",
                target
            )));
        }

        // 状态检查：只有 Playing 和 Paused 可以 seek
        let state = self.state.lock();
        state
            .can_transition_to(&PlayerEvent::Seek {
                target,
                mode: SeekMode::Absolute,
            })
            .map_err(RaptorError::InvalidState)?;
        drop(state);

        // Clamp target to duration if available
        let clamped_target = if let Some(pipeline) = self.pipeline.lock().as_ref() {
            let duration = pipeline.duration_secs();
            if duration > 0.0 && target > duration {
                tracing::warn!(
                    "seek target {:.3}s exceeds duration {:.3}s, clamping",
                    target,
                    duration
                );
                duration
            } else {
                target
            }
        } else {
            target
        };

        let pos = self
            .pipeline
            .lock()
            .as_ref()
            .map(|p| p.current_position_secs())
            .unwrap_or(0.0);

        if let Some(pipeline) = self.pipeline.lock().as_ref() {
            *pipeline.seek_request.lock() = Some(raptor_pipeline::pipeline::SeekRequest {
                target: clamped_target,
                from: pos,
            });
        }

        let _ = self.event_tx.send(RaptorEvent::Seek {
            from: pos,
            to: clamped_target,
        });
        Ok(CommandResult::Empty)
    }

    fn stop_pipeline(&self) {
        // 先取出 pipeline 并置位 shutdown：position reporter 只在该标志为 true 时才退出，
        // 若先 join 再 stop()（stop() 内部才置位）会互相等待而死锁
        let pipeline = self.pipeline.lock().take();
        if let Some(p) = pipeline.as_ref() {
            p.request_shutdown();
        }

        // 停止位置报备
        if let Some(handle) = self.position_reporter.lock().take() {
            let _ = handle.join();
        }

        if let Some(p) = pipeline {
            // join 所有工作线程
            p.stop();
        }
        *self.pipeline_handles.lock() = None;

        // 清除渲染器命令通道
        *self.renderer_cmd_tx.lock() = None;

        // 清除字幕/弹幕引擎引用
        *self.subtitle_engine.lock() = None;
        *self.danmaku_engine.lock() = None;
    }

    fn start_position_reporter(&self, pipeline: Arc<Pipeline>) {
        let properties = self.properties.clone();
        let handle = std::thread::Builder::new()
            .name("raptor-pos-rpt".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(200));

                    // 如果 pipeline 已停止，退出
                    if pipeline.shutdown.load(std::sync::atomic::Ordering::Acquire) {
                        break;
                    }

                    // 仅在非暂停状态更新
                    if !pipeline.is_paused() {
                        let pos = pipeline.current_position_secs();
                        properties.set("position", PropertyValue::Float(pos));
                    }
                }
            });

        match handle {
            Ok(h) => *self.position_reporter.lock() = Some(h),
            Err(e) => tracing::warn!("spawn position reporter failed: {e}"),
        }
    }

    fn load_subtitle(&self, path: &str) -> raptor_core::Result<CommandResult> {
        let engine = self
            .subtitle_engine
            .lock()
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                raptor_core::RaptorError::Internal("subtitle engine not initialized".into())
            })?;

        engine.lock().load_from_file(path)?;
        self.properties
            .set("subtitle_enabled", PropertyValue::Bool(true));
        tracing::info!("load_subtitle: loaded from {}", path);
        Ok(CommandResult::Empty)
    }

    fn toggle_subtitle(&self) -> raptor_core::Result<CommandResult> {
        let engine = self
            .subtitle_engine
            .lock()
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                raptor_core::RaptorError::Internal("subtitle engine not initialized".into())
            })?;

        let state = engine.lock().shared_state();
        let mut state = state.lock();
        state.enabled = !state.enabled;
        let enabled = state.enabled;
        drop(state);
        self.properties
            .set("subtitle_enabled", PropertyValue::Bool(enabled));
        tracing::info!("toggle_subtitle: enabled={}", enabled);
        Ok(CommandResult::Empty)
    }

    fn load_danmaku(&self, path: &str) -> raptor_core::Result<CommandResult> {
        let engine = self
            .danmaku_engine
            .lock()
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                raptor_core::RaptorError::Internal("danmaku engine not initialized".into())
            })?;

        engine.lock().load_from_file(path)?;
        self.properties
            .set("danmaku_enabled", PropertyValue::Bool(true));
        tracing::info!("load_danmaku: loaded from {}", path);
        Ok(CommandResult::Empty)
    }

    fn toggle_danmaku(&self) -> raptor_core::Result<CommandResult> {
        let engine = self
            .danmaku_engine
            .lock()
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                raptor_core::RaptorError::Internal("danmaku engine not initialized".into())
            })?;

        let state = engine.lock().shared_state();
        let mut state = state.lock();
        state.enabled = !state.enabled;
        let enabled = state.enabled;
        drop(state);
        self.properties
            .set("danmaku_enabled", PropertyValue::Bool(enabled));
        tracing::info!("toggle_danmaku: enabled={}", enabled);
        Ok(CommandResult::Empty)
    }

    /// 获取 HUD 统计信息（供 CLI 播放器输出）
    pub fn get_hud_stats(&self) -> raptor_render::HudStats {
        let (position, duration) = self
            .pipeline
            .lock()
            .as_ref()
            .map(|p| {
                (
                    p.current_position_secs(),
                    p.duration_us.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1_000_000.0,
                )
            })
            .unwrap_or((0.0, 0.0));

        let paused = self
            .pipeline
            .lock()
            .as_ref()
            .map(|p| p.is_paused())
            .unwrap_or(true);

        let (subtitle_on, danmaku_on, danmaku_count) = {
            let sub_on = self
                .subtitle_engine
                .lock()
                .as_ref()
                .map(|e| e.lock().shared_state().lock().enabled)
                .unwrap_or(false);
            let (dm_on, dm_count) = self
                .danmaku_engine
                .lock()
                .as_ref()
                .map(|e| {
                    let state = e.lock().shared_state();
                    let s = state.lock();
                    (s.enabled, s.instances.len() as u32)
                })
                .unwrap_or((false, 0));
            (sub_on, dm_on, dm_count)
        };

        raptor_render::HudStats {
            paused,
            position_secs: position,
            duration_secs: duration,
            subtitle_on,
            danmaku_on,
            danmaku_count,
            ..Default::default()
        }
    }

    // === Surface 管理（Android / 嵌入式平台） ===

    /// 设置外部 Surface（Android: ANativeWindow*）
    ///
    /// 如果在 `load_file` 之前调用，Surface 会被暂存，load_file 时自动创建 ExternalRenderer。
    /// 如果在 `load_file` 之后调用，通过 RendererCmd 通道将新渲染器发送到 render_loop。
    pub fn set_surface(
        &self,
        native_window: u64,
        native_display: u64,
        width: u32,
        height: u32,
    ) -> raptor_core::Result<()> {
        let surface = SurfaceHandle::new(native_window, native_display, width, height);

        if let Some(_pipeline) = self.pipeline.lock().as_ref() {
            // Pipeline 已运行 — 通过 channel 发送 SetSurface 命令
            if let Some(cmd_tx) = self.renderer_cmd_tx.lock().as_ref() {
                let mut ext = ExternalRenderer::new();
                ext.init_with_surface(surface).map_err(|e| {
                    raptor_core::RaptorError::Render(format!("init external renderer: {e}"))
                })?;

                // 创建 overlays（与 load_file 中的相同）
                let overlays: Vec<Box<dyn raptor_render::Overlay>> = {
                    let mut v: Vec<Box<dyn raptor_render::Overlay>> = Vec::new();
                    if let Some(ref engine) = *self.subtitle_engine.lock() {
                        v.push(Box::new(SharedOverlay::new(engine.clone())));
                    }
                    if let Some(ref engine) = *self.danmaku_engine.lock() {
                        v.push(Box::new(SharedOverlay::new(engine.clone())));
                    }
                    v
                };

                cmd_tx
                    .send(RendererCmd::SetSurface(Box::new(ext), surface, overlays))
                    .map_err(|e| {
                        raptor_core::RaptorError::Internal(format!("send SetSurface command: {e}"))
                    })?;
                tracing::info!(
                    "set_surface: sent SetSurface command ({}x{}, native_window=0x{:x})",
                    width,
                    height,
                    native_window
                );
            }
        } else {
            // Pipeline 尚未启动 — 暂存 surface，load_file 时使用
            *self.pending_surface.lock() = Some(surface);
            tracing::info!(
                "set_surface: stored pending surface ({}x{}, native_window=0x{:x})",
                width,
                height,
                native_window
            );
        }

        Ok(())
    }

    /// 分离当前 Surface（Android onPause 时调用）
    ///
    /// 同步语义：返回 Ok 表示渲染线程已经 drop 掉 `wgpu::Surface`，原生窗口引用
    /// 已归还。宿主在本调用返回后立即销毁 `ANativeWindow`，因此不能只"把命令入队"
    /// 就返回成功，否则渲染线程会在已释放的窗口上继续操作（UAF）。
    pub fn detach_surface(&self) -> raptor_core::Result<()> {
        *self.pending_surface.lock() = None;

        let Some(cmd_tx) = self.renderer_cmd_tx.lock().clone() else {
            // pipeline 未运行：没有 Surface 需要释放
            return Ok(());
        };

        let (ack_tx, ack_rx) = crossbeam_channel::bounded::<raptor_core::Result<()>>(1);
        cmd_tx
            .send(RendererCmd::DetachSurface(ack_tx))
            .map_err(|_| {
                raptor_core::RaptorError::Render("detach_surface: render loop is gone".into())
            })?;

        match ack_rx.recv_timeout(SURFACE_ACK_TIMEOUT) {
            Ok(result) => {
                if result.is_ok() {
                    tracing::info!("detach_surface: surface released by render thread");
                } else {
                    tracing::error!("detach_surface: render thread reported {result:?}");
                }
                result
            }
            Err(_) => Err(raptor_core::RaptorError::Render(format!(
                "detach_surface: render loop did not acknowledge within {SURFACE_ACK_TIMEOUT:?}"
            ))),
        }
    }

    /// 调整 Surface 尺寸（屏幕旋转等场景）
    pub fn resize_surface(&self, width: u32, height: u32) -> raptor_core::Result<()> {
        // 更新 pending surface 尺寸（如果尚未启动 pipeline）
        if let Some(ref mut surface) = *self.pending_surface.lock() {
            surface.width = width;
            surface.height = height;
        }
        if let Some(_pipeline) = self.pipeline.lock().as_ref() {
            if let Some(cmd_tx) = self.renderer_cmd_tx.lock().as_ref() {
                let _ = cmd_tx.send(RendererCmd::ResizeSurface { width, height });
                tracing::info!("resize_surface: sent ResizeSurface({}x{})", width, height);
            }
        }
        Ok(())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop_pipeline();
    }
}

// ═══════════════════════════════════════════════════
// FFI Handle
// ═══════════════════════════════════════════════════

/// 不透明句柄 — Dart 侧只看到指针
pub struct RaptorHandle {
    player: Player,
    event_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<RaptorEvent>>>,
    callback_thread: Mutex<Option<JoinHandle<()>>>,
    /// observer_id → property_name 映射（用于 unobserve 时定位属性名）
    observers: Mutex<HashMap<i64, String>>,
    last_error: Mutex<Option<String>>,
}

impl Drop for RaptorHandle {
    fn drop(&mut self) {
        // 1. 先停 pipeline 与 position reporter 线程：
        //    确保不再有后台线程调用 properties.set 触发 observer 回调，
        //    否则 observer 移除后仍可能调用已失效的 Dart 回调（UAF 窗口）
        self.player.stop_pipeline();

        // 2. 清理所有 observer，防止 UAF
        //    逐一调用 PropertyStore::unobserve 移除底层回调闭包
        let observers: HashMap<i64, String> = self.observers.lock().drain().collect();
        for (observer_id, name) in observers {
            self.player.properties.unobserve(&name, observer_id);
        }

        // 3. 发送 End 事件通知回调线程退出并 join
        let _ = self.player.event_tx.send(RaptorEvent::End);
        let thread = self.callback_thread.lock().take();
        if let Some(t) = thread {
            let _ = t.join();
        }
    }
}

// ═══════════════════════════════════════════════════
// C ABI Exports
// ═══════════════════════════════════════════════════

/// 创建播放器实例
fn raptor_create_impl() -> *mut RaptorHandle {
    // 初始化日志
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let player = Player::new(event_tx);

    Box::into_raw(Box::new(RaptorHandle {
        player,
        event_rx: Mutex::new(Some(event_rx)),
        callback_thread: Mutex::new(None),
        observers: Mutex::new(HashMap::new()),
        last_error: Mutex::new(None),
    }))
}

/// 销毁播放器实例
fn raptor_destroy_impl(handle: *mut RaptorHandle) {
    if handle.is_null() {
        return;
    }

    let _handle = unsafe { Box::from_raw(handle) };
    // Drop 会负责清理：
    // 1. Player::drop → stop_pipeline（停止所有工作线程）
    // 2. RaptorHandle::drop → 发送 End 事件、join 回调线程
    //    → 清理所有 observer 防止 UAF
}

/// 发送命令（JSON 格式）
fn raptor_command_impl(handle: *mut RaptorHandle, cmd_json: *const c_char) -> c_int {
    if handle.is_null() || cmd_json.is_null() {
        return ErrorCode::InvalidArgument as c_int;
    }

    let handle = unsafe { &*handle };
    let cmd_str = match unsafe { CStr::from_ptr(cmd_json) }.to_str() {
        Ok(s) => s,
        Err(_) => return ErrorCode::InvalidArgument as c_int,
    };

    let cmd: Command = match serde_json::from_str(cmd_str) {
        Ok(c) => c,
        Err(e) => {
            *handle.last_error.lock() = Some(format!("parse command: {}", e));
            return ErrorCode::InvalidArgument as c_int;
        }
    };

    match handle.player.dispatch_command(cmd) {
        Ok(_) => ErrorCode::Ok as c_int,
        Err(e) => {
            *handle.last_error.lock() = Some(format!("{}", e));
            e.error_code() as c_int
        }
    }
}

/// 读取属性（返回 JSON 字符串，调用者需调用 raptor_free_string 释放）
fn raptor_get_property_impl(handle: *mut RaptorHandle, name: *const c_char) -> *mut c_char {
    if handle.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }

    let handle = unsafe { &*handle };
    let name = match unsafe { CStr::from_ptr(name) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };

    match handle.player.properties.get(name) {
        Some(val) => {
            let json = val.to_json();
            match CString::new(json) {
                Ok(cstr) => cstr.into_raw(),
                Err(_) => std::ptr::null_mut(),
            }
        }
        None => std::ptr::null_mut(),
    }
}

/// 设置属性（JSON 格式）
fn raptor_set_property_impl(
    handle: *mut RaptorHandle,
    name: *const c_char,
    value_json: *const c_char,
) -> c_int {
    if handle.is_null() || name.is_null() || value_json.is_null() {
        return ErrorCode::InvalidArgument as c_int;
    }

    let handle = unsafe { &*handle };
    let name = match unsafe { CStr::from_ptr(name) }.to_str() {
        Ok(s) => s,
        Err(_) => return ErrorCode::InvalidArgument as c_int,
    };
    let value_str = match unsafe { CStr::from_ptr(value_json) }.to_str() {
        Ok(s) => s,
        Err(_) => return ErrorCode::InvalidArgument as c_int,
    };

    // 尝试解析 JSON 值
    let value: PropertyValue = if let Ok(n) = value_str.parse::<i64>() {
        PropertyValue::Int(n)
    } else if let Ok(f) = value_str.parse::<f64>() {
        PropertyValue::Float(f)
    } else if value_str == "true" {
        PropertyValue::Bool(true)
    } else if value_str == "false" {
        PropertyValue::Bool(false)
    } else {
        // 当作字符串（去掉引号）
        let s = value_str.trim_matches('"').to_string();
        PropertyValue::String(s)
    };

    handle.player.properties.set(name, value);
    ErrorCode::Ok as c_int
}

// === Property Observer ===

/// 属性观察者回调函数类型
pub type RaptorPropertyCallback = extern "C" fn(*const c_char, *mut c_void);

/// 订阅属性变化
///
/// 当指定属性发生变化时，调用 C 函数指针回调。
/// 返回观察者 ID（>= 0），失败返回 -1。
/// 回调参数：(value_json: *const c_char, user_data: *mut c_void)
fn raptor_observe_property_impl(
    handle: *mut RaptorHandle,
    name: *const c_char,
    callback: Option<RaptorPropertyCallback>,
    user_data: *mut c_void,
) -> i64 {
    if handle.is_null() || name.is_null() {
        return -1;
    }

    let Some(cb) = callback else {
        return -1;
    };

    let handle = unsafe { &*handle };
    let name = match unsafe { CStr::from_ptr(name) }.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };

    // 将 user_data 转为 usize（Send-safe）
    let user_data_addr = user_data as usize;

    let observer_id = handle.player.properties.observe(
        name,
        Arc::new(move |value: &PropertyValue| {
            let json = value.to_json();
            if let Ok(cstr) = CString::new(json) {
                let ud = user_data_addr as *mut c_void;
                cb(cstr.as_ptr(), ud);
            }
        }),
    );

    handle
        .observers
        .lock()
        .insert(observer_id, name.to_string());
    observer_id
}

/// 取消属性订阅
///
/// 传入 `raptor_observe_property` 返回的 observer ID。
/// 成功返回 0，失败返回负数错误码。
fn raptor_unobserve_property_impl(handle: *mut RaptorHandle, observer_id: i64) -> c_int {
    if handle.is_null() {
        return ErrorCode::InvalidArgument as c_int;
    }

    let handle = unsafe { &*handle };
    // 通过 observer_id → property_name 映射，定位属性名并调用 PropertyStore::unobserve
    let property_name = handle.observers.lock().remove(&observer_id);
    if let Some(name) = property_name {
        handle.player.properties.unobserve(&name, observer_id);
    }
    ErrorCode::Ok as c_int
}

// === 事件 ===

/// 事件回调函数类型
//
// 仅供 Rust 侧签名使用；`#[no_mangle]` 导出函数里的回调参数必须写成匿名
// `Option<extern "C" fn(..)>`，否则 cbindgen 只输出 `struct Option_RaptorEventCallback`
// 占位类型，C 头文件里无法声明回调。
pub type RaptorEventCallback = extern "C" fn(*const c_char, *mut c_void);

/// 设置事件回调
///
/// 注册后启动后台线程，从事件通道读取事件并调用回调函数。
/// 注意：设置回调后 `raptor_poll_event` 将不再可用（event_rx 已被回调线程接管）。
/// 传入 `None` 作为 callback 不会取消回调（需要销毁实例）。
fn raptor_set_event_callback_impl(
    handle: *mut RaptorHandle,
    callback: Option<RaptorEventCallback>,
    user_data: *mut c_void,
) {
    if handle.is_null() {
        return;
    }
    let handle = unsafe { &*handle };

    let Some(cb) = callback else { return };

    // 取出 event_rx（只能调用一次，后续 poll_event 不可用）
    let event_rx = handle.event_rx.lock().take();
    let Some(mut rx) = event_rx else {
        tracing::warn!("raptor_set_event_callback: event_rx already taken by previous callback");
        return;
    };

    // 将 user_data 指针转为 usize（Send-safe），线程内再转回
    let user_data_addr = user_data as usize;

    let thread = std::thread::Builder::new()
        .name("raptor-evt-cb".into())
        .spawn(move || {
            while let Some(event) = rx.blocking_recv() {
                let is_end = matches!(event, RaptorEvent::End);
                match serde_json::to_string(&event) {
                    Ok(json) => {
                        if let Ok(cstr) = CString::new(json) {
                            let ud = user_data_addr as *mut c_void;
                            cb(cstr.as_ptr(), ud);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("event serialize error: {}", e);
                    }
                }
                if is_end {
                    break;
                }
            }
        })
        .expect("spawn event callback thread");

    *handle.callback_thread.lock() = Some(thread);
}

/// 轮询事件（非阻塞，返回 JSON 字符串或 null）
///
/// 注意：如果已调用 `raptor_set_event_callback`，此函数将始终返回 null（event_rx 已被回调线程接管）。
fn raptor_poll_event_impl(handle: *mut RaptorHandle) -> *mut c_char {
    if handle.is_null() {
        return std::ptr::null_mut();
    }

    let handle = unsafe { &*handle };
    let mut guard = handle.event_rx.lock();
    let Some(ref mut rx) = *guard else {
        return std::ptr::null_mut();
    };

    match rx.try_recv() {
        Ok(event) => match serde_json::to_string(&event) {
            Ok(json) => match CString::new(json) {
                Ok(cstr) => cstr.into_raw(),
                Err(_) => std::ptr::null_mut(),
            },
            Err(_) => std::ptr::null_mut(),
        },
        Err(_) => std::ptr::null_mut(),
    }
}

/// 获取 GPU 纹理 ID（供 Flutter Texture Widget 使用）
fn raptor_get_texture_id_impl(handle: *mut RaptorHandle) -> i64 {
    if handle.is_null() {
        return -1;
    }
    // TODO: 从 renderer 获取实际纹理 ID
    -1
}

/// 设置平台原生渲染器
fn raptor_set_renderer_impl(handle: *mut RaptorHandle, _renderer: *mut c_void) {
    if handle.is_null() {
        // TODO: 集成平台原生渲染器
    }
}

// === Android Surface 管理 ===

/// 设置外部 Surface（Android: ANativeWindow*）
///
/// 在 `raptor_command(LoadFile)` 之前或之后调用均可。
/// - 之前：Surface 被暂存，load_file 时自动创建 ExternalRenderer
/// - 之后：通过 channel 将新渲染器发送到 render_loop 进行热替换
///
/// `native_window`: ANativeWindow* 转为 u64
/// `native_display`: 平台显示连接（Android 不使用，传 0）
/// `width`/`height`: Surface 尺寸（像素）
fn raptor_set_surface_impl(
    handle: *mut RaptorHandle,
    native_window: i64,
    native_display: i64,
    width: u32,
    height: u32,
) -> c_int {
    if handle.is_null() {
        return ErrorCode::InvalidArgument as c_int;
    }
    let handle = unsafe { &*handle };
    match handle
        .player
        .set_surface(native_window as u64, native_display as u64, width, height)
    {
        Ok(()) => ErrorCode::Ok as c_int,
        Err(e) => {
            *handle.last_error.lock() = Some(format!("{}", e));
            e.error_code() as c_int
        }
    }
}

/// 分离当前 Surface（Android onPause 时调用）
///
/// 渲染线程将暂停上屏，但保持解码状态不变。
/// 音频继续播放，视频帧被静默丢弃。
fn raptor_detach_surface_impl(handle: *mut RaptorHandle) -> c_int {
    if handle.is_null() {
        return ErrorCode::InvalidArgument as c_int;
    }
    let handle = unsafe { &*handle };
    match handle.player.detach_surface() {
        Ok(()) => ErrorCode::Ok as c_int,
        Err(e) => {
            *handle.last_error.lock() = Some(format!("{}", e));
            e.error_code() as c_int
        }
    }
}

/// 调整 Surface 尺寸（屏幕旋转等场景）
fn raptor_resize_surface_impl(handle: *mut RaptorHandle, width: u32, height: u32) -> c_int {
    if handle.is_null() {
        return ErrorCode::InvalidArgument as c_int;
    }
    let handle = unsafe { &*handle };
    match handle.player.resize_surface(width, height) {
        Ok(()) => ErrorCode::Ok as c_int,
        Err(e) => {
            *handle.last_error.lock() = Some(format!("{}", e));
            e.error_code() as c_int
        }
    }
}

/// 释放由 raptor 分配的字符串
fn raptor_free_string_impl(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            let _ = CString::from_raw(s);
        }
    }
}

/// 获取最近的错误信息（返回 JSON 字符串或 null）
fn raptor_last_error_impl(handle: *mut RaptorHandle) -> *mut c_char {
    if handle.is_null() {
        return std::ptr::null_mut();
    }

    let handle = unsafe { &*handle };
    let err = handle.last_error.lock().take();
    match err {
        Some(msg) => match CString::new(msg) {
            Ok(cstr) => cstr.into_raw(),
            Err(_) => std::ptr::null_mut(),
        },
        None => std::ptr::null_mut(),
    }
}

// ═══════════════════════════════════════════════════
// C ABI 守卫包装层 — 所有 extern "C" 入口在此捕获 panic，
// 防止 unwind 跨越 FFI 边界导致 UB/abort
// ═══════════════════════════════════════════════════

/// 创建播放器实例
#[no_mangle]
pub extern "C" fn raptor_create() -> *mut RaptorHandle {
    cabi!("raptor_create", std::ptr::null_mut(), || {
        raptor_create_impl()
    })
}

/// 销毁播放器实例
#[no_mangle]
pub extern "C" fn raptor_destroy(handle: *mut RaptorHandle) {
    cabi!("raptor_destroy", (), || { raptor_destroy_impl(handle) })
}

/// 发送命令（JSON 格式）
#[no_mangle]
pub extern "C" fn raptor_command(handle: *mut RaptorHandle, cmd_json: *const c_char) -> c_int {
    cabi!("raptor_command", ErrorCode::Internal as c_int, || {
        raptor_command_impl(handle, cmd_json)
    })
}

/// 读取属性（返回 JSON 字符串，调用者需调用 raptor_free_string 释放）
#[no_mangle]
pub extern "C" fn raptor_get_property(
    handle: *mut RaptorHandle,
    name: *const c_char,
) -> *mut c_char {
    cabi!("raptor_get_property", std::ptr::null_mut(), || {
        raptor_get_property_impl(handle, name)
    })
}

/// 设置属性（JSON 格式）
#[no_mangle]
pub extern "C" fn raptor_set_property(
    handle: *mut RaptorHandle,
    name: *const c_char,
    value_json: *const c_char,
) -> c_int {
    cabi!("raptor_set_property", ErrorCode::Internal as c_int, || {
        raptor_set_property_impl(handle, name, value_json)
    })
}

/// 订阅属性变化
#[no_mangle]
pub extern "C" fn raptor_observe_property(
    handle: *mut RaptorHandle,
    name: *const c_char,
    callback: Option<extern "C" fn(*const c_char, *mut c_void)>,
    user_data: *mut c_void,
) -> i64 {
    cabi!("raptor_observe_property", -1i64, || {
        raptor_observe_property_impl(handle, name, callback, user_data)
    })
}

/// 取消属性订阅
#[no_mangle]
pub extern "C" fn raptor_unobserve_property(handle: *mut RaptorHandle, observer_id: i64) -> c_int {
    cabi!(
        "raptor_unobserve_property",
        ErrorCode::Internal as c_int,
        || { raptor_unobserve_property_impl(handle, observer_id) }
    )
}

/// 设置事件回调
///
/// 注意：设置回调后 `raptor_poll_event` 将不再可用（event_rx 已被回调线程接管）。
#[no_mangle]
pub extern "C" fn raptor_set_event_callback(
    handle: *mut RaptorHandle,
    callback: Option<extern "C" fn(*const c_char, *mut c_void)>,
    user_data: *mut c_void,
) {
    cabi!("raptor_set_event_callback", (), || {
        raptor_set_event_callback_impl(handle, callback, user_data)
    })
}

/// 轮询事件（非阻塞，返回 JSON 字符串或 null）
#[no_mangle]
pub extern "C" fn raptor_poll_event(handle: *mut RaptorHandle) -> *mut c_char {
    cabi!("raptor_poll_event", std::ptr::null_mut(), || {
        raptor_poll_event_impl(handle)
    })
}

/// 获取 GPU 纹理 ID（供 Flutter Texture Widget 使用）
#[no_mangle]
pub extern "C" fn raptor_get_texture_id(handle: *mut RaptorHandle) -> i64 {
    cabi!("raptor_get_texture_id", -1i64, || {
        raptor_get_texture_id_impl(handle)
    })
}

/// 设置平台原生渲染器
#[no_mangle]
pub extern "C" fn raptor_set_renderer(handle: *mut RaptorHandle, renderer: *mut c_void) {
    cabi!("raptor_set_renderer", (), || {
        raptor_set_renderer_impl(handle, renderer)
    })
}

/// 设置外部 Surface（Android: ANativeWindow*）
#[no_mangle]
pub extern "C" fn raptor_set_surface(
    handle: *mut RaptorHandle,
    native_window: i64,
    native_display: i64,
    width: u32,
    height: u32,
) -> c_int {
    cabi!("raptor_set_surface", ErrorCode::Internal as c_int, || {
        raptor_set_surface_impl(handle, native_window, native_display, width, height)
    })
}

/// 分离当前 Surface（Android onPause 时调用）
#[no_mangle]
pub extern "C" fn raptor_detach_surface(handle: *mut RaptorHandle) -> c_int {
    cabi!(
        "raptor_detach_surface",
        ErrorCode::Internal as c_int,
        || { raptor_detach_surface_impl(handle) }
    )
}

/// 调整 Surface 尺寸（屏幕旋转等场景）
#[no_mangle]
pub extern "C" fn raptor_resize_surface(
    handle: *mut RaptorHandle,
    width: u32,
    height: u32,
) -> c_int {
    cabi!(
        "raptor_resize_surface",
        ErrorCode::Internal as c_int,
        || { raptor_resize_surface_impl(handle, width, height) }
    )
}

/// 释放由 raptor 分配的字符串
#[no_mangle]
pub extern "C" fn raptor_free_string(s: *mut c_char) {
    cabi!("raptor_free_string", (), || { raptor_free_string_impl(s) })
}

/// 获取最近的错误信息（返回 JSON 字符串或 null）
#[no_mangle]
pub extern "C" fn raptor_last_error(handle: *mut RaptorHandle) -> *mut c_char {
    cabi!("raptor_last_error", std::ptr::null_mut(), || {
        raptor_last_error_impl(handle)
    })
}
