//! 字幕渲染器 — 实现 Overlay trait，使用 ab_glyph + wgpu 渲染字幕文本
//!
//! ASS 特效路径：解析层保留覆盖标签段（`crates/raptor-subtitle/src/ass.rs`），
//! `update(pts)` 每帧为每个活跃事件求值出 `RenderItem`（位置/字号/颜色/淡入淡出），
//! `render()` 逐 item 光栅化（带缓存）并按锚点+对齐上屏。
//! 纯文本事件（SRT / 内嵌字幕）走相同的 item 管线，锚点为底部居中，
//! 行为与旧实现一致。

use std::num::NonZeroU64;

use crate::ass::{eval_fade, eval_move, Alignment, AssEvent, AssStyle, AssTags};
use crate::types::{SubtitleConfig, SubtitleEvent};
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use parking_lot::Mutex;
use raptor_render::Overlay;
use std::collections::HashMap;
use std::sync::Arc;

/// 单帧最多渲染的字幕条数（也等于 alpha uniform 的槽位数）
const MAX_ITEMS: usize = 8;
/// uniform 槽位间隔（min_uniform_buffer_offset_alignment）
const UNIFORM_SLOT: u64 = 256;

/// 字幕状态 — 由外部线程写入，渲染线程读取
pub struct SubtitleState {
    /// 字幕事件列表（纯文本视图，SRT/内嵌字幕及诊断用）
    pub events: Vec<SubtitleEvent>,
    /// ASS 事件（含覆盖标签段）；非空时作为渲染数据源
    pub ass_events: Vec<AssEvent>,
    /// ASS 样式表（[V4+ Styles]）
    pub styles: HashMap<String, AssStyle>,
    /// ASS 脚本分辨率（PlayResX/Y）；0 = 回退到 config 画布尺寸
    pub play_res: (f32, f32),
    /// 当前显示的字幕文本（空 = 不显示；兼容旧接口）
    pub current_text: String,
    /// 是否启用
    pub enabled: bool,
    /// 事件源是否为用户显式加载的字幕文件
    ///
    /// 为 true 时忽略内嵌字幕轨的实时追加：多轨切换尚未实现，
    /// 两个来源同时写入会叠着显示两套字幕。
    pub external_source: bool,
}

/// 缓存的纹理
struct CachedTexture {
    texture: wgpu::Texture,
    width: u32,
    height: u32,
}

