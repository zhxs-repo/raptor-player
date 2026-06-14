//! 弹幕渲染器 — 实现 Overlay trait，使用 ab_glyph + wgpu 渲染弹幕文本
//!
//! 渲染流程：
//! 1. 使用 ab_glyph 将文本光栅化为 RGBA bitmap
//! 2. 上传为 wgpu texture
//! 3. 使用简单 quad + alpha blending 渲染到 surface

use crate::layout::LayoutEngine;
use crate::types::{DanmakuConfig, DanmakuInstance, DanmakuItem};
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use parking_lot::Mutex;
use raptor_render::Overlay;
use std::collections::HashMap;
use std::sync::Arc;

/// 缓存的弹幕纹理
struct CachedTexture {
    texture: wgpu::Texture,
    width: u32,
    height: u32,
}

/// 弹幕渲染状态 — 由 DanmakuEngine 和 render 线程共享
pub struct DanmakuState {
    /// 布局后的弹幕实例列表
    pub instances: Vec<DanmakuInstance>,
    /// 是否启用
    pub enabled: bool,
    /// 全局不透明度
    pub opacity: f32,
    /// 当前播放时间（秒），由 update() 写入，render() 读取
    pub current_pts: f64,
}

/// 弹幕引擎 — 管理弹幕数据、布局、渲染
///
/// 实现 Overlay trait，可集成到 OverlayStack 中。
pub struct DanmakuEngine {
    /// 共享状态（布局线程写入，渲染线程读取）
    state: Arc<Mutex<DanmakuState>>,
    /// 渲染配置
    config: DanmakuConfig,
    /// 字体原始字节数据
    font_data: Option<Vec<u8>>,
    /// TTC 字体偏移（如果是 TTC 文件，存储第一个字体的 offset 和 length）
    font_ttc_offset: Option<(usize, usize)>,
    /// 缓存的纹理（key = danmaku index）
    texture_cache: HashMap<usize, CachedTexture>,
    /// wgpu 渲染管线（惰性初始化）
    render_pipeline: Option<wgpu::RenderPipeline>,
    bind_group_layout: Option<wgpu::BindGroupLayout>,
    sampler: Option<wgpu::Sampler>,
    /// opacity uniform buffer
    uniform_buffer: Option<wgpu::Buffer>,
    /// 当前 surface format（用于检测 format 变化）
    surface_format: Option<wgpu::TextureFormat>,
    /// 每帧渲染状态诊断计数器
    frame_count: u64,
    /// 首次渲染标志（用于调试日志）
    first_render_logged: bool,
    /// 光栅化全透明警告计数器（只输出前 3 次）
    rasterize_zero_warn_count: u32,
}

// Safety: DanmakuEngine is only accessed from one thread at a time via the Overlay trait
unsafe impl Send for DanmakuEngine {}

impl DanmakuEngine {
    pub fn new(config: DanmakuConfig) -> Self {
        Self {
            state: Arc::new(Mutex::new(DanmakuState {
                instances: Vec::new(),
                enabled: true,
                opacity: 1.0,
                current_pts: 0.0,
            })),
            config,
            font_data: None,
            font_ttc_offset: None,
            texture_cache: HashMap::new(),
            render_pipeline: None,
            bind_group_layout: None,
            sampler: None,
            uniform_buffer: None,
            surface_format: None,
            frame_count: 0,
            first_render_logged: false,
            rasterize_zero_warn_count: 0,
        }
    }

    /// 设置字体数据（支持 TTF/OTF/TTC 格式）
    pub fn set_font(&mut self, font_data: Vec<u8>) {
        // 检测 TTC 格式（TrueType Collection）
        if font_data.len() >= 12 && &font_data[0..4] == b"ttcf" {
            // TTC header: "ttcf" + version(4) + num_fonts(4) + offsets(4*N)
            let num_fonts = u32::from_be_bytes(
                font_data[8..12].try_into().unwrap_or([0,0,0,0])
            );
            if num_fonts > 0 && font_data.len() >= 16 {
                let offset = u32::from_be_bytes(
                    font_data[12..16].try_into().unwrap_or([0,0,0,0])
                ) as usize;
                if offset < font_data.len() {
                    tracing::info!("DanmakuEngine: TTC font detected, {} fonts, first at offset {}", num_fonts, offset);
                    self.font_ttc_offset = Some((offset, font_data.len() - offset));
                    self.font_data = Some(font_data);
                    self.texture_cache.clear();
                    return;
                }
            }
        }
        // 普通 TTF/OTF 格式
        self.font_ttc_offset = None;
        self.font_data = Some(font_data);
        self.texture_cache.clear();
    }

