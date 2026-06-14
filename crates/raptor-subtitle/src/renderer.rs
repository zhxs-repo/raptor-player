//! 字幕渲染器 — 实现 Overlay trait，使用 ab_glyph + wgpu 渲染字幕文本

use crate::types::{SubtitleConfig, SubtitleEvent};
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use parking_lot::Mutex;
use raptor_render::Overlay;
use std::sync::Arc;

/// 字幕渲染状态 — 由外部线程写入，渲染线程读取
pub struct SubtitleState {
    /// 字幕事件列表
    pub events: Vec<SubtitleEvent>,
    /// 当前显示的字幕文本（空 = 不显示）
    pub current_text: String,
    /// 是否启用
    pub enabled: bool,
}

/// 缓存的纹理
struct CachedTexture {
    texture: wgpu::Texture,
    width: u32,
    height: u32,
}

/// 字幕引擎 — 管理字幕数据、解析、渲染
///
/// 实现 Overlay trait，可集成到 OverlayStack 中。
pub struct SubtitleEngine {
    /// 共享状态
    state: Arc<Mutex<SubtitleState>>,
    /// 渲染配置
    config: SubtitleConfig,
    /// 字体原始字节数据
    font_data: Option<Vec<u8>>,
    /// TTC 字体偏移
    font_ttc_offset: Option<(usize, usize)>,
    /// 缓存的纹理（key = 文本哈希）
    texture_cache: std::collections::HashMap<u64, CachedTexture>,
    /// 上一次渲染的文本（用于缓存命中检测）
    last_text: String,
    /// wgpu 渲染管线（惰性初始化）
    render_pipeline: Option<wgpu::RenderPipeline>,
    bind_group_layout: Option<wgpu::BindGroupLayout>,
    sampler: Option<wgpu::Sampler>,
    surface_format: Option<wgpu::TextureFormat>,
    /// 诊断计数器
    frame_count: u64,
}

// Safety: SubtitleEngine is only accessed from one thread at a time via Overlay
unsafe impl Send for SubtitleEngine {}

impl SubtitleEngine {
    pub fn new(config: SubtitleConfig) -> Self {
        Self {
            state: Arc::new(Mutex::new(SubtitleState {
                events: Vec::new(),
                current_text: String::new(),
                enabled: true,
            })),
            config,
            font_data: None,
            font_ttc_offset: None,
            texture_cache: std::collections::HashMap::new(),
            last_text: String::new(),
            render_pipeline: None,
            bind_group_layout: None,
            sampler: None,
            surface_format: None,
            frame_count: 0,
        }
    }

    /// 设置字体数据（支持 TTF/OTF/TTC 格式）
    pub fn set_font(&mut self, font_data: Vec<u8>) {
        if font_data.len() >= 12 && &font_data[0..4] == b"ttcf" {
            let num_fonts = u32::from_be_bytes(
                font_data[8..12].try_into().unwrap_or([0,0,0,0])
            );
            if num_fonts > 0 && font_data.len() >= 16 {
                let offset = u32::from_be_bytes(
                    font_data[12..16].try_into().unwrap_or([0,0,0,0])
                ) as usize;
                if offset < font_data.len() {
                    tracing::info!("SubtitleEngine: TTC font detected, {} fonts, first at offset {}", num_fonts, offset);
                    self.font_ttc_offset = Some((offset, font_data.len() - offset));
                    self.font_data = Some(font_data);
                    self.texture_cache.clear();
                    return;
                }
            }
        }
        self.font_ttc_offset = None;
        self.font_data = Some(font_data);
        self.texture_cache.clear();
    }

