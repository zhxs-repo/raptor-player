//! ExternalRenderer — 外部 Surface 注入渲染器（Android / 嵌入式平台）
//!
//! 与 `WgpuRenderer`（自管窗口 + winit EventLoop）不同，`ExternalRenderer`
//! 接受外部传入的平台原生 Surface 句柄（如 Android `ANativeWindow*`），
//! 通过 wgpu 创建 GPU surface 进行渲染。
//!
//! 使用场景：
//! - Android: Flutter `SurfaceView` 提供 `ANativeWindow*`，通过 FFI 传给 Rust
//! - 嵌入式: 任何提供原生窗口句柄的平台
//!
//! 渲染线程独立运行，通过 channel 接收命令。Surface 可以在运行时 detach/reattach，
//! 支持 Android Activity 生命周期（onPause 时 Surface 销毁，onResume 时重建）。

use crate::clock::OverlayClock;
use crate::overlay::OverlayStack;
use crate::wgpu_renderer::SurfaceHandle;
use crate::yuv_pipeline::{interleave_uv_planes, setup_yuv_pipeline};
use raptor_core::Result;
use raptor_ffmpeg::{PixelFormat, VideoFrame};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use super::wgpu_renderer::VideoOutput;

/// 无 vsync 平台上的兜底渲染周期（60Hz）
const EXT_FALLBACK_FRAME_PERIOD: Duration = Duration::from_micros(16_667);
/// FIFO（vsync）模式下的轮询预算：节拍由 present 阻塞决定，此值只保证命令响应性
const EXT_VSYNC_POLL_BUDGET: Duration = Duration::from_millis(4);
/// pending 帧队列深度
const EXT_PENDING_CAPACITY: usize = 2;

// ─── Render thread commands ──────────────────────────────────────

enum ExtCmd {
    /// 替换 Overlay 栈
    SetOverlays(Vec<Box<dyn crate::overlay::Overlay>>),
    /// 分离 Surface（Surface 被销毁）— ack 回传"原生窗口引用已释放"
    DetachSurface(AckTx),
    /// 重新附加 Surface（Surface 重建）— ack 回传"surface 已就绪可上屏"
    ReattachSurface(SurfaceHandle, AckTx),
    /// 关闭渲染线程
    Shutdown,
}

type AckTx = mpsc::Sender<std::result::Result<(), String>>;

/// Surface 操作确认超时
///
/// 宿主（Android `surfaceDestroyed`）在 FFI 返回后会立即释放 `ANativeWindow`，
/// 因此 detach 必须等渲染线程真正 drop 掉 `wgpu::Surface` 才能返回成功；
/// 拿不到确认时必须报错，不能静默 Ok。
const SURFACE_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

fn wait_surface_ack(
    ack_rx: mpsc::Receiver<std::result::Result<(), String>>,
    what: &str,
    timeout: std::time::Duration,
) -> Result<()> {
    match ack_rx.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(raptor_core::RaptorError::Render(format!("{what}: {e}"))),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(raptor_core::RaptorError::Render(format!(
            "{what}: render thread did not acknowledge within {timeout:?}"
        ))),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(raptor_core::RaptorError::Render(
            format!("{what}: render thread exited before acknowledging"),
        )),
    }
}

// ─── Internal render state ───────────────────────────────────────

struct ExtRenderState {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// 当前 attached 的平台 Surface；detach 时必须在本线程同步 drop，
    /// 否则它会继续持有宿主已销毁的 `ANativeWindow`
    surface: Option<wgpu::Surface<'static>>,
    surface_config: Option<wgpu::SurfaceConfiguration>,
    surface_format: wgpu::TextureFormat,
    render_pipeline: wgpu::RenderPipeline,
    y_texture: wgpu::Texture,
    uv_texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    video_width: u32,
    video_height: u32,
    last_frame: Option<VideoFrame>,
    /// 已收到但尚未到上屏节拍的视频帧（按序，每节拍取一帧）
    pending_frames: VecDeque<VideoFrame>,
    overlay_stack: OverlayStack,
    render_frame_count: Arc<AtomicU64>,
    /// 叠加层挂钟时钟：pipeline 线程锚定，本线程每个节拍读取
    overlay_clock: Arc<OverlayClock>,
    /// 上一次 present 使用的媒体时间（秒），用于跳过无变化的重绘
    last_pts: f64,
    /// 目标渲染周期（仅在无 vsync 阻塞的软件节流路径下生效）
    frame_period: Duration,
    /// present 是否由 vsync 阻塞（FIFO 下上屏速率由显示器决定）
    vsync_locked: bool,
}