    /// 从存储的字体数据创建 FontRef（自动处理 TTC/TTF）
    fn make_font(&self) -> Option<FontRef<'_>> {
        let data = self.font_data.as_ref()?;
        if let Some((offset, len)) = self.font_ttc_offset {
            FontRef::try_from_slice(&data[offset..offset + len]).ok()
        } else {
            FontRef::try_from_slice(data).ok()
        }
    }

    /// 获取共享状态句柄（供外部线程写入弹幕数据）
    pub fn shared_state(&self) -> Arc<Mutex<DanmakuState>> {
        self.state.clone()
    }

    /// 加载弹幕数据并执行布局
    pub fn load_items(&self, items: Vec<DanmakuItem>) {
        let engine = LayoutEngine::new(self.config.clone());
        let instances = engine.layout(&items);

        // 输出时间分布诊断
        if !instances.is_empty() {
            let min_start = instances.iter().map(|i| i.start_time).fold(f64::INFINITY, f64::min);
            let max_start = instances.iter().map(|i| i.start_time).fold(f64::NEG_INFINITY, f64::max);
            let max_end = instances.iter().map(|i| i.end_time).fold(f64::NEG_INFINITY, f64::max);
            // 统计前10条的时间分布
            let sample: Vec<String> = instances.iter().take(10).map(|i| {
                format!("{:.1}-{:.1}", i.start_time, i.end_time)
            }).collect();
            tracing::info!(
                "DanmakuEngine::load_items: {} instances, time_range=[{:.2}s, {:.2}s], max_end={:.2}s, first10=[{}]",
                instances.len(), min_start, max_start, max_end, sample.join(", ")
            );
        }

        let mut state = self.state.lock();
        state.instances = instances;
    }

    /// 从文件加载弹幕（自动选择解析器）
    pub fn load_from_file(&self, path: &str) -> raptor_core::Result<()> {
        let data = std::fs::read(path)
            .map_err(|e| raptor_core::RaptorError::FileNotFound(format!("{}: {}", path, e)))?;

        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("json");

        let parser = crate::parser::parser_for_extension(ext);
        let items = parser.parse(&data);
        tracing::info!("DanmakuEngine: loaded {} items from {}", items.len(), path);
        self.load_items(items);
        Ok(())
    }

    /// 渲染单条弹幕文本为 RGBA bitmap（含 DFM 风格黑色描边）
    fn rasterize_text(
        &self,
        text: &str,
        font_size: u32,
        color: u32,
    ) -> Option<(Vec<u8>, u32, u32)> {
        let font = self.make_font()?;

        let scale = PxScale::from(font_size as f32);
        let scaled_font = font.as_scaled(scale);

        // 计算文本尺寸
        let mut width = 0.0f32;
        let mut glyphs = Vec::new();
        let ascent = scaled_font.ascent();

        for ch in text.chars() {
            let glyph = scaled_font.scaled_glyph(ch);
            let advance = scaled_font.h_advance(glyph.id);
            glyphs.push((glyph, width));
            width += advance;
        }

        // 描边偏移 1px（DFM 使用 Paint.setShadowLayer 实现黑色描边）
        let outline_offset: f32 = 1.5;
        let width_px = (width + outline_offset * 2.0).ceil() as u32;
        let height_px = (scaled_font.ascent() + scaled_font.descent() + outline_offset * 2.0).ceil() as u32;

        if width_px == 0 || height_px == 0 {
            return None;
        }

        let mut pixels = vec![0u8; (width_px * height_px * 4) as usize];

        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;

        // 第一遍：绘制黑色描边（偏移 8 个方向，模拟 DFM 的 shadowLayer 效果）
        for &(ox, oy) in &[
            (-outline_offset, 0.0), (outline_offset, 0.0),
            (0.0, -outline_offset), (0.0, outline_offset),
            (-outline_offset * 0.7, -outline_offset * 0.7),
            (outline_offset * 0.7, -outline_offset * 0.7),
            (-outline_offset * 0.7, outline_offset * 0.7),
            (outline_offset * 0.7, outline_offset * 0.7),
        ] {
            for (glyph, x_offset) in &glyphs {
                if let Some(outlined) = scaled_font.outline_glyph(glyph.clone()) {
                    let bounds = outlined.px_bounds();
                    let glyph_x = *x_offset + outline_offset + ox;
                    let base_y = ascent + outline_offset + oy;
                    outlined.draw(|gx, gy, coverage| {
                        let px = (glyph_x + gx as f32) as i32;
                        let py = (base_y + gy as f32 - bounds.min.y) as i32;
                        if px >= 0 && py >= 0 && (px as u32) < width_px && (py as u32) < height_px {
                            let idx = ((py as u32 * width_px + px as u32) * 4) as usize;
                            if idx + 3 < pixels.len() {
                                let alpha = (coverage * 255.0) as u8;
                                // 描边: 黑色, 使用 max 避免覆盖已有的彩色像素
                                if pixels[idx + 3] < alpha {
                                    pixels[idx] = 0;
                                    pixels[idx + 1] = 0;
                                    pixels[idx + 2] = 0;
                                    pixels[idx + 3] = alpha;
                                }
                            }
                        }
                    });
                }
            }
        }

        // 第二遍：绘制彩色文字（覆盖在描边之上）
        for (glyph, x_offset) in &glyphs {
            if let Some(outlined) = scaled_font.outline_glyph(glyph.clone()) {
                let bounds = outlined.px_bounds();
                let glyph_x = *x_offset + outline_offset;
                let base_y = ascent + outline_offset;
                outlined.draw(|gx, gy, coverage| {
                    let px = (glyph_x + gx as f32) as u32;
                    let py = (base_y + gy as f32 - bounds.min.y) as u32;
                    if px < width_px && py < height_px {
                        let idx = ((py * width_px + px) * 4) as usize;
                        if idx + 3 < pixels.len() {
                            let alpha = (coverage * 255.0) as u8;
                            // 彩色文字直接覆盖（包括描边像素）
                            pixels[idx] = r;
                            pixels[idx + 1] = g;
                            pixels[idx + 2] = b;
                            pixels[idx + 3] = alpha;
                        }
                    }
                });
            }
        }

        Some((pixels, width_px, height_px))
    }

    /// 确保渲染管线已初始化
    fn ensure_pipeline(
        &mut self,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
    ) {
        // 如果 format 变化，重建管线
        if self.surface_format == Some(surface_format) && self.render_pipeline.is_some() {
            return;
        }

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("danmaku_overlay"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/overlay.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("danmaku_bg_layout"),
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
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("danmaku_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("danmaku_pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("danmaku_render_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
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
            multiview_mask: None,
            cache: None,
        });

        // opacity uniform buffer (f32 = 4 bytes)
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("danmaku_opacity_uniform"),
            size: 16, // 最小 16 字节对齐
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        self.render_pipeline = Some(render_pipeline);
        self.bind_group_layout = Some(bind_group_layout);
        self.sampler = Some(sampler);
        self.uniform_buffer = Some(uniform_buffer);
        self.surface_format = Some(surface_format);
        tracing::info!("DanmakuEngine: render pipeline initialized for {:?}", surface_format);
    }
}