/// 一帧中一条字幕的求值结果（画布坐标系）
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RenderItem {
    text: String,
    font_size: f32,
    /// 样式/覆盖颜色 RGBA（0..1，直通 alpha）
    colour: [f32; 4],
    /// \fad 求值得到的整体不透明度乘子（0..1）
    alpha: f32,
    /// 锚点（画布像素）
    anchor: (f32, f32),
    /// 对齐（锚点相对文本框的位置）
    align: Alignment,
    /// 定位所用画布尺寸
    canvas: (f32, f32),
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
    /// 缓存的纹理（key = 文本+字号+颜色哈希）
    texture_cache: HashMap<u64, CachedTexture>,
    /// 本帧活跃字幕的求值结果（update 写入，render 读取）
    active_items: Vec<RenderItem>,
    /// wgpu 渲染管线（惰性初始化）
    render_pipeline: Option<wgpu::RenderPipeline>,
    bind_group_layout: Option<wgpu::BindGroupLayout>,
    sampler: Option<wgpu::Sampler>,
    surface_format: Option<wgpu::TextureFormat>,
    /// per-item 淡出 alpha（uniform buffer，MAX_ITEMS 个槽位）
    alpha_buffer: Option<wgpu::Buffer>,
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
                ass_events: Vec::new(),
                styles: HashMap::new(),
                play_res: (0.0, 0.0),
                current_text: String::new(),
                enabled: true,
                external_source: false,
            })),
            config,
            font_data: None,
            texture_cache: HashMap::new(),
            active_items: Vec::new(),
            render_pipeline: None,
            bind_group_layout: None,
            sampler: None,
            surface_format: None,
            alpha_buffer: None,
            frame_count: 0,
        }
    }

    /// 设置字体数据（支持 TTF/OTF/TTC 格式）
    pub fn set_font(&mut self, font_data: Vec<u8>) {
        let font_data = if font_data.len() >= 16 && &font_data[0..4] == b"ttcf" {
            // TTC: 提取第一个字体并重写 offset 表；直接切片会导致 ab_glyph 解析失败
            let offset = u32::from_be_bytes(font_data[12..16].try_into().unwrap()) as usize;
            match raptor_render::extract_font_from_ttc(&font_data, offset) {
                Some(extracted) => {
                    tracing::info!("SubtitleEngine: TTC font extracted from offset {offset}");
                    extracted
                }
                None => font_data,
            }
        } else {
            font_data
        };
        self.font_data = Some(font_data);
        self.texture_cache.clear();
    }

    fn make_font(&self) -> Option<FontRef<'_>> {
        FontRef::try_from_slice(self.font_data.as_ref()?).ok()
    }

    /// 获取共享状态句柄
    pub fn shared_state(&self) -> Arc<Mutex<SubtitleState>> {
        self.state.clone()
    }

    /// 加载字幕文件（.ass/.ssa 保留样式表与特效标签；其余按纯文本事件）
    pub fn load_from_file(&self, path: &str) -> raptor_core::Result<()> {
        let data = std::fs::read(path)
            .map_err(|e| raptor_core::RaptorError::FileNotFound(format!("{}: {}", path, e)))?;

        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("srt");

        let mut state = self.state.lock();
        if ext.eq_ignore_ascii_case("ass") || ext.eq_ignore_ascii_case("ssa") {
            let parser = crate::parser::AssParser::new();
            let doc = parser.parse_document(&data);
            tracing::info!(
                "SubtitleEngine: loaded {} ASS events ({} styles, play_res={:.0}x{:.0}) from {}",
                doc.events.len(),
                doc.styles.len(),
                doc.play_res_x,
                doc.play_res_y,
                path
            );
            state.events = doc.events.iter().map(|e| e.base.clone()).collect();
            state.ass_events = doc.events;
            state.styles = doc.styles;
            state.play_res = (doc.play_res_x, doc.play_res_y);
        } else {
            let parser = crate::parser::parser_for_extension(ext);
            let events = parser.parse(&data);
            tracing::info!(
                "SubtitleEngine: loaded {} events from {}",
                events.len(),
                path
            );
            state.ass_events = events.iter().cloned().map(AssEvent::plain).collect();
            state.events = events;
            state.styles = HashMap::new();
            state.play_res = (0.0, 0.0);
        }
        state.external_source = true;
        Ok(())
    }

    /// 从字幕事件列表加载（纯文本，无特效）
    pub fn load_events(&self, events: Vec<SubtitleEvent>) {
        let mut state = self.state.lock();
        state.ass_events = events.iter().cloned().map(AssEvent::plain).collect();
        state.events = events;
        state.styles = HashMap::new();
        state.play_res = (0.0, 0.0);
        state.external_source = false;
    }

    /// 追加字幕事件（不清空已有内容），供内嵌字幕边解码边注入使用
    pub fn append_events(&self, events: Vec<SubtitleEvent>) {
        if events.is_empty() {
            return;
        }
        let mut state = self.state.lock();
        if state.external_source {
            tracing::debug!("subtitle: 忽略内嵌轨追加（用户已加载外部字幕文件）");
            return;
        }
        state.events.extend(events.iter().cloned());
        state
            .ass_events
            .extend(events.into_iter().map(AssEvent::plain));
    }

    /// config 派生的默认样式（无 ASS 样式表时使用，行为与旧实现一致）
    fn config_default_style(&self) -> AssStyle {
        let c = self.config.default_color;
        AssStyle {
            name: "Default".to_string(),
            font_name: String::new(),
            font_size: 0.0, // 0 = 使用 config.default_font_size
            primary_colour: [
                ((c >> 16) & 0xFF) as f32 / 255.0,
                ((c >> 8) & 0xFF) as f32 / 255.0,
                (c & 0xFF) as f32 / 255.0,
                1.0,
            ],
            alignment: Alignment::default_bottom_center(),
            margin_l: 0.0,
            margin_r: 0.0,
            margin_v: 0.0, // 0 = 使用 config.bottom_margin
        }
    }

    /// 合并样式表 + 覆盖标签 + 边距，在 pts 时刻求值出一条字幕的渲染参数
    fn make_item(&self, ev: &AssEvent, state: &SubtitleState, pts: f64) -> Option<RenderItem> {
        // 折叠所有段的标签（后写赢）
        let mut tags = AssTags::default();
        for (seg_tags, _) in &ev.segments {
            tags.apply_overlay(seg_tags);
        }

        let text = if ev.segments.is_empty() {
            ev.base.text.clone()
        } else {
            let joined: String = ev.segments.iter().map(|(_, t)| t.as_str()).collect();
            joined.replace("\\N", "\n").replace("\\n", "\n")
        };
        if text.trim().is_empty() {
            return None;
        }

        let file_style = state.styles.get(&ev.base.style);
        let default = self.config_default_style();
        let style = file_style.unwrap_or(&default);

        let font_size = tags
            .font_size
            .or(if style.font_size > 0.0 {
                Some(style.font_size)
            } else {
                None
            })
            .unwrap_or(self.config.default_font_size as f32);

        let colour = tags.color.unwrap_or(style.primary_colour);
        let align = tags.alignment.unwrap_or(style.alignment);

        // 边距：事件字段 > 样式 > config 默认（0 视为未设置）
        let pick_margin = |ev_m: f32, st_m: f32, cfg_default: f32| -> f32 {
            if ev_m > 0.0 {
                ev_m
            } else if st_m > 0.0 {
                st_m
            } else {
                cfg_default
            }
        };
        let canvas = if state.play_res.0 > 0.0 && state.play_res.1 > 0.0 {
            state.play_res
        } else {
            (
                self.config.canvas_width as f32,
                self.config.canvas_height as f32,
            )
        };
        let margin_l = pick_margin(ev.margin_l, style.margin_l, 0.0);
        let margin_r = pick_margin(ev.margin_r, style.margin_r, 0.0);
        let margin_v = pick_margin(
            ev.margin_v,
            style.margin_v,
            self.config.bottom_margin as f32,
        );

        // 锚点：\move 插值 > \pos > 对齐+边距推出的默认锚点
        let anchor = if let Some(mv) = tags.move_ {
            eval_move(&mv, pts, ev.base.start_time, ev.base.end_time)
        } else if let Some(pos) = tags.pos {
            pos
        } else {
            let (w, h) = canvas;
            let x = match align.h() {
                0 => margin_l,
                1 => w / 2.0,
                _ => w - margin_r,
            };
            let y = match align.v() {
                0 => h - margin_v,
                1 => h / 2.0,
                _ => margin_v,
            };
            (x, y)
        };

        let alpha = tags
            .fade
            .map(|f| eval_fade(f, pts, ev.base.start_time, ev.base.end_time))
            .unwrap_or(1.0);

        Some(RenderItem {
            text,
            font_size,
            colour,
            alpha,
            anchor,
            align,
            canvas,
        })
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        // 与 shader 中 vec4<f32> 对齐（16 字节）
                        min_binding_size: NonZeroU64::new(16),
                    },
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

        let alpha_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("subtitle_alpha"),
            size: (MAX_ITEMS as u64) * UNIFORM_SLOT,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
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
        self.alpha_buffer = Some(alpha_buffer);
        self.surface_format = Some(surface_format);
    }

    /// 缓存 key：文本 + 字号 + 颜色
    fn item_hash(item: &RenderItem) -> u64 {
        let mut hash: u64 = 5381;
        for byte in item.text.bytes() {
            hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
        }
        for bits in [
            item.font_size.to_bits(),
            item.colour[0].to_bits(),
            item.colour[1].to_bits(),
            item.colour[2].to_bits(),
        ] {
            hash = hash.wrapping_mul(31).wrapping_add(bits as u64);
        }
        hash
    }

    /// 渲染文本为 RGBA bitmap（指定字号与颜色）
    fn rasterize_text(&self, item: &RenderItem) -> Option<(Vec<u8>, u32, u32)> {
        let font = self.make_font()?;

        let scale = PxScale::from(item.font_size);
        let scaled_font = font.as_scaled(scale);
        let ascent = scaled_font.ascent();

        // 按行处理（ab_glyph 的 descent 为负值，行高 = ascent - descent）
        let lines: Vec<&str> = item.text.lines().collect();
        let line_height = ascent - scaled_font.descent() + 4.0;

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

        let r = (item.colour[0] * 255.0) as u8;
        let g = (item.colour[1] * 255.0) as u8;
        let b = (item.colour[2] * 255.0) as u8;

        for (line_idx, glyphs) in line_glyphs.iter().enumerate() {
            let line_y = 4.0 + line_idx as f32 * line_height + ascent;
            // 居中
            let line_width: f32 = glyphs
                .last()
                .map(|(_, x)| {
                    x + glyphs
                        .iter()
                        .last()
                        .map(|(g, _)| scaled_font.h_advance(g.id))
                        .unwrap_or(0.0)
                })
                .unwrap_or(0.0);
            let line_offset_x = (width_px as f32 - line_width) / 2.0;

            for (glyph, x_offset) in glyphs {
                if let Some(outlined) = scaled_font.outline_glyph(glyph.clone()) {
                    let bounds = outlined.px_bounds();
                    let base_x = line_offset_x + x_offset;
                    outlined.draw(|gx, gy, coverage| {
                        // bbox.min.y 为负（基线上方），必须加回而非减去；
                        // 先做浮点边界检查，避免负值 `as u32` 饱和成 0 写错行
                        let fx = base_x + gx as f32;
                        let fy = line_y + bounds.min.y + gy as f32;
                        if fx < 0.0 || fy < 0.0 || fx >= width_px as f32 || fy >= height_px as f32 {
                            return;
                        }
                        let (px, py) = (fx as u32, fy as u32);
                        let idx = ((py * width_px + px) * 4) as usize;
                        if idx + 3 < pixels.len() {
                            let alpha = (coverage * 255.0) as u8;
                            pixels[idx] = r;
                            pixels[idx + 1] = g;
                            pixels[idx + 2] = b;
                            pixels[idx + 3] = alpha;
                        }
                    });
                }
            }
        }

        Some((pixels, width_px, height_px))
    }
}