impl ExtRenderState {
    /// 从 SurfaceHandle 创建完整的渲染状态
    fn new(
        handle: &SurfaceHandle,
        render_frame_count: Arc<AtomicU64>,
        overlay_clock: Arc<OverlayClock>,
    ) -> std::result::Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: Default::default(),
            backend_options: Default::default(),
            display: None,
        });

        let surface = unsafe { Self::create_surface_from_handle(&instance, handle)? };

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .map_err(|_| "no suitable GPU adapter".to_string())?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("raptor_ext_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            experimental_features: Default::default(),
            memory_hints: Default::default(),
            trace: Default::default(),
        }))
        .map_err(|e| format!("device: {e}"))?;

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .first()
            .copied()
            .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
        let surface_config = Self::make_surface_config(&caps, handle);
        surface.configure(&device, &surface_config);

        // 使用 1x1 占位纹理初始化 pipeline（真正的视频尺寸在首帧时更新）
        let (render_pipeline, bind_group, y_texture, uv_texture) =
            setup_yuv_pipeline(&device, surface_format, 1, 1, "ext");

        let vsync_locked = surface_config.present_mode == wgpu::PresentMode::Fifo;

        tracing::info!(
            "ExtRenderState initialized: {}x{}, format={:?}, present={:?}",
            handle.width,
            handle.height,
            surface_format,
            surface_config.present_mode
        );

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            surface: Some(surface),
            surface_config: Some(surface_config),
            surface_format,
            render_pipeline,
            y_texture,
            uv_texture,
            bind_group,
            video_width: 0,
            video_height: 0,
            last_frame: None,
            pending_frames: VecDeque::with_capacity(EXT_PENDING_CAPACITY),
            overlay_stack: OverlayStack::new(),
            render_frame_count,
            overlay_clock,
            last_pts: 0.0,
            frame_period: EXT_FALLBACK_FRAME_PERIOD,
            vsync_locked,
        })
    }

    fn make_surface_config(
        caps: &wgpu::SurfaceCapabilities,
        handle: &SurfaceHandle,
    ) -> wgpu::SurfaceConfiguration {
        // 优先 FIFO：present 阻塞到 vblank，叠加层动画自然对齐显示器刷新率
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
            wgpu::PresentMode::Fifo
        } else {
            wgpu::PresentMode::Mailbox
        };
        wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: caps
                .formats
                .first()
                .copied()
                .unwrap_or(wgpu::TextureFormat::Bgra8Unorm),
            width: handle.width.max(1),
            height: handle.height.max(1),
            present_mode,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            // 留 1 个额外在飞帧，让下一帧的编码/提交与本帧扫描输出重叠，
            // FIFO 锁定下不会引入撕裂
            desired_maximum_frame_latency: 2,
        }
    }

    /// 释放 Surface — 在渲染线程上同步 drop，归还平台原生窗口引用
    ///
    /// 先等待在途提交结束，避免 swapchain 纹理仍被 GPU 引用时窗口已销毁。
    fn detach_surface(&mut self) {
        if self.surface.is_none() {
            return;
        }
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_millis(500)),
        });
        self.surface = None;
        self.surface_config = None;
        self.last_frame = None;
    }

    /// 附加新的原生 Surface — 复用 instance/adapter/device，仅重建 surface
    fn attach_surface(&mut self, handle: &SurfaceHandle) -> std::result::Result<(), String> {
        // 旧 surface 必须先归还，避免同时持有两个原生窗口引用
        self.detach_surface();

        let surface = unsafe { Self::create_surface_from_handle(&self.instance, handle)? };
        let caps = surface.get_capabilities(&self.adapter);
        if caps.formats.is_empty() {
            return Err("new surface is not compatible with the existing adapter".to_string());
        }
        let config = Self::make_surface_config(&caps, handle);
        surface.configure(&self.device, &config);

        if config.format != self.surface_format {
            self.surface_format = config.format;
            // pipeline 与纹理尺寸相关：置 0 让下一帧按新 format 重建
            self.video_width = 0;
            self.video_height = 0;
        }

        self.surface = Some(surface);
        self.surface_config = Some(config);
        Ok(())
    }

    fn reconfigure_surface(&mut self) {
        if let (Some(surface), Some(config)) = (&self.surface, &self.surface_config) {
            surface.configure(&self.device, config);
        }
    }

    /// 从平台原生 SurfaceHandle 创建 wgpu Surface
    ///
    /// # Safety
    /// `handle.native_window` 必须指向有效的平台原生窗口对象。
    unsafe fn create_surface_from_handle(
        instance: &wgpu::Instance,
        handle: &SurfaceHandle,
    ) -> std::result::Result<wgpu::Surface<'static>, String> {
        use raw_window_handle::{
            AndroidDisplayHandle, AndroidNdkWindowHandle, RawDisplayHandle, RawWindowHandle,
        };
        use std::ptr::NonNull;

        let ptr = NonNull::new(handle.native_window as *mut std::ffi::c_void)
            .ok_or_else(|| "native_window pointer is null".to_string())?;
        let raw_window = RawWindowHandle::AndroidNdk(AndroidNdkWindowHandle::new(ptr));
        let raw_display = RawDisplayHandle::Android(AndroidDisplayHandle::new());

        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(raw_display),
            raw_window_handle: raw_window,
        };

        instance
            .create_surface_unsafe(target)
            .map_err(|e| format!("create surface: {e}"))
    }

    /// 上传视频帧纹理 + 计算 viewport + 渲染 + present
    fn render_frame(&mut self, frame: &VideoFrame) {
        // Surface 已 detach：没有原生窗口可写
        if self.surface.is_none() {
            return;
        }

        // 视频分辨率变化 → 重建纹理
        if frame.width != self.video_width || frame.height != self.video_height {
            self.video_width = frame.width;
            self.video_height = frame.height;
            let (pipeline, bg, yt, uvt) = setup_yuv_pipeline(
                &self.device,
                self.surface_format,
                frame.width,
                frame.height,
                "ext",
            );
            self.render_pipeline = pipeline;
            self.bind_group = bg;
            self.y_texture = yt;
            self.uv_texture = uvt;
        }

        // 上传 Y 平面
        if let Some(y_plane) = frame.planes.first() {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.y_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &y_plane.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(y_plane.stride as u32),
                    rows_per_image: Some(frame.height),
                },
                wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
            );
        }

        // 上传 UV 平面
        match frame.format {
            PixelFormat::Nv12 => {
                if let Some(uv_plane) = frame.planes.get(1) {
                    self.queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &self.uv_texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        &uv_plane.data,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(uv_plane.stride as u32),
                            rows_per_image: Some(frame.height / 2),
                        },
                        wgpu::Extent3d {
                            width: frame.width / 2,
                            height: frame.height / 2,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
            _ => {
                let (u_data, u_stride) = frame
                    .planes
                    .get(1)
                    .map(|p| (p.data.as_slice(), p.stride))
                    .unwrap_or((&[], 0));
                let (v_data, v_stride) = frame
                    .planes
                    .get(2)
                    .map(|p| (p.data.as_slice(), p.stride))
                    .unwrap_or((&[], 0));
                let uv_w = (frame.width / 2) as usize;
                let uv_h = (frame.height / 2) as usize;
                let uv_interleaved =
                    interleave_uv_planes(u_data, u_stride, v_data, v_stride, uv_w, uv_h);
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.uv_texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &uv_interleaved,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some((uv_w * 2) as u32),
                        rows_per_image: Some(frame.height / 2),
                    },
                    wgpu::Extent3d {
                        width: frame.width / 2,
                        height: frame.height / 2,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }

        // 叠加层时间以挂钟为准，在两个视频帧之间也连续推进；
        // 无时间戳的帧沿用上一次 present 的时间，避免把叠加层时钟重置为 0
        let overlay_pts = frame.pts_secs().unwrap_or(self.last_pts);
        self.present_frame(self.overlay_clock.now_pts().max(overlay_pts));
    }

    /// 收下 pipeline 提交的视频帧，等下一个节拍再上屏
    fn enqueue_frame(&mut self, frame: VideoFrame) {
        // 队列满说明一个节拍内到了多帧：丢最旧的一帧，保证上屏顺序单调推进
        if self.pending_frames.len() == EXT_PENDING_CAPACITY {
            self.pending_frames.pop_front();
        }
        self.pending_frames.push_back(frame);
    }

    /// 一个渲染节拍：有新视频帧就上传上屏，否则只在叠加层时间推进时重绘
    fn render_tick(&mut self) {
        if self.surface.is_none() {
            return;
        }
        if let Some(frame) = self.pending_frames.pop_front() {
            self.last_frame = Some(frame.clone());
            self.render_frame(&frame);
            return;
        }
        // 叠加层不推进（无叠加层/暂停/帧流停滞）时不重复 present，省下空转的 GPU 与 CPU
        if self.last_frame.is_none()
            || self.overlay_stack.is_empty()
            || !self.overlay_clock.is_live()
        {
            return;
        }
        let pts = self.overlay_clock.now_pts();
        if pts != self.last_pts {
            self.present_frame(pts);
        }
    }

    /// 获取 surface 纹理 → video pass + overlay pass → present
    fn present_frame(&mut self, pts: f64) {
        let Some(surface) = self.surface.as_ref() else {
            // Surface 已 detach：没有原生窗口可写
            return;
        };
        let surface_texture = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.reconfigure_surface();
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                tracing::error!("ExternalRenderer: GPU surface lost");
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => {
                tracing::warn!("ExternalRenderer: surface texture unavailable");
                return;
            }
        };

        // 计算 letterbox / pillarbox viewport
        let surf_w = surface_texture.texture.width() as f32;
        let surf_h = surface_texture.texture.height() as f32;
        let video_w = self.video_width as f32;
        let video_h = self.video_height as f32;
        let viewport = if video_w > 0.0 && video_h > 0.0 {
            let scale = (surf_w / video_w).min(surf_h / video_h);
            let vp_w = (video_w * scale).round();
            let vp_h = (video_h * scale).round();
            let vp_x = ((surf_w - vp_w) / 2.0).round();
            let vp_y = ((surf_h - vp_h) / 2.0).round();
            (vp_x, vp_y, vp_w, vp_h)
        } else {
            (0.0, 0.0, surf_w, surf_h)
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ext_yuv_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            pass.set_pipeline(&self.render_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_viewport(viewport.0, viewport.1, viewport.2, viewport.3, 0.0, 1.0);
            pass.draw(0..4, 0..1);
        }

        // Overlay 叠加层渲染
        self.overlay_stack.update_all(pts);
        if !self.overlay_stack.is_empty() {
            self.overlay_stack.render_all(
                &self.device,
                &self.queue,
                &mut encoder,
                &view,
                self.surface_format,
                surface_texture.texture.width(),
                surface_texture.texture.height(),
            );
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        surface_texture.present();
        self.last_pts = pts;
        self.render_frame_count.fetch_add(1, Ordering::Relaxed);
    }
}

// ─── Render thread loop ──────────────────────────────────────────

fn ext_render_loop(
    frame_rx: mpsc::Receiver<VideoFrame>,
    ctrl_rx: mpsc::Receiver<ExtCmd>,
    ready_tx: mpsc::Sender<std::result::Result<(), String>>,
    initial_handle: SurfaceHandle,
    render_frame_count: Arc<AtomicU64>,
    overlay_clock: Arc<OverlayClock>,
    surface_attached: Arc<AtomicBool>,
) {
    // 创建初始渲染状态
    let mut state = match ExtRenderState::new(&initial_handle, render_frame_count, overlay_clock) {
        Ok(s) => {
            let _ = ready_tx.send(Ok(()));
            s
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e.clone()));
            tracing::error!("ExtRenderState init failed: {e}");
            return;
        }
    };

    surface_attached.store(true, Ordering::Release);
    tracing::info!("ExternalRenderer: render thread started");

    let mut next_tick = Instant::now();
    'outer: loop {
        // 控制命令优先于视频帧处理：detach 必须抢在积压帧之前生效，
        // 否则会在宿主即将销毁的原生窗口上继续上屏
        loop {
            match ctrl_rx.try_recv() {
                Ok(ExtCmd::SetOverlays(overlays)) => {
                    state.overlay_stack = OverlayStack::new();
                    for overlay in overlays {
                        state.overlay_stack.push(overlay);
                    }
                    tracing::info!(
                        "ExternalRenderer: overlay stack updated ({} overlays)",
                        state.overlay_stack.len()
                    );
                    // 叠加层内容变了但挂钟可能没推进（如暂停中加载弹幕）：立即重绘一次
                    if state.last_frame.is_some() && state.surface.is_some() {
                        let pts = state.overlay_clock.now_pts().max(state.last_pts);
                        state.present_frame(pts);
                    }
                }
                Ok(ExtCmd::DetachSurface(ack)) => {
                    tracing::info!("ExternalRenderer: detaching surface");
                    state.detach_surface();
                    surface_attached.store(false, Ordering::Release);
                    let _ = ack.send(Ok(()));
                }
                Ok(ExtCmd::ReattachSurface(handle, ack)) => {
                    tracing::info!(
                        "ExternalRenderer: reattaching surface {}x{}",
                        handle.width,
                        handle.height
                    );
                    let result = state.attach_surface(&handle).map(|()| {
                        surface_attached.store(true, Ordering::Release);
                    });
                    if let Err(e) = &result {
                        tracing::error!("ExternalRenderer: reattach failed: {e}");
                    }
                    let _ = ack.send(result);
                }
                Ok(ExtCmd::Shutdown) => {
                    tracing::info!("ExternalRenderer: shutdown requested");
                    break 'outer;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    tracing::info!("ExternalRenderer: control channel closed");
                    break 'outer;
                }
            }
        }

        // FIFO 下 present 自身阻塞到下一个 vblank，节拍由显示器决定；
        // 无 vsync 的后端按 60Hz 兜底做软件节流
        let wait = if state.vsync_locked {
            EXT_VSYNC_POLL_BUDGET
        } else {
            next_tick.saturating_duration_since(Instant::now())
        };
        match frame_rx.recv_timeout(wait) {
            Ok(frame) => state.enqueue_frame(frame),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                tracing::info!("ExternalRenderer: frame channel closed");
                break;
            }
        }

        // 命令在节拍到期前到达时不占用本节拍，但 pending 已满时必须让出一次上屏，
        // 否则片源帧率高于刷新率会把渲染饿死
        if !state.vsync_locked
            && Instant::now() < next_tick
            && state.pending_frames.len() < EXT_PENDING_CAPACITY
        {
            continue;
        }

        state.render_tick();

        if !state.vsync_locked {
            next_tick += state.frame_period;
            let now = Instant::now();
            if next_tick < now {
                next_tick = now;
            }
        }
    }

    // 线程退出前归还原生窗口引用，避免 Surface 晚于宿主窗口销毁
    state.detach_surface();
    surface_attached.store(false, Ordering::Release);
    tracing::info!("ExternalRenderer: render thread exiting");
}

