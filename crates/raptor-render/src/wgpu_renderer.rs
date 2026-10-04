use crate::clock::OverlayClock;
use crate::overlay::OverlayStack;
use crate::yuv_pipeline::{color_params_bytes, interleave_uv_planes, setup_yuv_pipeline};
use raptor_core::Result;
use raptor_ffmpeg::{PixelFormat, VideoFrame};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use winit::event_loop::{EventLoop, EventLoopBuilder};
use winit::platform::pump_events::EventLoopExtPumpEvents;
#[cfg(target_os = "windows")]
use winit::platform::windows::EventLoopBuilderExtWindows;
use winit::window::WindowBuilder;

/// 拿不到显示器刷新率时的兜底渲染周期（60Hz）
const FALLBACK_FRAME_PERIOD: Duration = Duration::from_micros(16_667);
/// FIFO（vsync）模式下的轮询预算：上屏节拍由 present 阻塞自然对齐刷新率，
/// 这里只需保证命令与窗口事件的响应性
const VSYNC_POLL_BUDGET: Duration = Duration::from_millis(4);
/// pending 帧队列深度：够吸收一次节拍错相，又不引入额外延迟
const PENDING_FRAME_CAPACITY: usize = 2;
/// 刷新率探测间隔（窗口可能被拖到另一块显示器上）
const REFRESH_PROBE_INTERVAL: Duration = Duration::from_secs(1);

/// HUD 播放统计 — 用于窗口标题栏显示
#[derive(Debug, Clone, Default)]
pub struct HudStats {
    pub paused: bool,
    pub video_codec: String,
    pub audio_codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub rendered_frames: u64,
    pub dropped_frames: u64,
    pub subtitle_on: bool,
    pub danmaku_on: bool,
    pub danmaku_count: u32,
}

/// 平台原生 Surface 句柄 — 用于外部 Surface 注入（Android / 嵌入式）
///
/// FFI 层将平台原生窗口指针封装为此类型，供 `ExternalRenderer` 创建 wgpu Surface。
/// 各平台含义：
/// - Android: `native_window` = `ANativeWindow*`
/// - 其他平台可扩展（如 WaylandSurface、XlibWindow 等）
#[derive(Debug, Clone, Copy)]
pub struct SurfaceHandle {
    /// 平台原生窗口指针（Android: `ANativeWindow*`）
    pub native_window: u64,
    /// 平台原生显示连接（Android 不使用，保留为 0）
    pub native_display: u64,
    /// Surface 宽度（像素）
    pub width: u32,
    /// Surface 高度（像素）
    pub height: u32,
}

impl SurfaceHandle {
    pub fn new(native_window: u64, native_display: u64, width: u32, height: u32) -> Self {
        Self {
            native_window,
            native_display,
            width,
            height,
        }
    }
}

/// VideoOutput trait — 视频输出抽象
pub trait VideoOutput: Send {
    fn init(&mut self, width: u32, height: u32) -> Result<()>;
    fn submit_frame(&mut self, frame: &VideoFrame) -> Result<()>;
    fn set_size(&mut self, width: u32, height: u32) -> Result<()>;
    fn texture_id(&self) -> Option<i64> {
        None
    }
    fn supports_native_hw_frame(&self) -> bool {
        false
    }
    fn should_stop(&self) -> bool {
        false
    }
    fn poll(&mut self) {}
    /// 获取渲染线程累计渲染帧总数（用于计算实时 FPS）
    fn render_frame_count(&self) -> u64 {
        0
    }
    /// 更新窗口标题（用于 HUD 信息显示）
    fn set_title(&mut self, _title: &str) {}

    /// 冻结叠加层挂钟（暂停/EOF 时调用）
    ///
    /// 渲染线程会在两个视频帧之间按挂钟外推 Overlay 时间，暂停时必须冻结，
    /// 否则画面静止而弹幕继续滑动。
    fn freeze_overlay_clock(&self) {}