impl Overlay for SubtitleEngine {
    fn update(&mut self, pts: f64) {
        let state = self.state.lock();
        if !state.enabled {
            self.active_items.clear();
            return;
        }

        // 数据源：ASS 事件优先（含特效）；无 ASS 数据时回退到直接写入的 events
        // （外部测试会绕过 load_* 直接赋值 state.events）
        let fallback: Vec<AssEvent>;
        let source: &[AssEvent] = if state.ass_events.is_empty() {
            fallback = state
                .events
                .iter()
                .filter(|e| pts >= e.start_time && pts < e.end_time)
                .cloned()
                .map(AssEvent::plain)
                .collect();
            &fallback
        } else {
            &state.ass_events
        };

        let mut refs: Vec<&AssEvent> = source
            .iter()
            .filter(|e| {
                pts >= e.base.start_time && pts < e.base.end_time && !e.base.text.trim().is_empty()
            })
            .collect();

        // layer 升序：高 layer 后绘制（压在上层）；stable sort 保持同层文件顺序
        refs.sort_by_key(|e| e.layer);

        let items: Vec<RenderItem> = refs
            .iter()
            .take(MAX_ITEMS)
            .filter_map(|e| self.make_item(e, &state, pts))
            .collect();

        // 兼容旧接口：拼接纯文本
        let new_text = if refs.is_empty() {
            String::new()
        } else {
            refs.iter()
                .map(|e| e.base.text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        };

        if self.frame_count.is_multiple_of(300) {
            let preview: String = new_text.chars().take(40).collect();
            tracing::info!(
                "SubtitleEngine::update: frame={} pts={:.2}s events={} active={} text={:?}",
                self.frame_count,
                pts,
                source.len(),
                items.len(),
                preview
            );
        }
        self.frame_count += 1;

        self.active_items = items;
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
        if self.active_items.is_empty() {
            return;
        }

        self.ensure_pipeline(device, surface_format);

        let (pipeline, bg_layout, sampler, alpha_buffer) = match (
            &self.render_pipeline,
            &self.bind_group_layout,
            &self.sampler,
            &self.alpha_buffer,
        ) {
            (Some(p), Some(b), Some(s), Some(a)) => (p, b, s, a),
            _ => return,
        };

        let items = std::mem::take(&mut self.active_items);

        let mut prepared: Vec<(u64, f32)> = Vec::with_capacity(items.len());
        for item in &items {
            let hash = Self::item_hash(item);
            if !self.texture_cache.contains_key(&hash) {
                if let Some((pixels, w, h)) = self.rasterize_text(item) {
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
                    if self.texture_cache.len() > 64 {
                        self.texture_cache.clear();
                    }
                    self.texture_cache.insert(
                        hash,
                        CachedTexture {
                            texture,
                            width: w,
                            height: h,
                        },
                    );
                }
            }
            let mult = item.alpha * item.colour[3];
            prepared.push((hash, mult));
        }

        // 一个 render pass 内逐 item 切换 viewport + bind group
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

        for (slot, (item, (hash, mult))) in items.iter().zip(prepared.iter()).enumerate() {
            let Some(cached) = self.texture_cache.get(hash) else {
                continue;
            };

            // 画布坐标 → 表面像素
            let scale_x = surface_width as f32 / item.canvas.0;
            let scale_y = surface_height as f32 / item.canvas.1;
            let tex_w = cached.width as f32 * scale_x;
            let tex_h = cached.height as f32 * scale_y;
            let ax = item.anchor.0 * scale_x;
            let ay = item.anchor.1 * scale_y;
            let x = match item.align.h() {
                0 => ax,
                1 => ax - tex_w / 2.0,
                _ => ax - tex_w,
            };
            let y = match item.align.v() {
                0 => ay - tex_h,
                1 => ay - tex_h / 2.0,
                _ => ay,
            };

            // alpha 写入本 slot 的 uniform 区间
            let offset = (slot as u64) * UNIFORM_SLOT;
            queue.write_buffer(alpha_buffer, offset, &alpha_slot_bytes(*mult));

            let view = cached
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
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
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: alpha_buffer,
                            offset,
                            size: NonZeroU64::new(16),
                        }),
                    },
                ],
            });

            pass.set_bind_group(0, &bind_group, &[]);
            // set_viewport 使用像素坐标，vertex shader 的 NDC (-1..1) 会自动映射到此矩形
            pass.set_viewport(x, y, tex_w, tex_h, 0.0, 1.0);
            pass.draw(0..4, 0..1);
        }

        self.active_items = items;
    }

    fn is_visible(&self) -> bool {
        self.state.lock().enabled
    }
}