// ─── Public API ──────────────────────────────────────────────────

/// ExternalRenderer — 外部 Surface 注入渲染器
///
/// 用于 Android 等嵌入式平台，宿主应用（Flutter）提供原生 Surface，
/// ExternalRenderer 在其上创建 wgpu surface 进行视频渲染。
///
/// 与 `WgpuRenderer` 的区别：
/// - 不创建自己的窗口（无 winit 依赖）
/// - Surface 可在运行时 detach/reattach（支持 Activity 生命周期）
/// - 使用 `SurfaceHandle` 而非 winit Window
pub struct ExternalRenderer {
    width: u32,
    height: u32,
    /// 视频帧通道（bounded，满时丢帧做背压）
    frame_tx: Option<mpsc::SyncSender<VideoFrame>>,
    /// 控制通道（unbounded：detach/reattach 必须能立即入队，不被帧背压阻塞）
    ctrl_tx: Option<mpsc::Sender<ExtCmd>>,
    /// 渲染线程句柄，drop 时 join 以确保 Surface 在自身销毁前被释放
    render_thread: Option<std::thread::JoinHandle<()>>,
    /// 当前已 attach 的原生窗口指针，用于识别对同一窗口的重复 attach
    attached_window: Option<u64>,
    /// 当前是否有有效的 Surface attached
    surface_attached: Arc<AtomicBool>,
    /// 渲染线程累计帧数
    render_frame_count: Arc<AtomicU64>,
    /// 叠加层挂钟时钟：本结构按上屏帧锚定，渲染线程按挂钟外推
    overlay_clock: Arc<OverlayClock>,
    initialized: bool,
}