impl Overlay for DanmakuEngine {
    fn update(&mut self, pts: f64) {
        // 将当前 pts 写入共享状态，供 render() 使用
        self.state.lock().current_pts = pts;
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        surface_format: wgpu::TextureFormat,
        surface_width: u32,
        surface_height: u32,
    ) {
        // 确保渲染管线就绪
        self.ensure_pipeline(device, surface_format);

        let (pipeline, bg_layout, sampler, uniform_buf) = match (
            &self.render_pipeline,
            &self.bind_group_layout,
            &self.sampler,
            &self.uniform_buffer,
        ) {
            (Some(p), Some(b), Some(s), Some(u)) => (p, b, s, u),
            _ => return,
        };

        let state = self.state.lock();
        if !state.enabled || state.instances.is_empty() {
            if self.frame_count.is_multiple_of(300) {
                tracing::info!(
                    "DanmakuEngine::render: skip frame={} enabled={} instances={}",
                    self.frame_count, state.enabled, state.instances.len()
                );
            }
            self.frame_count += 1;
            return;
        }

        let current_time = state.current_pts;

        let active = LayoutEngine::active_at(&state.instances, current_time);
        if active.is_empty() {
            // 每 60 帧输出一次诊断（约 2 秒）
            if self.frame_count.is_multiple_of(60) {
                // 找到距当前 pts 最近的实例
                let nearest = state.instances.iter().min_by(|a, b| {
                    let da = (a.start_time - current_time).abs();
                    let db = (b.start_time - current_time).abs();
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                });
                let first = state.instances.first();
                let last = state.instances.last();
                tracing::info!(
                    "DanmakuEngine: NO_ACTIVE frame={} pts={:.2}s total={} first=[{:.2},{:.2}) last=[{:.2},{:.2}) nearest=[{:.2},{:.2}) \"{}\"",
                    self.frame_count, current_time, state.instances.len(),
                    first.map(|f| f.start_time).unwrap_or(-1.0),
                    first.map(|f| f.end_time).unwrap_or(-1.0),
                    last.map(|l| l.start_time).unwrap_or(-1.0),
                    last.map(|l| l.end_time).unwrap_or(-1.0),
                    nearest.map(|n| n.start_time).unwrap_or(-1.0),
                    nearest.map(|n| n.end_time).unwrap_or(-1.0),
                    nearest.map(|n| &n.item.text[..n.item.text.len().min(20)]).unwrap_or(""),
                );
            }
            self.frame_count += 1;
            return;
        }

        // 首次渲染时输出调试日志
        if !self.first_render_logged {
            self.first_render_logged = true;
            tracing::info!(
                "DanmakuEngine: FIRST_RENDER pts={:.2}s active={} total={} surface={}x{} scale=({:.3},{:.3})",
                current_time, active.len(), state.instances.len(),
                surface_width, surface_height,
                surface_width as f32 / self.config.canvas_width,
                surface_height as f32 / self.config.canvas_height,
            );
            if let Some(first) = active.first() {
                let x = LayoutEngine::current_x(first, current_time);
                tracing::info!(
                    "DanmakuEngine: first_active: \"{}\" time=[{:.2},{:.2}) y={:.1} x={:.1} font={} color=#{:06X}",
                    first.item.text, first.start_time, first.end_time, first.y, x,
                    first.item.font_size, first.item.color
                );
            }
        }

        // 每 300 帧输出渲染统计
        if self.frame_count.is_multiple_of(300) && self.frame_count > 0 {
            tracing::info!(
                "DanmakuEngine: frame={} pts={:.2}s active={} cache_size={}",
                self.frame_count, current_time, active.len(), self.texture_cache.len()
            );
        }
        self.frame_count += 1;

        // 每帧更新 opacity uniform
        let opacity = state.opacity;
        queue.write_buffer(uniform_buf, 0, &opacity.to_ne_bytes());

        // 计算画布到 surface 的缩放比例
        let scale_x = surface_width as f32 / self.config.canvas_width;
        let scale_y = surface_height as f32 / self.config.canvas_height;

        // 为每条活跃弹幕渲染纹理 quad
        for (i, inst) in active.iter().enumerate() {
            let idx = state
                .instances
                .iter()
                .position(|x| std::ptr::eq(x, *inst))
                .unwrap_or(i);

            // 检查纹理缓存
            if !self.texture_cache.contains_key(&idx) {
                // 光栅化文本
                if let Some((pixels, w, h)) =
                    self.rasterize_text(&inst.item.text, inst.item.font_size, inst.item.color)
                {
                    // 检查光栅化结果是否有非透明像素
                    let non_transparent = pixels.chunks(4).filter(|p| p[3] > 0).count();
                    if non_transparent == 0 && self.rasterize_zero_warn_count < 3 {
                        self.rasterize_zero_warn_count += 1;
                        tracing::warn!(
                            "DanmakuEngine: rasterize_text produced 0 visible pixels for \"{}\" font={} color=#{:06X}",
                            inst.item.text, inst.item.font_size, inst.item.color
                        );
                    }
                    let texture = device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("danmaku_text"),
                        size: wgpu::Extent3d {
                            width: w.max(1),
                            height: h.max(1),
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        &pixels,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(w * 4),
                            rows_per_image: Some(h),
                        },
                        wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                    );
                    self.texture_cache.insert(
                        idx,
                        CachedTexture {
                            texture,
                            width: w,
                            height: h,
                        },
                    );
                }
            }

            // 计算当前帧的屏幕坐标
            let x = LayoutEngine::current_x(inst, current_time) * scale_x;
            let y = inst.y * scale_y;

            if let Some(cached) = self.texture_cache.get(&idx) {
                let tex_w = cached.width as f32 * scale_x;
                let tex_h = cached.height as f32 * scale_y;

                let view = cached.texture.create_view(&wgpu::TextureViewDescriptor::default());
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("danmaku_bg"),
                    layout: bg_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: uniform_buf.as_entire_binding(),
                        },
                    ],
                });

                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("danmaku_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    ..Default::default()
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                // set_viewport 使用像素坐标，vertex shader 的 NDC (-1..1) 会自动映射到此矩形
                pass.set_viewport(x, y, tex_w, tex_h, 0.0, 1.0);
                pass.draw(0..4, 0..1);
            }
        }
    }

    fn is_visible(&self) -> bool {
        self.state.lock().enabled
    }
}
