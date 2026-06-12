use raptor_core::Result;
use raptor_ffmpeg::{PixelFormat, VideoFrame};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use winit::event_loop::{EventLoop, EventLoopBuilder};
use winit::platform::pump_events::EventLoopExtPumpEvents;
#[cfg(target_os = "windows")]
use winit::platform::windows::EventLoopBuilderExtWindows;
use winit::window::WindowBuilder;

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
}

enum WindowCmd {
    Frame(VideoFrame),
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
    window_should_close: bool,
    /// 共享标志：窗口关闭时设置为 true，供 WgpuRenderer 侧读取
    window_closed_flag: Arc<AtomicBool>,
}

impl WindowRenderer {
    /// `closed_flag` 用于在窗口关闭时通知外部（WgpuRenderer 侧）。
    fn new(
        width: u32,
        height: u32,
        closed_flag: Arc<AtomicBool>,
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

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = unsafe {
            wgpu::SurfaceTargetUnsafe::from_window(&window)
                .map(|target| instance.create_surface_unsafe(target))
                .map_err(|e| format!("surface target: {e}"))?
        }
        .map_err(|e| format!("surface: {e}"))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok_or_else(|| "no suitable GPU adapter".to_string())?;

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("raptor_device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
            },
            None,
        ))
        .map_err(|e| format!("device: {e}"))?;

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .first()
            .copied()
            .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);

        // 优先使用 Mailbox（无 vsync 阻塞、低延迟），不可用时回退到 FIFO
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: width.max(1),
            height: height.max(1),
            present_mode,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&device, &surface_config);

        let (render_pipeline, bind_group, y_texture, uv_texture) =
            Self::setup_pipeline(&device, surface_format, width, height);

        tracing::info!(
            "WindowRenderer initialized: {}x{}, format={:?}",
            width,
            height,
            surface_format
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
            window_should_close: false,
            window_closed_flag: closed_flag,
        })
    }

    fn pump_events(&mut self) {
        let mut should_close = false;
        let _ =
            self.event_loop
                .pump_events(Some(std::time::Duration::from_millis(5)), |event, _| {
                    if let winit::event::Event::WindowEvent {
                        event: winit::event::WindowEvent::CloseRequested,
                        ..
                    } = event
                    {
                        should_close = true;
                    }
                });
        if should_close {
            self.window_should_close = true;
        }
    }

    fn render_frame(&mut self, frame: &VideoFrame) {
        self.pump_events();
        if self.window_should_close {
            return;
        }

        if frame.width != self.width || frame.height != self.height {
            self.width = frame.width;
            self.height = frame.height;
            let (pipeline, bg, yt, uvt) = Self::setup_pipeline(
                &self.device,
                self.surface_config.format,
                frame.width,
                frame.height,
            );
            self.render_pipeline = pipeline;
            self.bind_group = bg;
            self.y_texture = yt;
            self.uv_texture = uvt;
            self.surface_config.width = frame.width.max(1);
            self.surface_config.height = frame.height.max(1);
            self.surface.configure(&self.device, &self.surface_config);
        }

        if let (Some(y_tex), Some(y_plane)) = (Some(&self.y_texture), frame.planes.first()) {
            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: y_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &y_plane.data,
                wgpu::ImageDataLayout {
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

        if let Some(uv_tex) = Some(&self.uv_texture) {
            match frame.format {
                PixelFormat::Nv12 => {
                    if let Some(uv_plane) = frame.planes.get(1) {
                        self.queue.write_texture(
                            wgpu::ImageCopyTexture {
                                texture: uv_tex,
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            &uv_plane.data,
                            wgpu::ImageDataLayout {
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
                        wgpu::ImageCopyTexture {
                            texture: uv_tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        &uv_interleaved,
                        wgpu::ImageDataLayout {
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

        let surface_texture = match self.surface.get_current_texture() {
            Ok(t) => t,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.surface_config);
                return;
            }
            Err(wgpu::SurfaceError::OutOfMemory) => {
                tracing::error!("GPU out of memory");
                return;
            }
            Err(e) => {
                tracing::warn!("surface texture error: {}", e);
                return;
            }
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
            pass.draw(0..4, 0..1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        surface_texture.present();
    }

    fn setup_pipeline(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> (
        wgpu::RenderPipeline,
        wgpu::BindGroup,
        wgpu::Texture,
        wgpu::Texture,
    ) {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuv_to_rgb"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/yuv_to_rgb.wgsl").into()),
        });

        let y_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("y_texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let uv_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("uv_texture"),
            size: wgpu::Extent3d {
                width: (width / 2).max(1),
                height: (height / 2).max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuv_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuv_bind_group_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let uv_view = uv_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuv_bind_group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uv_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuv_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuv_render_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        (render_pipeline, bind_group, y_texture, uv_texture)
    }

    fn run(
        mut self,
        cmd_rx: mpsc::Receiver<WindowCmd>,
        ready_tx: mpsc::Sender<()>,
        closed_flag: Arc<AtomicBool>,
    ) {
        let _ = ready_tx.send(());
        loop {
            match cmd_rx.recv_timeout(std::time::Duration::from_millis(16)) {
                Ok(WindowCmd::Frame(frame)) => {
                    self.render_frame(&frame);
                    if self.window_should_close {
                        closed_flag.store(true, Ordering::Release);
                        break;
                    }
                }
                Ok(WindowCmd::Shutdown) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.pump_events();
                    if self.window_should_close {
                        closed_flag.store(true, Ordering::Release);
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
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
            initialized: false,
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
        std::thread::Builder::new()
            .name("raptor-window".into())
            .spawn(
                move || match WindowRenderer::new(width, height, closed_flag) {
                    Ok(wr) => {
                        let flag = Arc::clone(&wr.window_closed_flag);
                        wr.run(cmd_rx, ready_tx, flag);
                    }
                    Err(e) => {
                        tracing::error!("WindowRenderer init failed: {e}");
                    }
                },
            )
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
    /// 窗口线程的 recv_timeout(16ms) 已保证每 16ms 泵一次事件，无需外部触发。
    fn poll(&mut self) {
        // 无操作 — 窗口线程自行泵事件
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

fn interleave_uv_planes(
    u_data: &[u8],
    u_stride: usize,
    v_data: &[u8],
    v_stride: usize,
    uv_width: usize,
    uv_height: usize,
) -> Vec<u8> {
    let mut result = Vec::with_capacity(uv_width * uv_height * 2);
    for row in 0..uv_height {
        for col in 0..uv_width {
            let u = if row * u_stride + col < u_data.len() {
                u_data[row * u_stride + col]
            } else {
                128
            };
            let v = if row * v_stride + col < v_data.len() {
                v_data[row * v_stride + col]
            } else {
                128
            };
            result.push(u);
            result.push(v);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_interleave_uv_planes() {
        let u = vec![10u8, 20, 30, 40];
        let v = vec![50u8, 60, 70, 80];
        let result = interleave_uv_planes(&u, 2, &v, 2, 2, 2);
        assert_eq!(result, vec![10, 50, 20, 60, 30, 70, 40, 80]);
    }
    #[test]
    fn test_interleave_uv_empty() {
        let result = interleave_uv_planes(&[], 0, &[], 0, 0, 0);
        assert!(result.is_empty());
    }
    #[test]
    fn test_renderer_new() {
        let renderer = WgpuRenderer::new();
        assert_eq!(renderer.width, 0);
        assert_eq!(renderer.height, 0);
        assert!(!renderer.window_closed.load(Ordering::Relaxed));
    }
}