unsafe impl Send for ExternalRenderer {}

impl ExternalRenderer {
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            frame_tx: None,
            ctrl_tx: None,
            render_thread: None,
            attached_window: None,
            surface_attached: Arc::new(AtomicBool::new(false)),
            render_frame_count: Arc::new(AtomicU64::new(0)),
            overlay_clock: Arc::new(OverlayClock::new()),
            initialized: false,
        }
    }

    /// 使用指定的 SurfaceHandle 初始化渲染器
    ///
    /// 创建渲染线程并等待 GPU 资源就绪。
    pub fn init_with_surface(&mut self, handle: SurfaceHandle) -> Result<()> {
        tracing::info!(
            "ExternalRenderer::init_with_surface({}x{}, native_window=0x{:x})",
            handle.width,
            handle.height,
            handle.native_window
        );

        self.width = handle.width;
        self.height = handle.height;
        let native_window = handle.native_window;

        let (frame_tx, frame_rx) = mpsc::sync_channel::<VideoFrame>(4);
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<ExtCmd>();
        let (ready_tx, ready_rx) = mpsc::channel();
        let surface_attached = Arc::clone(&self.surface_attached);
        let frame_counter = Arc::clone(&self.render_frame_count);
        let overlay_clock = Arc::clone(&self.overlay_clock);

        let thread = std::thread::Builder::new()
            .name("raptor-ext-render".into())
            .spawn(move || {
                ext_render_loop(
                    frame_rx,
                    ctrl_rx,
                    ready_tx,
                    handle,
                    frame_counter,
                    overlay_clock,
                    surface_attached,
                );
            })
            .map_err(|e| {
                raptor_core::RaptorError::Internal(format!("spawn ext render thread: {e}"))
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.frame_tx = Some(frame_tx);
                self.ctrl_tx = Some(ctrl_tx);
                self.render_thread = Some(thread);
                self.attached_window = Some(native_window);
                self.initialized = true;
                tracing::info!("ExternalRenderer ready: {}x{}", self.width, self.height);
                Ok(())
            }
            Ok(Err(e)) => Err(raptor_core::RaptorError::Render(format!(
                "ext render init: {e}"
            ))),
            Err(_) => Err(raptor_core::RaptorError::Internal(
                "ext render thread failed to start".into(),
            )),
        }
    }

    /// 设置 Overlay 叠加层（字幕、弹幕等）
    pub fn set_overlays(&self, overlays: Vec<Box<dyn crate::overlay::Overlay>>) {
        if let Some(tx) = &self.ctrl_tx {
            if let Err(e) = tx.send(ExtCmd::SetOverlays(overlays)) {
                tracing::error!("ExternalRenderer::set_overlays FAILED: {}", e);
            }
        } else {
            tracing::warn!("ExternalRenderer::set_overlays: not initialized, overlays dropped");
        }
    }
}