    /// 设置 Overlay 叠加层（字幕、弹幕等）
    ///
    /// 默认实现为 no-op，具体实现在各 Renderer 中。
    fn set_overlays(&mut self, _overlays: Vec<Box<dyn crate::overlay::Overlay>>) {}

    // === Surface 生命周期（Android / 嵌入式平台） ===

    /// 分离当前 Surface（Surface 被销毁时调用，如 Android onPause）
    ///
    /// 渲染线程将暂停上屏，但保持解码状态不变。
    fn detach_surface(&mut self) -> Result<()> {
        Ok(())
    }

    /// 重新附加 Surface（Surface 重建时调用，如 Android onResume）
    ///
    /// 渲染线程使用新的 SurfaceHandle 重建 wgpu Surface 并恢复上屏。
    fn reattach_surface(&mut self, _handle: SurfaceHandle) -> Result<()> {
        Ok(())
    }
}

enum WindowCmd {
    Frame(VideoFrame),
    SetOverlays(Vec<Box<dyn crate::overlay::Overlay>>),
    SetTitle(String),
    Shutdown,
}

struct WindowRenderer {
    width: u32,
    height: u32,
    event_loop: EventLoop<()>,
    #[allow(dead_code)]
    window: winit::window::Window,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    render_pipeline: wgpu::RenderPipeline,
    y_texture: wgpu::Texture,
    uv_texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    /// 色彩参数 uniform（矩阵 + 量程），每个视频帧更新一次
    color_params: wgpu::Buffer,
    window_size: (u32, u32),
    last_frame: Option<VideoFrame>,
    /// 已收到但尚未到上屏节拍的视频帧（按序，每节拍取一帧）
    pending_frames: VecDeque<VideoFrame>,
    window_should_close: bool,
    title: String,
    /// 共享标志：窗口关闭时设置为 true，供 WgpuRenderer 侧读取
    window_closed_flag: Arc<AtomicBool>,
    /// 累计渲染帧数（供 WgpuRenderer 侧读取计算 FPS）
    render_frame_count: Arc<AtomicU64>,
    /// 叠加层挂钟时钟：pipeline 线程锚定，窗口线程每个节拍读取
    overlay_clock: Arc<OverlayClock>,
    /// 上一次 present 使用的媒体时间（秒），用于跳过无变化的重绘
    last_pts: f64,
    /// 目标渲染周期（仅在无 vsync 阻塞的软件节流路径下生效）
    frame_period: Duration,
    /// present 是否由 vsync 阻塞（FIFO 下上屏速率由显示器决定）
    vsync_locked: bool,
    /// 上次探测显示器刷新率的时刻
    refresh_probe_at: Instant,
    /// Overlay 合成栈（字幕、弹幕等叠加层）
    overlay_stack: OverlayStack,
}