    fn make_font(&self) -> Option<FontRef<'_>> {
        let data = self.font_data.as_ref()?;
        if let Some((offset, len)) = self.font_ttc_offset {
            FontRef::try_from_slice(&data[offset..offset + len]).ok()
        } else {
            FontRef::try_from_slice(data).ok()
        }
    }

    /// 获取共享状态句柄
    pub fn shared_state(&self) -> Arc<Mutex<SubtitleState>> {
        self.state.clone()
    }

    /// 加载字幕文件
    pub fn load_from_file(&self, path: &str) -> raptor_core::Result<()> {
        let data = std::fs::read(path)
            .map_err(|e| raptor_core::RaptorError::FileNotFound(format!("{}: {}", path, e)))?;

        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("srt");

        let parser = crate::parser::parser_for_extension(ext);
        let events = parser.parse(&data);
        tracing::info!("SubtitleEngine: loaded {} events from {}", events.len(), path);

        let mut state = self.state.lock();
        state.events = events;
        Ok(())
    }

    /// 从字幕事件列表加载
    pub fn load_events(&self, events: Vec<SubtitleEvent>) {
        let mut state = self.state.lock();
        state.events = events;
    }

    /// 渲染文本为 RGBA bitmap
    fn rasterize_text(&self, text: &str) -> Option<(Vec<u8>, u32, u32)> {
        let font = self.make_font()?;

        let scale = PxScale::from(self.config.default_font_size as f32);
        let scaled_font = font.as_scaled(scale);
        let ascent = scaled_font.ascent();

        // 按行处理
        let lines: Vec<&str> = text.lines().collect();
        let line_height = ascent + scaled_font.descent() + 4.0; // 行间距 4px

        let mut max_width = 0.0f32;
        let mut line_glyphs: Vec<Vec<(ab_glyph::Glyph, f32)>> = Vec::new();

        for line in &lines {
            let mut width = 0.0f32;
            let mut glyphs = Vec::new();
            for ch in line.chars() {
                let glyph = scaled_font.scaled_glyph(ch);
                let advance = scaled_font.h_advance(glyph.id);
                glyphs.push((glyph, width));
                width += advance;
            }
            max_width = max_width.max(width);
            line_glyphs.push(glyphs);
        }

        let width_px = max_width.ceil() as u32 + 8; // 4px padding each side
        let height_px = (line_height * lines.len() as f32).ceil() as u32 + 8;

        if width_px == 0 || height_px == 0 {
            return None;
        }

        let mut pixels = vec![0u8; (width_px * height_px * 4) as usize];

        let r = ((self.config.default_color >> 16) & 0xFF) as u8;
        let g = ((self.config.default_color >> 8) & 0xFF) as u8;
        let b = (self.config.default_color & 0xFF) as u8;

        for (line_idx, glyphs) in line_glyphs.iter().enumerate() {
            let line_y = 4.0 + line_idx as f32 * line_height + ascent;
            // 居中
            let line_width: f32 = glyphs.last().map(|(_, x)| {
                x + glyphs.iter().last().map(|(g, _)| scaled_font.h_advance(g.id)).unwrap_or(0.0)
            }).unwrap_or(0.0);
            let line_offset_x = (width_px as f32 - line_width) / 2.0;

            for (glyph, x_offset) in glyphs {
                if let Some(outlined) = scaled_font.outline_glyph(glyph.clone()) {
                    let bounds = outlined.px_bounds();
                    let base_x = line_offset_x + x_offset;
                    outlined.draw(|gx, gy, coverage| {
                        let px = (base_x + gx as f32) as u32;
                        let py = (line_y + gy as f32 - bounds.min.y) as u32;
                        if px < width_px && py < height_px {
                            let idx = ((py * width_px + px) * 4) as usize;
                            if idx + 3 < pixels.len() {
                                let alpha = (coverage * 255.0) as u8;
                                pixels[idx] = r;
                                pixels[idx + 1] = g;
                                pixels[idx + 2] = b;
                                pixels[idx + 3] = alpha;
                            }
                        }
                    });
                }
            }
        }

        Some((pixels, width_px, height_px))
    }

    /// 确保渲染管线就绪
    fn ensure_pipeline(&mut self, device: &wgpu::Device, surface_format: wgpu::TextureFormat) {
        if self.surface_format == Some(surface_format) && self.render_pipeline.is_some() {
            return;
        }

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("subtitle_overlay"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/overlay.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("subtitle_bg_layout"),
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
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("subtitle_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("subtitle_pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("subtitle_render_pipeline"),
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

        self.render_pipeline = Some(render_pipeline);
        self.bind_group_layout = Some(bind_group_layout);
        self.sampler = Some(sampler);
        self.surface_format = Some(surface_format);
    }

    /// 简单的字符串哈希（用于缓存 key）
    fn text_hash(text: &str) -> u64 {
        let mut hash: u64 = 5381;
        for byte in text.bytes() {
            hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
        }
        hash
    }
}

impl Overlay for SubtitleEngine {
    fn update(&mut self, pts: f64) {
        let state = self.state.lock();
        if !state.enabled {
            return;
        }

        // 收集当前时间点所有活跃的字幕事件（最多 8 行）
        let active: Vec<&SubtitleEvent> = state
            .events
            .iter()
            .filter(|e| pts >= e.start_time && pts < e.end_time)
            .take(8)
            .collect();

        let new_text = if active.is_empty() {
            String::new()
        } else {
            active.iter().map(|e| e.text.as_str()).collect::<Vec<_>>().join("\n")
        };

        // 每 300 帧输出诊断日志
        if self.frame_count.is_multiple_of(300) {
            tracing::info!(
                "SubtitleEngine::update: frame={} pts={:.2}s events={} active={} text={:?}",
                self.frame_count, pts, state.events.len(), active.len(),
                if new_text.len() > 40 { &new_text[..40] } else { &new_text }
            );
        }
        self.frame_count += 1;

        drop(state);
        let mut state = self.state.lock();
        state.current_text = new_text;
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
        self.ensure_pipeline(device, surface_format);

        let (pipeline, bg_layout, sampler) = match (
            &self.render_pipeline,
            &self.bind_group_layout,
            &self.sampler,
        ) {
            (Some(p), Some(b), Some(s)) => (p, b, s),
            _ => return,
        };

        let state = self.state.lock();
        if !state.enabled || state.current_text.is_empty() {
            return;
        }

        let text = state.current_text.clone();
        drop(state);

        // 检查缓存
        let hash = Self::text_hash(&text);
        if !self.texture_cache.contains_key(&hash) {
            if let Some((pixels, w, h)) = self.rasterize_text(&text) {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("subtitle_text"),
                    size: wgpu::Extent3d {
                        width: w.max(1),
                        height: h.max(1),
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
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
                self.texture_cache.insert(hash, CachedTexture { texture, width: w, height: h });
            }
        }

        if let Some(cached) = self.texture_cache.get(&hash) {
            // 计算字幕位置（底部居中，像素坐标）
            let scale_x = surface_width as f32 / self.config.canvas_width as f32;
            let scale_y = surface_height as f32 / self.config.canvas_height as f32;
            let tex_w = cached.width as f32 * scale_x;
            let tex_h = cached.height as f32 * scale_y;
            let x = (surface_width as f32 - tex_w) / 2.0;
            let y = surface_height as f32 - tex_h - self.config.bottom_margin as f32 * scale_y;

            let view = cached.texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("subtitle_bg"),
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
                ],
            });

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("subtitle_pass"),
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

        self.last_text = text;
    }

    fn is_visible(&self) -> bool {
        self.state.lock().enabled
    }
}