/// uniform 槽位内容：[alpha, 0, 0, 0]
fn alpha_slot_bytes(mult: f32) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&mult.clamp(0.0, 1.0).to_le_bytes());
    bytes
}

#[cfg(test)]
mod rasterize_regression {
    use super::*;

    /// 防回归：ab_glyph 字形 bbox 的 min.y 为负（基线上方）。
    /// 行坐标必须是 line_y + bounds.min.y + gy；用减法会把全部像素推出纹理
    /// （修复前字幕一个像素都画不出）。
    #[test]
    fn rasterize_paints_glyph_pixels() {
        let mut engine = SubtitleEngine::new(SubtitleConfig::default());
        let font = crate::font::load_system_font().expect("system font");
        engine.set_font(font);
        engine.load_events(vec![SubtitleEvent {
            start_time: 1.0,
            end_time: 5.0,
            text: "字幕测试".to_string(),
            style: "Default".to_string(),
        }]);
        engine.update(2.0);
        assert_eq!(engine.active_items.len(), 1);
        let item = &engine.active_items[0];
        let (px, w, h) = engine
            .rasterize_text(item)
            .expect("rasterize should succeed");
        let painted = px.chunks(4).filter(|p| p[3] > 0).count();
        assert!(
            painted > 50,
            "expected glyph coverage, got painted={painted} ({w}x{h})"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ass::AssDocument;

    fn event(start: f64, end: f64, text: &str) -> SubtitleEvent {
        SubtitleEvent {
            start_time: start,
            end_time: end,
            text: text.to_string(),
            style: "Default".to_string(),
        }
    }

    fn engine_with_doc(doc: AssDocument) -> SubtitleEngine {
        let engine = SubtitleEngine::new(SubtitleConfig::default());
        {
            let mut state = engine.state.lock();
            state.ass_events = doc.events;
            state.styles = doc.styles;
            state.play_res = (doc.play_res_x, doc.play_res_y);
            state.events = Vec::new();
        }
        engine
    }

    fn ass(src: &str) -> AssDocument {
        AssParser::new().parse_document(src.as_bytes())
    }

    use crate::parser::AssParser;

    /// 用户显式加载字幕文件后，内嵌轨的实时追加不得再混入同一事件表
    #[test]
    fn append_events_ignored_after_external_file_load() {
        let dir = std::env::temp_dir().join(format!("raptor_sub_ext_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ext.ass");
        std::fs::write(
            &path,
            "[Script Info]\nPlayResX: 1920\nPlayResY: 1080\n\n\
             [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour\n\
             Style: Default,Arial,60,&H00FFFFFF\n\n\
             [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
             Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,外部字幕\n",
        )
        .unwrap();

        let engine = SubtitleEngine::new(SubtitleConfig::default());
        engine.load_from_file(path.to_str().unwrap()).unwrap();
        let state = engine.shared_state();
        let after_load = state.lock().events.len();

        engine.append_events(vec![event(1.0, 2.0, "内嵌轨")]);
        assert_eq!(
            state.lock().events.len(),
            after_load,
            "外部字幕文件生效期间应忽略内嵌轨追加"
        );

        // 换回事件列表加载（如内嵌轨自身重新载入）则恢复追加能力
        engine.load_events(vec![event(1.0, 2.0, "内嵌轨")]);
        engine.append_events(vec![event(5.0, 6.0, "内嵌轨2")]);
        assert_eq!(state.lock().events.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_plain_event_anchors_bottom_center() {
        let mut engine = SubtitleEngine::new(SubtitleConfig::default());
        engine.load_events(vec![event(1.0, 5.0, "Hello")]);
        engine.update(2.0);
        assert_eq!(engine.active_items.len(), 1);
        let item = &engine.active_items[0];
        assert_eq!(item.text, "Hello");
        // config 画布 1920x1080，底部居中：锚点 (960, 1080-50)
        assert_eq!(item.anchor, (960.0, 1030.0));
        assert_eq!(item.align, Alignment(2));
        // current_text 兼容语义保持
        assert_eq!(engine.state.lock().current_text, "Hello");
    }

    #[test]
    fn test_an8_moves_anchor_to_top() {
        let doc = ass(r#"[Script Info]
PlayResX: 640
PlayResY: 360

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,{\an8}top line
"#);
        let mut engine = engine_with_doc(doc);
        engine.update(2.0);
        let item = &engine.active_items[0];
        assert_eq!(item.canvas, (640.0, 360.0));
        assert_eq!(item.align, Alignment(8));
        // an8：顶部，y = margin_v（无样式 → config bottom_margin 50）
        assert_eq!(item.anchor, (320.0, 50.0));
    }

    #[test]
    fn test_pos_and_fad_and_colour() {
        let doc = ass(r#"[Script Info]
PlayResX: 1000
PlayResY: 500

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:11.00,Default,,0,0,0,,{\pos(120,80)\fad(1000,0)\c&H0000FF&}fx
"#);
        let mut engine = engine_with_doc(doc);

        engine.update(1.5); // 淡入中点
        let item = &engine.active_items[0];
        assert_eq!(item.anchor, (120.0, 80.0));
        assert_eq!(item.colour, [1.0, 0.0, 0.0, 1.0]);
        assert!((item.alpha - 0.5).abs() < 0.01);

        engine.update(5.0); // 淡入完成
        assert!((engine.active_items[0].alpha - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_move_interpolates_per_frame() {
        let doc = ass(r#"[Script Info]
PlayResX: 1920
PlayResY: 1080

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:10.00,0:00:20.00,Default,,0,0,0,,{\move(100,100,500,100)}scroll
"#);
        let mut engine = engine_with_doc(doc);
        engine.update(12.5);
        assert_eq!(engine.active_items[0].anchor, (200.0, 100.0));
        engine.update(15.0);
        assert_eq!(engine.active_items[0].anchor, (300.0, 100.0));
        engine.update(19.0);
        assert_eq!(engine.active_items[0].anchor, (460.0, 100.0));
    }

    #[test]
    fn test_style_fontsize_colour_margin_from_table() {
        let doc = ass(r#"[Script Info]
PlayResX: 320
PlayResY: 240

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Big,MS PGothic,36,&H0000FF00&,&H00000000,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,6,10,10,20

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:05.00,Big,,0,0,0,,styled
"#);
        let mut engine = engine_with_doc(doc);
        engine.update(2.0);
        let item = &engine.active_items[0];
        assert_eq!(item.font_size, 36.0);
        assert_eq!(item.colour, [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(item.align, Alignment(6)); // 右中
                                              // an6 右中：x = 320 - margin_r(10)，y = 240/2
        assert_eq!(item.anchor, (310.0, 120.0));
    }

    #[test]
    fn test_multi_event_layer_ordering_and_cap() {
        let mut src = String::from(
            "[Script Info]\nPlayResX: 1920\nPlayResY: 1080\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        );
        for i in 0..12 {
            src.push_str(&format!(
                "Dialogue: {},0:00:01.00,0:00:05.00,Default,,0,0,0,,line {}\n",
                i % 3,
                i
            ));
        }
        let doc = ass(&src);
        let mut engine = engine_with_doc(doc);
        engine.update(2.0);
        assert_eq!(engine.active_items.len(), MAX_ITEMS);
        // layer 升序绘制：首个 item 来自最低 layer（i % 3 == 0 → i=0）
        assert_eq!(engine.active_items[0].text, "line 0");
    }

    #[test]
    fn test_fs_override() {
        let doc = ass(r#"[Script Info]
PlayResX: 1920
PlayResY: 1080

[V4+ Styles]
Format: Name,Fontname,Fontsize,PrimaryColour,SecondaryColour,OutlineColour,BackColour,Bold,Italic,Underline,StrikeOut,ScaleX,ScaleY,Spacing,Angle,BorderStyle,Outline,Shadow,Alignment,MarginL,MarginR,MarginV,Encoding
Style: Default,Arial,20,&H00FFFFFF,0,0,0,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,{\fs48}big{\fs20} small
"#);
        let mut engine = engine_with_doc(doc);
        engine.update(2.0);
        // 后写赢：整条事件用最后出现的 \fs
        assert_eq!(engine.active_items[0].font_size, 20.0);
        assert_eq!(engine.active_items[0].text, "big small");
    }
}