impl WindowRenderer {
    /// `closed_flag` 用于在窗口关闭时通知外部（WgpuRenderer 侧）。
    fn new(
        width: u32,
        height: u32,
        closed_flag: Arc<AtomicBool>,
        frame_counter: Arc<AtomicU64>,
        overlay_clock: Arc<OverlayClock>,
    ) -> std::result::Result<Self, String> {
        let mut builder = EventLoopBuilder::new();
        #[cfg(target_os = "windows")]
        builder.with_any_thread(true);
        let event_loop = builder.build().map_err(|e| format!("event loop: {e}"))?;
        let window = WindowBuilder::new()
            .with_title("Raptor Player")
            .with_inner_size(winit::dpi::LogicalSize::new(width, height))
            .build(&event_loop)
            .map_err(|e| format!("window: {e}"))?;

        let window_size = {
            let s = window.inner_size();
            (s.width.max(1), s.height.max(1))
        };

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: Default::default(),
            backend_options: Default::default(),
            display: None,
        });

        let surface = unsafe {
            wgpu::SurfaceTargetUnsafe::from_display_and_window(&window, &window)
                .map(|target| instance.create_surface_unsafe(target))
                .map_err(|e| format!("surface target: {e}"))?
        }
        .map_err(|e| format!("surface: {e}"))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .map_err(|_| "no suitable GPU adapter".to_string())?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("raptor_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
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

        // 优先 FIFO：把上屏节拍交给显示器（vsync 阻塞），叠加层动画与刷新率对齐且无撕裂。
        // 极少数不支持 FIFO 的后端回退 Mailbox，由本线程按刷新率做软件节流。
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
            wgpu::PresentMode::Fifo
        } else {
            wgpu::PresentMode::Mailbox
        };

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: window_size.0,
            height: window_size.1,
            present_mode,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            // 2 个在飞帧：FIFO 下仍然锁在 vblank，但允许下一帧的上传/绘制与
            // 当前帧的扫描输出重叠，避免单帧偶发抖动直接掉到半刷新率
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        let (render_pipeline, bind_group, y_texture, uv_texture, color_params) =
            setup_yuv_pipeline(&device, surface_format, width, height, "");

        let frame_period = Self::probe_frame_period(&window);
        let vsync_locked = present_mode == wgpu::PresentMode::Fifo;

        tracing::info!(
            "WindowRenderer initialized: {}x{}, format={:?}, present={:?}, target {:.1}fps",
            width,
            height,
            surface_format,
            present_mode,
            1.0 / frame_period.as_secs_f64()
        );

        Ok(Self {
            width,
            height,
            event_loop,
            window,
            device,
            queue,
            surface,
            surface_config,
            render_pipeline,
            y_texture,
            uv_texture,
            bind_group,
            color_params,
            window_size,
            last_frame: None,
            pending_frames: VecDeque::with_capacity(PENDING_FRAME_CAPACITY),
            window_should_close: false,
            title: "Raptor Player".to_string(),
            window_closed_flag: closed_flag,
            render_frame_count: frame_counter,
            overlay_clock,
            last_pts: 0.0,
            frame_period,
            vsync_locked,
            refresh_probe_at: Instant::now(),
            overlay_stack: OverlayStack::new(),
        })
    }

    /// 目标渲染周期：跟随当前显示器的刷新率，读不到时按 60Hz 兜底
    fn probe_frame_period(window: &winit::window::Window) -> Duration {
        window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz())
            .filter(|mhz| *mhz > 0)
            .map(|mhz| Duration::from_secs_f64(1000.0 / mhz as f64))
            .unwrap_or(FALLBACK_FRAME_PERIOD)
    }

    fn pump_events(&mut self) {
        let mut should_close = false;
        let mut new_size: Option<(u32, u32)> = None;
        // 非阻塞取事件：节拍由 run() 的 recv_timeout 提供，这里绝不等待，
        // 否则每个渲染周期都要白等数毫秒，把上屏速率压到刷新率以下
        let _ = self
            .event_loop
            .pump_events(Some(Duration::ZERO), |event, _| {
                if let winit::event::Event::WindowEvent { event, .. } = event {
                    match event {
                        winit::event::WindowEvent::CloseRequested => {
                            should_close = true;
                        }
                        winit::event::WindowEvent::Resized(size) => {
                            new_size = Some((size.width.max(1), size.height.max(1)));
                        }
                        _ => {}
                    }
                }
            });
        if should_close {
            self.window_should_close = true;
        }
        if self.title != "Raptor Player" {
            self.window.set_title(&self.title);
        }
        // 窗口可能被拖到另一块显示器上：定期重新对齐刷新率
        if self.refresh_probe_at.elapsed() >= REFRESH_PROBE_INTERVAL {
            self.refresh_probe_at = Instant::now();
            let period = Self::probe_frame_period(&self.window);
            if period != self.frame_period {
                tracing::info!(
                    "WindowRenderer: refresh target changed to {:.1}fps",
                    1.0 / period.as_secs_f64()
                );
                self.frame_period = period;
            }
        }
        if let Some(size) = new_size {
            self.window_size = size;
            self.surface_config.width = size.0;
            self.surface_config.height = size.1;
            self.surface.configure(&self.device, &self.surface_config);
            if let Some(frame) = self.last_frame.clone() {
                self.render_frame_inner(&frame);
            }
        }
    }

    /// 实际渲染逻辑：上传纹理 + 计算 viewport + 绘制
    fn render_frame_inner(&mut self, frame: &VideoFrame) {
        // 视频分辨率变化 → 重建纹理（不影响 surface 尺寸）
        if frame.width != self.width || frame.height != self.height {
            self.width = frame.width;
            self.height = frame.height;
            let (pipeline, bg, yt, uvt, cp) = setup_yuv_pipeline(
                &self.device,
                self.surface_config.format,
                frame.width,
                frame.height,
                "",
            );
            self.render_pipeline = pipeline;
            self.bind_group = bg;
            self.y_texture = yt;
            self.uv_texture = uvt;
            self.color_params = cp;
        }
        // 矩阵系数与量程写错是画质错误而不是抖动，所以每帧都按帧上的元数据更新
        self.queue
            .write_buffer(&self.color_params, 0, &color_params_bytes(frame));

        // 上传 Y 平面
        if let (Some(y_tex), Some(y_plane)) = (Some(&self.y_texture), frame.planes.first()) {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: y_tex,
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
        if let Some(uv_tex) = Some(&self.uv_texture) {
            match frame.format {
                PixelFormat::Nv12 => {
                    if let Some(uv_plane) = frame.planes.get(1) {
                        self.queue.write_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture: uv_tex,
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
                            texture: uv_tex,
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
        }

        // 叠加层时间以挂钟为准，在两个视频帧之间也连续推进；
        // 无时间戳的帧沿用上一次 present 的时间，避免把叠加层时钟重置为 0
        let overlay_pts = frame.pts_secs().unwrap_or(self.last_pts);
        self.present_frame(self.overlay_clock.now_pts().max(overlay_pts));
    }

    /// 视频 pass + overlay pass + present（不上传视频纹理，由 render_frame_inner 或节拍空档调用）
    ///
    /// `pts` 为本次上屏的叠加层媒体时间（秒）。
    fn present_frame(&mut self, pts: f64) {
        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.surface_config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                tracing::error!("GPU surface lost");
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => {
                tracing::warn!("surface texture unavailable");
                return;
            }
        };

        // 计算保持视频比例的 viewport（letterbox / pillarbox）
        let surf_w = surface_texture.texture.width() as f32;
        let surf_h = surface_texture.texture.height() as f32;
        let video_w = self.width as f32;
        let video_h = self.height as f32;
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
                label: Some("yuv_pass"),
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
        // 渲染所有 Overlay 叠加层（字幕、弹幕等）
        // 在 video pass 之后、present 之前执行
        self.overlay_stack.update_all(pts);
        if !self.overlay_stack.is_empty() {
            self.overlay_stack.render_all(
                &self.device,
                &self.queue,
                &mut encoder,
                &view,
                self.surface_config.format,
                surface_texture.texture.width(),
                surface_texture.texture.height(),
            );
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        surface_texture.present();
        self.last_pts = pts;
        self.render_frame_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 收下 pipeline 提交的视频帧，等下一个节拍再上屏
    fn enqueue_frame(&mut self, frame: VideoFrame) {
        // 队列满说明一个节拍内到了多帧（片源帧率高于刷新率或相位错开）：
        // 丢最旧的一帧，保证上屏顺序单调推进
        if self.pending_frames.len() == PENDING_FRAME_CAPACITY {
            self.pending_frames.pop_front();
        }
        self.pending_frames.push_back(frame);
    }

    /// 一个渲染节拍：有新视频帧就上传上屏，否则只在叠加层时间推进时重绘
    fn render_tick(&mut self) {
        if let Some(frame) = self.pending_frames.pop_front() {
            self.last_frame = Some(frame.clone());
            self.render_frame_inner(&frame);
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

    fn run(
        mut self,
        cmd_rx: mpsc::Receiver<WindowCmd>,
        ready_tx: mpsc::Sender<()>,
        closed_flag: Arc<AtomicBool>,
    ) {
        let _ = ready_tx.send(());
        let mut next_tick = Instant::now();
        loop {
            // FIFO 下 present 自身阻塞到下一个 vblank，节拍由显示器决定；
            // 无 vsync 的后端按探测到的刷新率做软件节流
            let wait = if self.vsync_locked {
                VSYNC_POLL_BUDGET
            } else {
                next_tick.saturating_duration_since(Instant::now())
            };
            match cmd_rx.recv_timeout(wait) {
                Ok(cmd) => match cmd {
                    WindowCmd::Frame(frame) => self.enqueue_frame(frame),
                    WindowCmd::SetOverlays(overlays) => {
                        self.overlay_stack = OverlayStack::new();
                        for overlay in overlays {
                            self.overlay_stack.push(overlay);
                        }
                        tracing::info!(
                            "WindowRenderer: overlay stack updated ({} overlays)",
                            self.overlay_stack.len()
                        );
                        // 叠加层内容变了但挂钟可能没推进（如暂停中加载弹幕）：立即重绘一次
                        if self.last_frame.is_some() {
                            let pts = self.overlay_clock.now_pts().max(self.last_pts);
                            self.present_frame(pts);
                        }
                    }
                    WindowCmd::SetTitle(title) => {
                        self.title = title.clone();
                        self.window.set_title(&title);
                    }
                    WindowCmd::Shutdown => break,
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }

            // 命令在节拍到期前到达时不占用本节拍，但 pending 已满时必须让出一次上屏，
            // 否则片源帧率高于刷新率会把渲染饿死
            if !self.vsync_locked
                && Instant::now() < next_tick
                && self.pending_frames.len() < PENDING_FRAME_CAPACITY
            {
                continue;
            }

            self.pump_events();
            if self.window_should_close {
                closed_flag.store(true, Ordering::Release);
                break;
            }
            self.render_tick();

            if !self.vsync_locked {
                next_tick += self.frame_period;
                let now = Instant::now();
                if next_tick < now {
                    // 落后超过一个节拍（渲染耗时超出预算）：重新对齐，不追帧空转
                    next_tick = now;
                }
            }
        }
        tracing::info!("window thread exiting");
    }
}

/// WgpuRenderer — 基于 wgpu 的软解软渲实现
///
/// `init()` 时生成专属窗口线程，EventLoop/Window 始终在该线程上操作。
pub struct WgpuRenderer {
    width: u32,
    height: u32,
    cmd_tx: Option<mpsc::SyncSender<WindowCmd>>,
    /// 共享标志：窗口线程在窗口关闭时设置为 true，外部通过 should_stop() 读取
    window_closed: Arc<AtomicBool>,
    /// 窗口线程累计渲染帧数（用于计算实时 FPS）
    render_frame_count: Arc<AtomicU64>,
    /// 叠加层挂钟时钟：本线程按上屏帧锚定，窗口线程按挂钟外推
    overlay_clock: Arc<OverlayClock>,
    initialized: bool,
}

unsafe impl Send for WgpuRenderer {}

impl WgpuRenderer {
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            cmd_tx: None,
            window_closed: Arc::new(AtomicBool::new(false)),
            render_frame_count: Arc::new(AtomicU64::new(0)),
            overlay_clock: Arc::new(OverlayClock::new()),
            initialized: false,
        }
    }

    /// 设置 Overlay 叠加层（字幕、弹幕等）
    ///
    /// 将 overlays 发送到窗口线程，替换当前的 overlay 栈。
    /// 在 pipeline 启动后调用，确保窗口线程已就绪。
    pub fn set_overlays(&self, overlays: Vec<Box<dyn crate::overlay::Overlay>>) {
        if let Some(tx) = &self.cmd_tx {
            if let Err(e) = tx.try_send(WindowCmd::SetOverlays(overlays)) {
                tracing::error!(
                    "set_overlays FAILED: {:?} (channel full or disconnected)",
                    e
                );
            }
        } else {
            tracing::warn!("set_overlays: renderer not initialized, overlays dropped");
        }
    }
}

impl Default for WgpuRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoOutput for WgpuRenderer {
    fn init(&mut self, width: u32, height: u32) -> raptor_core::Result<()> {
        tracing::info!("WgpuRenderer::init({}x{})", width, height);
        self.width = width;
        self.height = height;
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<WindowCmd>(4);
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let closed_flag = Arc::clone(&self.window_closed);
        let frame_counter = Arc::clone(&self.render_frame_count);
        let overlay_clock = Arc::clone(&self.overlay_clock);
        std::thread::Builder::new()
            .name("raptor-window".into())
            .spawn(move || {
                match WindowRenderer::new(width, height, closed_flag, frame_counter, overlay_clock)
                {
                    Ok(wr) => {
                        let flag = Arc::clone(&wr.window_closed_flag);
                        wr.run(cmd_rx, ready_tx, flag);
                    }
                    Err(e) => {
                        tracing::error!("WindowRenderer init failed: {e}");
                    }
                }
            })
            .map_err(|e| raptor_core::RaptorError::Internal(format!("spawn window thread: {e}")))?;
        ready_rx.recv().map_err(|_| {
            raptor_core::RaptorError::Internal("window thread failed to start".into())
        })?;
        self.cmd_tx = Some(cmd_tx);
        self.initialized = true;
        tracing::info!("WgpuRenderer ready: {}x{}", width, height);
        Ok(())
    }

    fn submit_frame(&mut self, frame: &VideoFrame) -> raptor_core::Result<()> {
        if !self.initialized || self.window_closed.load(Ordering::Acquire) {
            return Ok(());
        }
        // 以本帧 PTS 重新锚定叠加层挂钟，窗口线程据此外推（无时间戳帧不覆盖）
        if let Some(secs) = frame.pts_secs() {
            self.overlay_clock.reanchor(secs);
        }
        if let Some(tx) = &self.cmd_tx {
            // 使用 try_send 避免无界队列内存增长；
            // 队列满时说明窗口线程消费不及时，丢弃该帧以施加反压
            match tx.try_send(WindowCmd::Frame(frame.clone())) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => {
                    tracing::debug!("submit_frame: channel full, dropping frame");
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    self.window_closed.store(true, Ordering::Release);
                }
            }
        }
        Ok(())
    }

    fn set_size(&mut self, width: u32, height: u32) -> raptor_core::Result<()> {
        self.width = width;
        self.height = height;
        Ok(())
    }

    fn should_stop(&self) -> bool {
        self.window_closed.load(Ordering::Acquire)
    }

    /// poll 不再发送空帧。
    ///
    /// 旧实现会向窗口线程发送零尺寸空帧来触发事件泵，但窗口线程收到空帧后
    /// 仍会执行完整的 render pass（清屏黑色 → present），导致画面/黑屏交替闪烁。
    /// 窗口线程按刷新率节拍自行泵事件，无需外部触发。
    fn poll(&mut self) {
        // 无操作 — 窗口线程自行泵事件
    }

    fn freeze_overlay_clock(&self) {
        self.overlay_clock.freeze();
    }

    fn render_frame_count(&self) -> u64 {
        self.render_frame_count.load(Ordering::Relaxed)
    }

    fn set_title(&mut self, title: &str) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.try_send(WindowCmd::SetTitle(title.to_string()));
        }
    }

    fn set_overlays(&mut self, overlays: Vec<Box<dyn crate::overlay::Overlay>>) {
        if let Some(tx) = &self.cmd_tx {
            if let Err(e) = tx.try_send(WindowCmd::SetOverlays(overlays)) {
                tracing::error!(
                    "VideoOutput::set_overlays FAILED: {:?} (channel full or disconnected)",
                    e
                );
            }
        } else {
            tracing::warn!("VideoOutput::set_overlays: not initialized, overlays dropped");
        }
    }
}

impl Drop for WgpuRenderer {
    fn drop(&mut self) {
        tracing::info!("WgpuRenderer::drop");
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(WindowCmd::Shutdown);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_renderer_new() {
        let renderer = WgpuRenderer::new();
        assert_eq!(renderer.width, 0);
        assert_eq!(renderer.height, 0);
        assert!(!renderer.window_closed.load(Ordering::Relaxed));
    }
}