impl Default for ExternalRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoOutput for ExternalRenderer {
    fn init(&mut self, width: u32, height: u32) -> Result<()> {
        // ExternalRenderer 需要通过 init_with_surface 初始化
        // 这里仅保存尺寸，真正的初始化延迟到 reattach_surface
        self.width = width;
        self.height = height;
        tracing::info!(
            "ExternalRenderer::init({}x{}) — awaiting surface",
            width,
            height
        );
        Ok(())
    }

    fn submit_frame(&mut self, frame: &VideoFrame) -> Result<()> {
        if !self.initialized || !self.surface_attached.load(Ordering::Acquire) {
            return Ok(());
        }
        // 以本帧 PTS 重新锚定叠加层挂钟（无时间戳帧不覆盖，避免时钟被重置为 0）
        if let Some(secs) = frame.pts_secs() {
            self.overlay_clock.reanchor(secs);
        }
        if let Some(tx) = &self.frame_tx {
            match tx.try_send(frame.clone()) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => {
                    tracing::debug!("ExternalRenderer: frame channel full, dropping frame");
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    self.surface_attached.store(false, Ordering::Release);
                }
            }
        }
        Ok(())
    }

    fn set_size(&mut self, width: u32, height: u32) -> Result<()> {
        self.width = width;
        self.height = height;
        Ok(())
    }

    fn should_stop(&self) -> bool {
        // ExternalRenderer 不会因为窗口关闭而停止
        false
    }

    fn poll(&mut self) {
        // 渲染线程自行循环，无需外部泵
    }

    fn freeze_overlay_clock(&self) {
        self.overlay_clock.freeze();
    }

    fn render_frame_count(&self) -> u64 {
        self.render_frame_count.load(Ordering::Relaxed)
    }

    /// 分离 Surface
    ///
    /// 同步语义：返回 Ok 表示渲染线程已经把 `wgpu::Surface`（及其持有的
    /// `ANativeWindow` 引用）释放完毕，宿主可以安全销毁原生窗口。
    fn detach_surface(&mut self) -> Result<()> {
        tracing::info!("ExternalRenderer::detach_surface");
        let Some(tx) = &self.ctrl_tx else {
            // 未初始化：没有 Surface 需要释放
            return Ok(());
        };
        let (ack_tx, ack_rx) = mpsc::channel();
        tx.send(ExtCmd::DetachSurface(ack_tx)).map_err(|_| {
            self.surface_attached.store(false, Ordering::Release);
            raptor_core::RaptorError::Render("detach surface: render thread exited".into())
        })?;
        let result = wait_surface_ack(
            ack_rx,
            "ExternalRenderer::detach_surface",
            SURFACE_ACK_TIMEOUT,
        );
        // 无论是否拿到确认都不保留窗口指针：确认失败时宿主照样已销毁窗口，
        // 留着它会让对同一指针的 reattach 被短路成"已就绪"
        self.attached_window = None;
        result
    }

    /// 重新附加 Surface
    ///
    /// 返回 Ok 表示新 surface 已 configure 完成、可以上屏（而非仅"命令已入队"）。
    fn reattach_surface(&mut self, handle: SurfaceHandle) -> Result<()> {
        tracing::info!(
            "ExternalRenderer::reattach_surface({}x{}, native_window=0x{:x})",
            handle.width,
            handle.height,
            handle.native_window
        );

        self.width = handle.width;
        self.height = handle.height;

        if !self.initialized {
            // 首次 reattach 等价于 init_with_surface
            return self.init_with_surface(handle);
        }

        // 同一原生窗口重复 attach（set_surface 路径已先调用 init_with_surface）：
        // 直接视为就绪，避免在渲染线程上无谓地归还并重建同一个 Surface
        if self.attached_window == Some(handle.native_window)
            && self.surface_attached.load(Ordering::Acquire)
        {
            tracing::debug!(
                "ExternalRenderer::reattach_surface: window 0x{:x} already attached",
                handle.native_window
            );
            return Ok(());
        }

        let Some(tx) = &self.ctrl_tx else {
            return Err(raptor_core::RaptorError::Render(
                "reattach surface: renderer not initialized".into(),
            ));
        };
        let (ack_tx, ack_rx) = mpsc::channel();
        tx.send(ExtCmd::ReattachSurface(handle, ack_tx))
            .map_err(|_| {
                raptor_core::RaptorError::Render("reattach surface: render thread exited".into())
            })?;
        let result = wait_surface_ack(
            ack_rx,
            "ExternalRenderer::reattach_surface",
            SURFACE_ACK_TIMEOUT,
        );
        if result.is_ok() {
            self.attached_window = Some(handle.native_window);
        }
        result
    }

    fn set_overlays(&mut self, overlays: Vec<Box<dyn crate::overlay::Overlay>>) {
        if let Some(tx) = &self.ctrl_tx {
            if let Err(e) = tx.send(ExtCmd::SetOverlays(overlays)) {
                tracing::error!("VideoOutput::set_overlays FAILED: {}", e);
            }
        } else {
            tracing::warn!("VideoOutput::set_overlays: not initialized, overlays dropped");
        }
    }
}

