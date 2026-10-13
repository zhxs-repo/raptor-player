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
use raptor_render::font_util::{extract_ttc_preferred, CJK_SC_PREFER};
use raptor_render::{Overlay, OverlayArea};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// 单帧最多渲染的字幕条数（也等于 alpha uniform 的槽位数）
///
/// Danmaku2ASS 风格的滚动字幕同屏常有 20~40 条事件，上限低于实际密度时，
/// 溢出裁剪会把仍在屏上滚动的老字幕整条抽走（表现为"滚到左侧突然消失"）。
/// 取 128 作为容量兜底，正常弹幕密度下不会触发裁剪。
const MAX_ITEMS: usize = 128;
/// uniform 槽位间隔（min_uniform_buffer_offset_alignment）
const UNIFORM_SLOT: u64 = 256;

/// 字幕位图缓存容量（张）
///
/// 一条长台词的光栅化位图可达 2000×2000，缓存必须有界，否则长时间播放会一路
/// 涨到 OOM。超出后按 FIFO 淘汰，本帧仍在用的条目不动。
const TEXTURE_CACHE_CAPACITY: usize = 192;

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
    /// `texture_cache` 的插入顺序（FIFO 近似 LRU）：本帧用到的 key 会被挪到末尾，
    /// 超出容量时从头淘汰
    texture_order: VecDeque<u64>,
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
            texture_order: VecDeque::new(),
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
        // 调用方通常已经递过来单字体（load_system_font 会挑好 face）；这里是
        // 直接喂 TTC 时的兜底，同样按 SC 优先而不是死取 face #0。
        let font_data = match extract_ttc_preferred(&font_data, CJK_SC_PREFER) {
            Some(face) => {
                tracing::info!(
                    "SubtitleEngine: TTC face #{} '{}' extracted",
                    face.index,
                    face.family
                );
                face.data
            }
            None => font_data,
        };
        self.font_data = Some(font_data);
        self.texture_cache.clear();
        self.texture_order.clear();
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

    /// 缓存 key：文本 + 字号 + 颜色 + 画布尺寸 + 字号系数
    ///
    /// 最后一项是**光栅化用的字号系数**（内容基准，见 [OverlayArea::content_scale]）：
    /// 同样一句话在不同系数下光栅出的位图尺寸不同，所以必须参与。
    ///
    /// 刻意**不含** surface 尺寸：字号改成内容基准后，位图只跟视频原生分辨率有关，
    /// 拖窗口 / 旋转屏幕 / 分屏都不会让这句话的位图失效，缓存因此可以跨 resize 复用。
    fn item_hash(item: &RenderItem, font_scale: f32) -> u64 {
        let mut hash: u64 = 5381;
        for byte in item.text.bytes() {
            hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
        }
        for bits in [
            item.font_size.to_bits(),
            item.colour[0].to_bits(),
            item.colour[1].to_bits(),
            item.colour[2].to_bits(),
            item.canvas.0.to_bits(),
            item.canvas.1.to_bits(),
            font_scale.to_bits(),
        ] {
            hash = hash.wrapping_mul(31).wrapping_add(bits as u64);
        }
        hash
    }

    /// 渲染文本为 RGBA bitmap（指定字号与颜色）
    ///
    /// [max_dim] 是设备允许的最大纹理边长（`max_texture_dimension_2d`）。
    /// 字幕字号来自 ASS 的 `\fs`，不受播放器控制：一条长台词在 1920x1080 的
    /// PlayRes 上按 fs=40 排出来轻易就是 3500+ 像素宽，而 GLES 后端的
    /// downlevel 上限只有 2048。硬建纹理会被 wgpu 判为 validation 错误，
    /// 进而让整帧的 bind group / submit 全部失效（黑屏，历史上直接 abort）。
    ///
    /// 所以这里先按可用边长反推一个缩放系数再光栅化。文字仍然按锚点落在画面矩形里
    /// 该落的位置（`render()` 用位置映射算 viewport），只是采样分辨率降低 ——
    /// 糊一点，远好过整条字幕画不出来。
    fn rasterize_text(
        &self,
        item: &RenderItem,
        font_scale: f32,
        max_dim: u32,
    ) -> Option<(Vec<u8>, u32, u32)> {
        let font = self.make_font()?;
        let lines: Vec<&str> = item.text.lines().collect();
        if lines.is_empty() {
            return None;
        }

        // ASS 的 \fs 是画布像素，乘上**内容基准**系数才是实际绘制像素。
        // 这个系数只跟视频原生分辨率有关，所以位图在拖窗口、旋转、分屏时都不会
        // 失效 —— 缓存 key 里也只放它，不放 surface 尺寸。
        let px_size = (item.font_size * font_scale).max(1.0);

        // 预算：每行 4px padding 已在下面统一加上，这里先算纯文本尺寸
        let budget = max_dim.saturating_sub(8).max(1) as f32;

        let layout = |scale_px: f32| {
            let scaled_font = font.as_scaled(PxScale::from(scale_px));
            let line_height = scaled_font.ascent() - scaled_font.descent() + 4.0;
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
            (scaled_font, line_height, max_width, line_glyphs)
        };

        // 先按目标字号量一次，只有超限才回退（避免误伤正常字号）
        let (scaled_font, line_height, max_width, line_glyphs) = layout(px_size);
        let overshoot = (max_width.max(line_height * lines.len() as f32)) / budget;
        let (scaled_font, line_height, max_width, line_glyphs) = if overshoot > 1.0 {
            layout(px_size / overshoot)
        } else {
            (scaled_font, line_height, max_width, line_glyphs)
        };

        let ascent = scaled_font.ascent();
        let width_px = (max_width.ceil() as u32).min(max_dim).max(1) + 0;
        let width_px = width_px.min(max_dim);
        let height_px = ((line_height * lines.len() as f32).ceil() as u32).min(max_dim);

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

        // layer 升序：高 layer 后绘制（压在上层）。同层按开始时间倒序：
        // 超出 MAX_ITEMS 时优先保留最新事件——最老的滚动字幕已接近出画，
        // 丢弃它们比把新事件整个挤掉（等旧批滚完才上场、一上场就在终点）好得多。
        refs.sort_by(|a, b| {
            a.layer
                .cmp(&b.layer)
                .then(b.base.start_time.total_cmp(&a.base.start_time))
        });

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
        area: OverlayArea,
    ) {
        if self.active_items.is_empty() {
            return;
        }

        self.ensure_pipeline(device, surface_format);

        // 单条字幕纹理不能超过这个边长，超了就是 validation 错误（见 rasterize_text）
        let max_dim = device.limits().max_texture_dimension_2d;

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
        let (_rect_x, _rect_y, rect_w, rect_h) = area.video_rect;

        // ── 两条缩放链刻意分开 ──────────────────────────────────────
        //
        // `pos_scale`：画布（ASS PlayRes）→ 画面矩形的仿射映射。位置必须跟着
        //   画面走（pillarbox 时才能贴住视频而不是黑边）。
        // `font_scale`：光栅化时的实际像素字号，**只跟视频原生分辨率有关**
        //   （见 OverlayArea::content_scale）。拖窗口 / 旋转 / 分屏时它不变，
        //   于是文字的物理大小保持恒定，缓存也不会被打掉。
        //
        // 位图按 font_scale 光栅化后**原尺寸 1:1 上屏**，绝不能再乘 pos_scale：
        // 那样会让同一个系数同时作用于位置和尺寸，长句在窗口变大时字反而变小。
        let pos_scale_of = |item: &RenderItem| {
            uniform_canvas_scale(item.canvas.0, item.canvas.1, rect_w as u32, rect_h as u32)
        };
        let font_scale_of = |item: &RenderItem| {
            let s = area.content_scale(item.canvas.1);
            if s.is_finite() && s > 0.0 {
                s
            } else {
                1.0
            }
        };

        let hash_of = |item: &RenderItem| Self::item_hash(item, font_scale_of(item));

        // 本帧用到的纹理 key 集合：本帧在用的条目永不淘汰
        let used_hashes: HashSet<u64> = items.iter().map(&hash_of).collect();

        let mut prepared: Vec<(u64, f32)> = Vec::with_capacity(items.len());
        for item in &items {
            let hash = hash_of(item);
            if !self.texture_cache.contains_key(&hash) {
                if let Some((pixels, w, h)) =
                    self.rasterize_text(item, font_scale_of(item), max_dim)
                {
                    let texture = device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("subtitle_text"),
                        size: wgpu::Extent3d {
                            // rasterize_text 已经按 max_dim 夹过了，这里再兜一层：
                            // 纹理一旦超限，wgpu 报的是 validation 错误而非本地 panic，
                            // 而 validation 错误会污染整条命令缓冲（后续 bind group、
                            // submit 全部失败），代价远大于丢掉这一条字幕。
                            width: w.clamp(1, max_dim),
                            height: h.clamp(1, max_dim),
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
                            // 上传尺寸必须和上面 clamp 后的纹理尺寸完全一致，
                            // 否则 wgpu 会因为数据量对不上而报 validation 错误
                            bytes_per_row: Some(w.clamp(1, max_dim) * 4),
                            rows_per_image: Some(h.clamp(1, max_dim)),
                        },
                        wgpu::Extent3d {
                            width: w.clamp(1, max_dim),
                            height: h.clamp(1, max_dim),
                            depth_or_array_layers: 1,
                        },
                    );
                    // 只登记插入顺序，淘汰统一放在本帧 item 处理完之后
                    self.texture_order.push_back(hash);
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

        // 本帧在用的条目刷到顺序末尾，再从头部淘汰直到回到容量上限。
        // 旧实现只在"新建纹理且 len > 256"时整表 retain：拖窗口期间每帧都要为每个
        // 活跃条目新建纹理，缓存表持续churn，而上限本身其实靠得住 —— 换成真正的
        // 容量裁剪后，既不再整表清空，也不会让长期播放的会话无限增长。
        self.texture_order
            .retain(|k| self.texture_cache.contains_key(k));
        for hash in &used_hashes {
            if let Some(pos) = self.texture_order.iter().position(|k| k == hash) {
                let k = self.texture_order.remove(pos).expect("position 刚取到");
                self.texture_order.push_back(k);
            }
        }
        while self.texture_order.len() > TEXTURE_CACHE_CAPACITY {
            match self.texture_order.pop_front() {
                Some(k) if !used_hashes.contains(&k) => {
                    self.texture_cache.remove(&k);
                }
                Some(k) => {
                    // 本帧还在用，不能丢；挪到末尾等下一帧再淘汰
                    self.texture_order.push_back(k);
                    break;
                }
                None => break,
            }
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

            // 画布坐标 → 画面矩形内的像素 → surface 像素
            //
            // 位置用 `pos_scale`（等比，取两轴较小值）：ASS 的 PlayRes 通常是
            // 1920x1080，而画面矩形可能是 1080x608 的横条。x/y 各自缩放会把 16:9
            // 的字幕横向压扁 —— 字形变形就是这么来的。
            //
            // 尺寸**不**用 pos_scale：位图已经按内容基准的字号光栅化好了，
            // 按原尺寸 1:1 放置即可（`tex_w/tex_h` 直接取缓存尺寸）。这样
            // 拖窗口时位置自适应而字形大小恒定，两者互不干扰。
            let pos_scale = pos_scale_of(item);
            let (x, y, tex_w, tex_h) = subtitle_placement(
                item,
                area.video_rect,
                pos_scale,
                cached.width as f32,
                cached.height as f32,
            );

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

/// 等比**位置**映射系数（contain）：取两轴较小值，保证画布完整放进目标矩形且不变形
///
/// 分开按 x/y 缩放是字幕在竖屏上被压扁的直接原因 —— 1920x1080 的 PlayRes
/// 映射到窄条画面时，x 方向要缩 0.56、y 方向要缩 1.37，比例失守。
///
/// 只用于**位置**。字号走 [`raptor_render::OverlayArea::content_scale`]（内容基准），
/// 两者刻意分开：位置跟画面走，字号跟内容走。
pub fn uniform_canvas_scale(canvas_w: f32, canvas_h: f32, target_w: u32, target_h: u32) -> f32 {
    if canvas_w <= 0.0 || canvas_h <= 0.0 || target_w == 0 || target_h == 0 {
        return 1.0;
    }
    (target_w as f32 / canvas_w).min(target_h as f32 / canvas_h)
}

/// 一条字幕的上屏矩形（surface 像素，左上角 + 宽高）
///
/// 这里刻意把**位置**与**尺寸**分成两个互不相干的输入：
///
/// - `pos_scale`：画布（ASS PlayRes）→ 画面矩形的等比映射，**随窗口变化**。
///   位置必须跟着画面走，pillarbox 时才能贴住视频而不是黑边。
/// - `tex_w/tex_h`：位图尺寸，光栅化时已按**内容基准**字号算好，**随窗口不变**。
///   直接原样使用，绝不再乘 `pos_scale` —— 否则同一个系数会同时作用于位置和
///   尺寸，窗口一变长句的字号反而跟着变。
pub(crate) fn subtitle_placement(
    item: &RenderItem,
    rect: (f32, f32, f32, f32),
    pos_scale: f32,
    tex_w: f32,
    tex_h: f32,
) -> (f32, f32, f32, f32) {
    let (rx, ry, rw, rh) = rect;
    // contain：等比映射后可能有一侧留白，需要在画面矩形内居中
    let offset_x = rx + (rw - item.canvas.0 * pos_scale) / 2.0;
    let offset_y = ry + (rh - item.canvas.1 * pos_scale) / 2.0;

    let ax = offset_x + item.anchor.0 * pos_scale;
    let ay = offset_y + item.anchor.1 * pos_scale;
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
    (x, y, tex_w, tex_h)
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
            .rasterize_text(item, 1.0, 2048)
            .expect("rasterize should succeed");
        let painted = px.chunks(4).filter(|p| p[3] > 0).count();
        assert!(
            painted > 50,
            "expected glyph coverage, got painted={painted} ({w}x{h})"
        );
    }

    /// 超长台词必须被压回纹理上限内，而不是建出超限纹理
    ///
    /// GLES 后端的 max_texture_dimension_2d 是 2048。ASS 里一条长台词按
    /// fs=40 排版轻易超过这个宽度，之前会直接触发 wgpu validation 错误，
    /// 连带整帧命令缓冲失效（真机上表现为 SIGABRT）。
    /// 光栅化字号必须随等比缩放变化，否则窗口尺寸变了字形大小对不上
    #[test]
    fn rasterized_bitmap_scales_with_canvas_scale() {
        let mut engine = SubtitleEngine::new(SubtitleConfig::default());
        let font = crate::font::load_system_font().expect("system font");
        engine.set_font(font);
        engine.load_events(vec![SubtitleEvent {
            start_time: 1.0,
            end_time: 5.0,
            text: "缩放测试".to_string(),
            style: "Default".to_string(),
        }]);
        engine.update(2.0);
        let item = &engine.active_items[0];

        let (_, w_small, _) = engine.rasterize_text(item, 0.5, 2048).expect("rasterize");
        let (_, w_big, _) = engine.rasterize_text(item, 1.0, 2048).expect("rasterize");

        assert!(
            w_big > w_small,
            "2x canvas scale should rasterize wider: {w_small} -> {w_big}"
        );
        let ratio = w_big as f32 / w_small as f32;
        assert!(
            (1.8..2.2).contains(&ratio),
            "expected ~2x width ratio, got {ratio}"
        );
    }

    /// 字幕必须按 ASS 的 PlayRes 等比映射到**画面矩形**
    ///
    /// 分轴缩放会把 16:9 的字幕压扁（这正是"文字被压窄"），所以取两轴的较小值。
    /// 映射目标从"整个 surface"改成"letterbox 之后的画面矩形"后，字幕才会贴住
    /// 视频而不是落到黑边上。
    #[test]
    fn pos_scale_is_uniform_and_fits_video_rect() {
        let (canvas_w, canvas_h) = (1920.0f32, 1080.0f32);

        // (surface 宽, surface 高, 视频宽, 视频高)
        for (sw, sh, vw, vh) in [
            (1920.0f32, 1080.0f32, 1920.0f32, 1080.0f32),
            (1080.0, 2340.0, 1920.0, 1080.0), // 竖屏播横屏视频 → letterbox
            (1920.0, 600.0, 1920.0, 1080.0),  // 扁窗口播高视频 → pillarbox
            (2400.0, 1080.0, 1920.0, 1080.0),
        ] {
            let rect = raptor_render::fit_video_rect(sw as u32, sh as u32, vw as u32, vh as u32);
            let rw = rect.2;
            let rh = rect.3;
            let uniform = (rw / canvas_w).min(rh / canvas_h);

            // 映射后必须完整落在画面矩形内（contain，不是 cover）
            assert!(
                canvas_w * uniform <= rw + 0.5,
                "scaled width {} exceeds rect {rw}",
                canvas_w * uniform
            );
            assert!(
                canvas_h * uniform <= rh + 0.5,
                "scaled height {} exceeds rect {rh}",
                canvas_h * uniform
            );
            // 宽高比必须保持 —— 分别缩放的话这个等式不成立
            let aspect_before = canvas_w / canvas_h;
            let aspect_after = (canvas_w * uniform) / (canvas_h * uniform);
            assert!(
                (aspect_before - aspect_after).abs() < 1e-4,
                "aspect changed: {aspect_before} -> {aspect_after} (rect {rw}x{rh})"
            );
        }
    }

    /// 核心保证：拖动窗口时字幕**位置自适应**但**尺寸恒定**
    ///
    /// 用户诉求原文是"不会跟随窗口拉伸或压缩，能自适应位置等属性，但大小不变"。
    /// 这两个性质由两个独立系数实现：
    /// - `content_scale`（字号）只看视频原生分辨率 → 拖窗口不变
    /// - `pos_scale`（位置映射）看画面矩形 → 拖窗口跟着走
    #[test]
    fn subtitle_keeps_glyph_size_across_windows_but_tracks_video_rect() {
        let mut engine = SubtitleEngine::new(SubtitleConfig::default());
        let font = crate::font::load_system_font().expect("system font");
        engine.set_font(font);
        engine.load_events(vec![SubtitleEvent {
            start_time: 1.0,
            end_time: 5.0,
            text: "大小不变".to_string(),
            style: "Default".to_string(),
        }]);
        engine.update(2.0);
        let item = engine.active_items[0].clone();

        // 同一个 1080p 视频，铺在各种尺寸的窗口里
        let windows = [
            (1920u32, 1080u32),
            (1080, 2340), // 竖屏手机
            (2560, 1440),
            (640, 480),
        ];

        let mut glyph_sizes = Vec::new();
        for (sw, sh) in windows {
            let area = raptor_render::OverlayArea::new(sw, sh, 1920, 1080);

            // 字号系数：与窗口无关
            let font_scale = area.content_scale(item.canvas.1);
            assert!(
                (font_scale - 1.0).abs() < 1e-6,
                "1080p 视频在 {sw}x{sh} 窗口下字号系数应为 1.0，实际 {font_scale}"
            );

            // 位图尺寸按这个系数光栅化
            let (_, tw, th) = engine
                .rasterize_text(&item, font_scale, 2048)
                .expect("rasterize");

            // 位置映射看画面矩形
            let rect = area.video_rect;
            let pos_scale =
                uniform_canvas_scale(item.canvas.0, item.canvas.1, rect.2 as u32, rect.3 as u32);
            let (x, y, _, _) = subtitle_placement(&item, rect, pos_scale, tw as f32, th as f32);

            // 字幕必须落在画面矩形内（不能跑到黑边上）
            assert!(
                x >= rect.0 - 1.0 && y >= rect.1 - 1.0,
                "{sw}x{sh}: 字幕左上角 ({x}, {y}) 跑到画面矩形 {rect:?} 之外"
            );
            assert!(
                x + tw as f32 <= rect.0 + rect.2 + 1.0,
                "{sw}x{sh}: 字幕右边缘越过画面矩形"
            );

            glyph_sizes.push((tw, th));
        }

        // 所有窗口下的位图尺寸必须完全一致
        for (w, h) in &glyph_sizes[1..] {
            assert_eq!(
                (*w, *h),
                glyph_sizes[0],
                "字号跟着窗口变了：{:?} → {:?}",
                glyph_sizes[0],
                (w, h)
            );
        }
    }

    #[test]
    fn oversized_subtitle_is_clamped_to_texture_limit() {
        let mut engine = SubtitleEngine::new(SubtitleConfig::default());
        let font = crate::font::load_system_font().expect("system font");
        engine.set_font(font);

        // 约 200 个汉字，fs=40 在 1920 画布上远超 2048 像素宽
        let long: String = "超长字幕".repeat(50);
        engine.load_events(vec![SubtitleEvent {
            start_time: 1.0,
            end_time: 5.0,
            text: long,
            style: "Default".to_string(),
        }]);
        engine.update(2.0);

        let item = &engine.active_items[0];
        let (_, w, h) = engine
            .rasterize_text(item, 1.0, 2048)
            .expect("rasterize should succeed");
        assert!(w <= 2048, "width {w} exceeds texture limit");
        assert!(h <= 2048, "height {h} exceeds texture limit");
        assert!(w > 0 && h > 0, "clamped size must stay non-zero");
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
        // 12 条低于上限，全部保留
        assert_eq!(engine.active_items.len(), 12);
        // layer 升序绘制：首个 item 来自最低 layer（i % 3 == 0 → i=0）
        assert_eq!(engine.active_items[0].text, "line 0");
    }

    #[test]
    fn test_overflow_keeps_newest_events() {
        // 模拟 Danmaku2ASS 滚动弹幕：超出上限的事件错峰开始、同层、持续 8s
        let n = MAX_ITEMS + 18;
        let mut src = String::from(
            "[Script Info]\nPlayResX: 1920\nPlayResY: 1080\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        );
        for i in 0..n {
            let start = i as f64 * 0.02;
            src.push_str(&format!(
                "Dialogue: 0,{:},{:},Default,,0,0,0,,{{\\move(2040,40,-120,40)}}dan {}\n",
                fmt_s(start),
                fmt_s(start + 8.0),
                i
            ));
        }
        let doc = ass(&src);
        let mut engine = engine_with_doc(doc);
        engine.update(3.0);
        assert_eq!(engine.active_items.len(), MAX_ITEMS);
        let texts: Vec<&str> = engine
            .active_items
            .iter()
            .map(|i| i.text.as_str())
            .collect();
        // 保留的是最新的 MAX_ITEMS 条：最后一条在、最老的 d0 被丢弃
        let last = format!("dan {}", n - 1);
        assert!(texts.contains(&last.as_str()), "texts={texts:?}");
        assert!(
            !texts.contains(&"dan 0"),
            "oldest must be dropped: {texts:?}"
        );
        // 最新一条刚上场，锚点应仍在移动起点（画布右侧）附近，而非终点
        let newest = engine.active_items.iter().find(|i| i.text == last).unwrap();
        assert!(
            newest.anchor.0 > 1900.0,
            "newest anchor should be near move start, got {:?}",
            newest.anchor
        );
    }

    fn fmt_s(secs: f64) -> String {
        let total = secs.max(0.0);
        let h = (total / 3600.0) as u32;
        let m = ((total / 60.0) as u32) % 60;
        let s = (total % 60.0) as u32;
        let cs = ((total * 100.0) as u32) % 100;
        format!("{h}:{m:02}:{s:02}.{cs:02}")
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