impl Drop for ExternalRenderer {
    fn drop(&mut self) {
        tracing::info!("ExternalRenderer::drop");
        // 先请求释放 Surface，再关闭线程：线程退出路径也会兜底 detach，
        // 但显式确认可以保证 join 返回前原生窗口引用已归还
        if let Some(tx) = self.ctrl_tx.take() {
            let (ack_tx, ack_rx) = mpsc::channel();
            if tx.send(ExtCmd::DetachSurface(ack_tx)).is_ok() {
                let _ = ack_rx.recv_timeout(SURFACE_ACK_TIMEOUT);
            }
            let _ = tx.send(ExtCmd::Shutdown);
        }
        self.frame_tx = None;
        if let Some(thread) = self.render_thread.take() {
            // 渲染线程持有 Surface，必须 join 到它真正退出
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_surface_handle_new() {
        let h = SurfaceHandle::new(0x1234, 0, 1920, 1080);
        assert_eq!(h.native_window, 0x1234);
        assert_eq!(h.native_display, 0);
        assert_eq!(h.width, 1920);
        assert_eq!(h.height, 1080);
    }

    #[test]
    fn test_external_renderer_new() {
        let r = ExternalRenderer::new();
        assert_eq!(r.width, 0);
        assert_eq!(r.height, 0);
        assert!(!r.initialized);
        assert!(!r.surface_attached.load(Ordering::Relaxed));
    }

    #[test]
    fn test_external_renderer_default() {
        let r = ExternalRenderer::default();
        assert_eq!(r.width, 0);
    }

    #[test]
    fn test_detach_without_init() {
        let mut r = ExternalRenderer::new();
        // detach_surface 在未初始化时不应 panic
        assert!(r.detach_surface().is_ok());
    }

    /// 回归：渲染线程已消失时 detach 不得静默返回 Ok
    ///
    /// 宿主（Android surfaceDestroyed）依据返回值决定是否释放 ANativeWindow，
    /// 假成功会让渲染线程继续操作已释放的原生窗口。
    #[test]
    fn test_detach_reports_error_when_render_thread_gone() {
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<ExtCmd>();
        drop(ctrl_rx);
        let mut r = ExternalRenderer::new();
        r.ctrl_tx = Some(ctrl_tx);
        r.initialized = true;
        let err = r.detach_surface().unwrap_err();
        assert!(
            matches!(err, raptor_core::RaptorError::Render(_)),
            "expected Render error, got {err}"
        );
    }

    #[test]
    fn test_reattach_reports_error_when_render_thread_gone() {
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<ExtCmd>();
        drop(ctrl_rx);
        let mut r = ExternalRenderer::new();
        r.ctrl_tx = Some(ctrl_tx);
        r.initialized = true;
        let err = r
            .reattach_surface(SurfaceHandle::new(0x1234, 0, 1920, 1080))
            .unwrap_err();
        assert!(
            matches!(err, raptor_core::RaptorError::Render(_)),
            "expected Render error, got {err}"
        );
    }

    #[test]
    fn test_reattach_same_window_is_noop() {
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<ExtCmd>();
        drop(ctrl_rx); // 即使渲染线程不存在，同一窗口的重复 attach 也不应重建 Surface
        let mut r = ExternalRenderer::new();
        r.ctrl_tx = Some(ctrl_tx);
        r.initialized = true;
        r.attached_window = Some(0x1234);
        r.surface_attached.store(true, Ordering::Release);
        assert!(r
            .reattach_surface(SurfaceHandle::new(0x1234, 0, 1920, 1080))
            .is_ok());
    }

    /// ack 协议三种结局：确认成功 / 通道关闭 / 超时未确认，后两者都必须报错
    #[test]
    fn test_surface_ack_requires_confirmation() {
        let timeout = std::time::Duration::from_millis(100);

        let (ack_tx, ack_rx) = mpsc::channel();
        ack_tx.send(Ok(())).unwrap();
        assert!(wait_surface_ack(ack_rx, "ack_ok", timeout).is_ok());

        let (ack_tx, ack_rx) = mpsc::channel();
        ack_tx.send(Err("attach failed".into())).unwrap();
        assert!(wait_surface_ack(ack_rx, "ack_err", timeout).is_err());

        let (ack_tx, ack_rx) = mpsc::channel();
        drop(ack_tx);
        assert!(wait_surface_ack(ack_rx, "ack_closed", timeout).is_err());

        let (_ack_tx, ack_rx) = mpsc::channel();
        assert!(wait_surface_ack(ack_rx, "ack_timeout", timeout).is_err());
    }

    #[test]
    fn test_submit_frame_without_init() {
        let mut r = ExternalRenderer::new();
        let frame = raptor_ffmpeg::VideoFrame {
            width: 320,
            height: 240,
            format: raptor_ffmpeg::PixelFormat::Nv12,
            planes: vec![],
            pts: Some(0),
            time_base: raptor_ffmpeg::time_base(1, 90000),
        };
        // 未初始化时 submit_frame 应静默返回 Ok
        assert!(r.submit_frame(&frame).is_ok());
    }
}
