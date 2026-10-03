//! 弹幕渲染器 — 使用 Glyph Atlas + R8 Alpha Mask 实现高效弹幕渲染
//!
//! 借鉴 Erika DFM+ 架构：
//! - Prepare/Frame-Query 两阶段布局（二分查找）
//! - R8 Alpha Mask 字形图集（fill + outline 分离）
//! - 单 Overlay Render Pass 批量绘制所有弹幕
//! - GPU 端描边（outline draw call）替代 CPU 8 方向描边

use crate::glyph_atlas::{
    extract_font_from_ttc, GlyphAtlas, GlyphInstance, RenderPlan, TextRasterizer,
};
use crate::layout::{LayoutEngine, PreparedLayout};
use crate::types::{DanmakuConfig, DanmakuInstance, DanmakuItem};
use bytemuck;
use parking_lot::Mutex;
use raptor_render::Overlay;
use std::collections::HashMap;
use std::sync::Arc;

/// GPU uniform 结构体 — 与 WGSL FrameUniforms 布局匹配（16 bytes）
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable, PartialEq)]
struct FrameUniformData {
    viewport: [f32; 2],
    _pad: [f32; 2],
}

/// GPU 实例属性 — 与 WGSL 里 @location(0..2) 的 instance-rate 属性逐字节对应（48 bytes）
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable, PartialEq)]
struct GpuGlyph {
    rect: [f32; 4],
    tex_rect: [f32; 4],
    color: [f32; 4],
}

impl GpuGlyph {
    /// 实例缓冲里的字节步长
    const STRIDE: wgpu::BufferAddress = std::mem::size_of::<GpuGlyph>() as wgpu::BufferAddress;

    /// 三个 instance-rate 属性的字节偏移，顺序必须与 shader 的 @location(0..2) 一致
    const ATTR_OFFSETS: [wgpu::BufferAddress; 3] = [
        std::mem::offset_of!(GpuGlyph, rect) as wgpu::BufferAddress,
        std::mem::offset_of!(GpuGlyph, tex_rect) as wgpu::BufferAddress,
        std::mem::offset_of!(GpuGlyph, color) as wgpu::BufferAddress,
    ];

    fn from_instance(inst: &GlyphInstance) -> Self {
        Self {
            rect: inst.rect,
            tex_rect: inst.tex_rect,
            color: inst.color_rgba,
        }
    }
}

/// 把一帧的 outline/fill 实例打包进暂存数组：outline 段在前、fill 段在后
///
/// 两段各自连续，绘制时可用同一缓冲的两个 slice 分别实例化绘制。
fn pack_instances(outline: &[GlyphInstance], fill: &[GlyphInstance], out: &mut Vec<GpuGlyph>) {
    out.clear();
    out.reserve(outline.len() + fill.len());
    out.extend(outline.iter().map(GpuGlyph::from_instance));
    out.extend(fill.iter().map(GpuGlyph::from_instance));
}

/// Atlas GPU 纹理缓存
struct AtlasGpuCache {
    version: u64,
    fill_texture: wgpu::Texture,
    outline_texture: wgpu::Texture,
}

/// 本帧的 GPU 绘制资源（实例缓冲 + 两个 bind group），跨帧复用
///
/// 只有实例数超过缓冲容量、或 atlas 版本变化时才重建；稳态每帧 0 次 GPU 资源创建。
struct DrawCache {
    /// 实例缓冲容量（以 glyph 实例个数计）
    capacity: usize,
    instance_buffer: wgpu::Buffer,
    uniform_buffer: wgpu::Buffer,
    /// 对应的 atlas 版本（bind group 绑定了具体纹理视图）
    atlas_version: u64,
    fill_bind_group: wgpu::BindGroup,
    outline_bind_group: wgpu::BindGroup,
}

/// 实例缓冲最小容量（个），避免首帧就反复扩容
const MIN_INSTANCE_CAPACITY: usize = 512;

/// 计算扩容后的新容量：至少容纳 `needed`，稳态下按翻倍增长
fn instance_capacity_for(current: usize, needed: usize) -> usize {
    if needed <= current {
        return current;
    }
    needed.max(current * 2).max(MIN_INSTANCE_CAPACITY)
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
    /// 预处理后的布局（在 load_items 时构建）
    pub prepared: Option<PreparedLayout>,
    /// 原始弹幕数据（viewport 变化时用于 relayout）
    pub items: Vec<DanmakuItem>,
    /// prepared 版本号：load_items/relayout 时递增，用于使字形放置缓存失效
    pub prepared_revision: u64,
}

/// 单条弹幕的字形放置缓存项
struct CachedPlacements {
    /// 写入时的 prepared 版本
    revision: u64,
    /// 字形放置列表（同一弹幕文本的所有字形，跨帧复用）
    placements: Arc<Vec<crate::glyph_atlas::TextGlyphPlacement>>,
}

/// 弹幕引擎 — 管理弹幕数据、布局、渲染
///
/// 实现 Overlay trait，可集成到 OverlayStack 中。
pub struct DanmakuEngine {
    /// 共享状态（布局线程写入，渲染线程读取）
    state: Arc<Mutex<DanmakuState>>,
    /// 渲染配置
    config: DanmakuConfig,
    /// 字体原始字节数据（TTC 已提取为独立 TTF）
    font_data: Option<Vec<u8>>,
    /// 字形图集（持久化，跨帧复用）
    glyph_atlas: GlyphAtlas,
    /// 字形光栅化器（含字形缓存）
    rasterizer: TextRasterizer,
    /// 条目级字形放置缓存：item_index → 该文本的全部字形放置
    ///
    /// 命中时跳过字体解析、逐字形查表与 atlas 打包，消除稳态每帧 CPU 开销。
    placement_cache: HashMap<usize, CachedPlacements>,
    /// wgpu 渲染管线（惰性初始化）
    render_pipeline: Option<wgpu::RenderPipeline>,
    bind_group_layout: Option<wgpu::BindGroupLayout>,
    sampler: Option<wgpu::Sampler>,
    /// 当前 surface format（用于检测 format 变化）
    surface_format: Option<wgpu::TextureFormat>,
    /// Atlas GPU 纹理缓存
    atlas_cache: Option<AtlasGpuCache>,
    /// 本帧绘制资源缓存（实例缓冲 + bind group）
    draw_cache: Option<DrawCache>,
    /// CPU 侧实例暂存数组（跨帧复用，避免每帧分配）
    instance_scratch: Vec<GpuGlyph>,
    /// Generation 计数器（seek/stop 时递增，用于使缓存失效）
    generation: u64,
    /// 每帧渲染状态诊断计数器
    frame_count: u64,
    /// 首次渲染标志（用于调试日志）
    first_render_logged: bool,
    /// 上一次渲染的 viewport 尺寸（用于检测窗口大小变化）
    last_viewport: Option<(u32, u32)>,
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
                prepared: None,
                items: Vec::new(),
                prepared_revision: 0,
            })),
            config,
            font_data: None,
            glyph_atlas: GlyphAtlas::new(),
            rasterizer: TextRasterizer::new(),
            placement_cache: HashMap::new(),
            render_pipeline: None,
            bind_group_layout: None,
            sampler: None,
            surface_format: None,
            atlas_cache: None,
            draw_cache: None,
            instance_scratch: Vec::new(),
            generation: 1,
            frame_count: 0,
            first_render_logged: false,
            last_viewport: None,
        }
    }

    /// 设置字体数据（支持 TTF/OTF/TTC 格式）
    ///
    /// TTC 文件会自动提取第一个字体为独立的 TTF 数据，
    /// 确保 `ab_glyph::FontRef::try_from_slice` 能正确解析。
    pub fn set_font(&mut self, font_data: Vec<u8>) {
        if font_data.len() >= 12 && &font_data[0..4] == b"ttcf" {
            let num_fonts = u32::from_be_bytes(font_data[8..12].try_into().unwrap_or([0, 0, 0, 0]));
            if num_fonts > 0 {
                let header_size = 12 + (num_fonts as usize) * 4;
                if font_data.len() >= header_size {
                    let offset =
                        u32::from_be_bytes(font_data[12..16].try_into().unwrap_or([0, 0, 0, 0]))
                            as usize;
                    if offset < font_data.len() {
                        // 使用 extract_font_from_ttc 正确重写偏移表
                        if let Some(extracted) = extract_font_from_ttc(&font_data, offset) {
                            tracing::info!(
                                "DanmakuEngine: TTC font detected ({} fonts), \
                                 extracted first font ({} bytes)",
                                num_fonts,
                                extracted.len()
                            );
                            self.font_data = Some(extracted);
                            self.invalidate_atlas();
                            return;
                        }
                        tracing::warn!(
                            "DanmakuEngine: TTC extraction failed, using raw slice fallback"
                        );
                    }
                }
            }
        }
        self.font_data = Some(font_data);
        self.invalidate_atlas();
    }

    /// 获取共享状态句柄（供外部线程写入弹幕数据）
    pub fn shared_state(&self) -> Arc<Mutex<DanmakuState>> {
        self.state.clone()
    }

    /// 加载弹幕数据并执行布局 + prepare
    pub fn load_items(&self, items: Vec<DanmakuItem>) {
        let engine = LayoutEngine::new(self.config.clone());
        let instances = engine.layout(&items);

        if !instances.is_empty() {
            let min_start = instances
                .iter()
                .map(|i| i.start_time)
                .fold(f64::INFINITY, f64::min);
            let max_end = instances
                .iter()
                .map(|i| i.end_time)
                .fold(f64::NEG_INFINITY, f64::max);
            tracing::info!(
                "DanmakuEngine::load_items: {} items -> {} instances, time=[{:.2}s, {:.2}s]",
                items.len(),
                instances.len(),
                min_start,
                max_end,
            );
        } else {
            tracing::warn!(
                "DanmakuEngine::load_items: {} items parsed but 0 instances produced",
                items.len()
            );
        }

        // 构建 PreparedLayout（预处理阶段）
        let prepared = engine.prepare(&instances);

        let mut state = self.state.lock();
        state.items = items;
        state.instances = instances;
        state.prepared = Some(prepared);
        state.prepared_revision += 1;
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

    /// 递增 generation（seek/stop 时调用，清除 atlas 缓存）
    pub fn invalidate(&mut self) {
        self.generation += 1;
        self.invalidate_atlas();
    }

    /// 清空 atlas 和缓存
    fn invalidate_atlas(&mut self) {
        self.glyph_atlas.clear();
        self.rasterizer.clear_cache();
        // 缓存里的 PackedGlyph 坐标指向 atlas 纹理，atlas 清空后即失效
        self.placement_cache.clear();
        self.atlas_cache = None;
        // bind group 里绑的是 atlas 纹理视图，纹理没了就必须重建
        self.draw_cache = None;
    }

    /// 确保渲染管线已初始化
    fn ensure_pipeline(&mut self, device: &wgpu::Device, surface_format: wgpu::TextureFormat) {
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
                // binding(0): R8Unorm texture
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
                // binding(1): sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // binding(2): uniform buffer (FrameUniformData)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
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
                // 顶点几何由 vertex_index 生成，只有实例数据走 instance-rate 属性缓冲
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: GpuGlyph::STRIDE,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: GpuGlyph::ATTR_OFFSETS[0],
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: GpuGlyph::ATTR_OFFSETS[1],
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: GpuGlyph::ATTR_OFFSETS[2],
                            shader_location: 2,
                        },
                    ],
                }],
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
        // bind group 与旧的 bind group layout 配套，重建管线后必须一并作废
        self.draw_cache = None;
        tracing::info!(
            "DanmakuEngine: render pipeline initialized for {:?}",
            surface_format
        );
    }

    /// 构建当前帧的渲染计划：frame query → 光栅化 → atlas pack → 生成 GlyphInstance
    fn build_render_plan_internal(
        &mut self,
        current_time: f64,
        scale_x: f32,
        scale_y: f32,
    ) -> RenderPlan {
        let state = self.state.lock();
        let Some(prepared) = state.prepared.as_ref() else {
            if self.frame_count.is_multiple_of(300) {
                tracing::warn!("build_render_plan: state.prepared is None");
            }
            return RenderPlan::empty();
        };

        let frame_layout = prepared.query_frame(current_time);
        if frame_layout.items.is_empty() {
            // 正常：当前时间无可见弹幕（不输出日志避免刷屏）
            return RenderPlan::empty();
        }

        let opacity = state.opacity;
        let revision = state.prepared_revision;

        let Some(font_data) = self.font_data.as_ref() else {
            if self.frame_count.is_multiple_of(300) {
                tracing::warn!("build_render_plan: font_data is None");
            }
            return RenderPlan::empty();
        };
        // TTC 已在 set_font 中提取为独立字体数据，直接使用
        let font_slice = font_data.as_slice();

        struct FrameGlyphs {
            frame_item_x: f32,
            frame_item_y: f32,
            color_rgba: [f32; 4],
            placements: Arc<Vec<crate::glyph_atlas::TextGlyphPlacement>>,
        }
        let mut frame_data: Vec<FrameGlyphs> = Vec::with_capacity(frame_layout.items.len());

        // Phase 1: 取得每条弹幕的字形放置（优先命中条目级缓存），
        // 未命中才光栅化并打包新字形到 atlas
        for frame_item in &frame_layout.items {
            let prepared_item = &prepared.items[frame_item.item_index];
            let color_rgba = color_u32_to_f32_array(prepared_item.color, opacity);

            let item_index = frame_item.item_index;
            let cached = self
                .placement_cache
                .get(&item_index)
                .filter(|entry| entry.revision == revision)
                .map(|entry| entry.placements.clone());

            let placements = match cached {
                Some(p) => p,
                None => {
                    let p = Arc::new(self.rasterizer.rasterize_and_pack_text(
                        font_slice,
                        &prepared_item.text,
                        prepared_item.font_size as f32,
                        &mut self.glyph_atlas,
                    ));
                    self.placement_cache.insert(
                        item_index,
                        CachedPlacements {
                            revision,
                            placements: p.clone(),
                        },
                    );
                    p
                }
            };

            if !placements.is_empty() {
                frame_data.push(FrameGlyphs {
                    frame_item_x: frame_item.x,
                    frame_item_y: frame_item.y,
                    color_rgba,
                    placements,
                });
            }
        }

        // Phase 2: 打包完成后取 atlas 快照（确保包含本帧新打包的字形）
        let atlas_snap = self.glyph_atlas.snapshot();
        let atlas_w = atlas_snap.width as f32;
        let atlas_h = atlas_snap.height as f32;

        // 诊断：query_frame 有可见弹幕但光栅化全部失败
        if frame_data.is_empty()
            && !frame_layout.items.is_empty()
            && self.frame_count.is_multiple_of(300)
        {
            tracing::warn!(
                "build_render_plan: {} visible items but rasterization produced 0 placements \
                 (font_slice_len={})",
                frame_layout.items.len(),
                font_slice.len(),
            );
        }

        // Phase 3: 使用最终 atlas 尺寸计算 UV 并生成 GlyphInstance
        let glyph_count: usize = frame_data.iter().map(|fd| fd.placements.len()).sum();
        let mut outline = Vec::with_capacity(glyph_count);
        let mut fill = Vec::with_capacity(glyph_count);

        for fd in &frame_data {
            for placement in fd.placements.iter() {
                if placement.packed.width == 0 || placement.packed.height == 0 {
                    continue;
                }

                let glyph_x = fd.frame_item_x * scale_x
                    + (placement.pen_x + placement.glyph.offset_x) * scale_x;
                let glyph_y = fd.frame_item_y * scale_y + placement.glyph.offset_y * scale_y;
                let glyph_w = placement.packed.width as f32 * scale_x;
                let glyph_h = placement.packed.height as f32 * scale_y;

                let tex_rect = [
                    placement.packed.x as f32 / atlas_w,
                    placement.packed.y as f32 / atlas_h,
                    placement.packed.width as f32 / atlas_w,
                    placement.packed.height as f32 / atlas_h,
                ];

                // Outline draw（黑色描边）
                // fill/outline bitmap 已统一为 outline 尺寸（含 dilation），
                // 两者使用相同的 rect 和 tex_rect，outline 的 dilation 已在 bitmap 中
                outline.push(GlyphInstance {
                    rect: [glyph_x, glyph_y, glyph_w, glyph_h],
                    tex_rect,
                    color_rgba: [0.0, 0.0, 0.0, fd.color_rgba[3]],
                });

                // Fill draw（彩色填充）
                fill.push(GlyphInstance {
                    rect: [glyph_x, glyph_y, glyph_w, glyph_h],
                    tex_rect,
                    color_rgba: fd.color_rgba,
                });
            }
        }

        RenderPlan {
            atlas: Some(atlas_snap),
            outline,
            fill,
        }
    }
}

impl Overlay for DanmakuEngine {
    fn update(&mut self, pts: f64) {
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

        // Phase 1: 从共享状态提取所需数据（短暂持有锁）
        // 不透明度已在 build_render_plan 里烘进实例颜色的 alpha，这里不再重复乘一次
        let (enabled, instance_count, current_pts) = {
            let state = self.state.lock();
            (state.enabled, state.instances.len(), state.current_pts)
        };

        if !enabled || instance_count == 0 {
            if self.frame_count.is_multiple_of(300) {
                tracing::info!(
                    "DanmakuEngine::render: skip frame={} enabled={} instances={}",
                    self.frame_count,
                    enabled,
                    instance_count
                );
            }
            self.frame_count += 1;
            return;
        }

        // 自适应 viewport: 检测 surface 尺寸变化，触发 relayout
        let viewport_changed = self.last_viewport != Some((surface_width, surface_height));
        if viewport_changed {
            let items_to_relayout: Option<Vec<DanmakuItem>> = {
                let state = self.state.lock();
                if !state.items.is_empty() {
                    Some(state.items.clone())
                } else {
                    None
                }
            };
            if let Some(items) = items_to_relayout {
                let new_w = surface_width as f32;
                let new_h = surface_height as f32;
                tracing::info!(
                    "DanmakuEngine: viewport changed {:?} -> {}x{}, relayout {} items",
                    self.last_viewport,
                    surface_width,
                    surface_height,
                    items.len()
                );
                let (new_instances, new_prepared) =
                    LayoutEngine::relayout(&items, new_w, new_h, &self.config);
                tracing::info!(
                    "DanmakuEngine: relayout produced {} instances, prepared {} items",
                    new_instances.len(),
                    new_prepared.items.len()
                );
                let mut state = self.state.lock();
                state.instances = new_instances;
                state.prepared = Some(new_prepared);
                state.prepared_revision += 1;
            } else {
                tracing::info!(
                    "DanmakuEngine: viewport changed {:?} -> {}x{}, but no items to relayout",
                    self.last_viewport,
                    surface_width,
                    surface_height
                );
            }
            self.config.viewport_width = surface_width as f32;
            self.config.viewport_height = surface_height as f32;
            self.last_viewport = Some((surface_width, surface_height));
        }

        // Phase 2: 构建渲染计划（CPU 工作：frame query + rasterize + atlas pack）
        // 布局已在实际像素坐标中完成，无需缩放
        let plan = self.build_render_plan_internal(current_pts, 1.0, 1.0);

        if plan.is_empty() {
            // 每 300 帧输出一次诊断：为什么 plan 为空
            if self.frame_count.is_multiple_of(300) {
                let (has_prepared, has_font) = {
                    let s = self.state.lock();
                    (s.prepared.is_some(), self.font_data.is_some())
                };
                tracing::info!(
                    "DanmakuEngine: plan EMPTY frame={} pts={:.2}s prepared={} font={} instances={}",
                    self.frame_count, current_pts, has_prepared, has_font,
                    self.state.lock().instances.len()
                );
            }
            self.frame_count += 1;
            return;
        }

        let Some(ref atlas) = plan.atlas else {
            self.frame_count += 1;
            return;
        };

        // 首次渲染日志
        if !self.first_render_logged {
            self.first_render_logged = true;
            tracing::info!(
                "DanmakuEngine: FIRST_RENDER instances={} (fill={} outline={}) atlas={}x{} v{}",
                plan.instance_count(),
                plan.fill.len(),
                plan.outline.len(),
                atlas.width,
                atlas.height,
                atlas.version,
            );
        }

        // 每 300 帧输出渲染统计
        if self.frame_count.is_multiple_of(300) && self.frame_count > 0 {
            tracing::info!(
                "DanmakuEngine: frame={} instances={} draw_calls=2 atlas_v{}",
                self.frame_count,
                plan.instance_count(),
                atlas.version,
            );
        }
        self.frame_count += 1;

        // Phase 3: 上传 atlas GPU 纹理（版本匹配则复用）
        let needs_upload = match &self.atlas_cache {
            Some(cache) => cache.version != atlas.version,
            None => true,
        };
        if needs_upload {
            let snapshot = atlas.clone();
            let ft = create_r8_texture(
                device,
                snapshot.width,
                snapshot.height,
                &[],
                snapshot.stride,
            );
            let ot = create_r8_texture(
                device,
                snapshot.width,
                snapshot.height,
                &[],
                snapshot.stride,
            );
            upload_r8_texture(
                queue,
                &ft,
                &snapshot.fill_alpha,
                snapshot.width,
                snapshot.height,
                snapshot.stride,
            );
            upload_r8_texture(
                queue,
                &ot,
                &snapshot.outline_alpha,
                snapshot.width,
                snapshot.height,
                snapshot.stride,
            );
            self.atlas_cache = Some(AtlasGpuCache {
                version: snapshot.version,
                fill_texture: ft,
                outline_texture: ot,
            });
        }
        let (fill_tex, outline_tex) = {
            let cache = self.atlas_cache.as_ref().unwrap();
            (cache.fill_texture.clone(), cache.outline_texture.clone())
        };

        // Phase 4: 借用 pipeline 引用
        let (pipeline, bg_layout, sampler) = match (
            &self.render_pipeline,
            &self.bind_group_layout,
            &self.sampler,
        ) {
            (Some(p), Some(b), Some(s)) => (p, b, s),
            _ => return,
        };

        // Phase 5: 组装实例数据（outline 段在前、fill 段在后），单个缓冲一次上传
        let outline_count = plan.outline.len();
        let fill_count = plan.fill.len();
        pack_instances(&plan.outline, &plan.fill, &mut self.instance_scratch);
        let needed = self.instance_scratch.len();
        let atlas_version = atlas.version;

        // Phase 6: 绘制资源按需重建（仅容量不足或 atlas 换版时创建）
        let cache_outdated = self
            .draw_cache
            .as_ref()
            .is_none_or(|c| c.capacity < needed || c.atlas_version != atlas_version);
        if cache_outdated {
            let old_capacity = self.draw_cache.as_ref().map_or(0, |c| c.capacity);
            let capacity = instance_capacity_for(old_capacity, needed);
            let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("danmaku_instances"),
                size: (capacity as u64) * GpuGlyph::STRIDE,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("danmaku_frame_uniform"),
                size: std::mem::size_of::<FrameUniformData>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let fill_view = fill_tex.create_view(&wgpu::TextureViewDescriptor::default());
            let outline_view = outline_tex.create_view(&wgpu::TextureViewDescriptor::default());
            let fill_bg = make_overlay_bind_group(
                device,
                bg_layout,
                sampler,
                &fill_view,
                &uniform_buffer,
                "danmaku_fill_bg",
            );
            let outline_bg = make_overlay_bind_group(
                device,
                bg_layout,
                sampler,
                &outline_view,
                &uniform_buffer,
                "danmaku_outline_bg",
            );
            self.draw_cache = Some(DrawCache {
                capacity,
                instance_buffer,
                uniform_buffer,
                atlas_version,
                fill_bind_group: fill_bg,
                outline_bind_group: outline_bg,
            });
            tracing::debug!(
                "DanmakuEngine: draw cache rebuilt capacity={} needed={} atlas_v{}",
                capacity,
                needed,
                atlas_version
            );
        }
        let cache = self
            .draw_cache
            .as_ref()
            .expect("danmaku draw cache 刚创建，不应为 None");

        queue.write_buffer(
            &cache.uniform_buffer,
            0,
            bytemuck::bytes_of(&FrameUniformData {
                viewport: [surface_width as f32, surface_height as f32],
                _pad: [0.0; 2],
            }),
        );
        queue.write_buffer(
            &cache.instance_buffer,
            0,
            bytemuck::cast_slice(&self.instance_scratch),
        );

        // 单个 overlay render pass — outline 一批 + fill 一批，共 2 次实例化 draw
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("danmaku_overlay_pass"),
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

            if outline_count > 0 {
                pass.set_bind_group(0, &cache.outline_bind_group, &[]);
                pass.set_vertex_buffer(
                    0,
                    cache
                        .instance_buffer
                        .slice(0..(outline_count as u64) * GpuGlyph::STRIDE),
                );
                pass.draw(0..4, 0..outline_count as u32);
            }
            if fill_count > 0 {
                let start = (outline_count as u64) * GpuGlyph::STRIDE;
                let end = start + (fill_count as u64) * GpuGlyph::STRIDE;
                pass.set_bind_group(0, &cache.fill_bind_group, &[]);
                pass.set_vertex_buffer(0, cache.instance_buffer.slice(start..end));
                pass.draw(0..4, 0..fill_count as u32);
            }
        }
    }

    fn is_visible(&self) -> bool {
        self.state.lock().enabled
    }
}

// ─── 辅助函数 ─────────────────────────────────────────

/// 创建一个 overlay bind group（atlas 纹理 + 采样器 + 帧 uniform）
///
/// 实例数据不走 bind group，而是 instance-rate 顶点缓冲，每帧 `set_vertex_buffer`。
fn make_overlay_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    texture: &wgpu::TextureView,
    uniform_buffer: &wgpu::Buffer,
    label: &'static str,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(texture),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: uniform_buffer.as_entire_binding(),
            },
        ],
    })
}

/// 0xRRGGBB 颜色转 [f32; 4] RGBA
fn color_u32_to_f32_array(color: u32, alpha: f32) -> [f32; 4] {
    let r = ((color >> 16) & 0xFF) as f32 / 255.0;
    let g = ((color >> 8) & 0xFF) as f32 / 255.0;
    let b = (color & 0xFF) as f32 / 255.0;
    [r, g, b, alpha]
}

/// 创建 R8Unorm 纹理
fn create_r8_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    _data: &[u8],
    _stride: usize,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("danmaku_atlas_r8"),
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
    })
}

/// 上传 R8 纹理数据
fn upload_r8_texture(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
) {
    if data.is_empty() {
        return;
    }
    // 如果 stride == width，可以直接上传
    if stride == width as usize {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    } else {
        // stride != width，需要逐行复制
        let mut tight = Vec::with_capacity((width * height) as usize);
        for row in 0..height as usize {
            let start = row * stride;
            let end = start + width as usize;
            if end <= data.len() {
                tight.extend_from_slice(&data[start..end]);
            } else {
                tight.extend(std::iter::repeat_n(0u8, width as usize));
            }
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &tight,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(x: f32) -> GlyphInstance {
        GlyphInstance {
            rect: [x, 0.0, 1.0, 1.0],
            tex_rect: [0.0, 0.0, 1.0, 1.0],
            color_rgba: [1.0, 1.0, 1.0, 1.0],
        }
    }

    #[test]
    fn pack_orders_outline_before_fill() {
        let outline = vec![glyph(1.0), glyph(2.0)];
        let fill = vec![glyph(3.0), glyph(4.0)];
        let mut out = Vec::new();
        pack_instances(&outline, &fill, &mut out);

        assert_eq!(out.len(), 4);
        assert_eq!(out[0].rect[0], 1.0);
        assert_eq!(out[1].rect[0], 2.0);
        assert_eq!(out[2].rect[0], 3.0);
        assert_eq!(out[3].rect[0], 4.0);
    }

    #[test]
    fn pack_clears_previous_frame() {
        let mut out = vec![glyph(9.0); 10]
            .iter()
            .map(GpuGlyph::from_instance)
            .collect::<Vec<_>>();
        pack_instances(&[], &[glyph(1.0)], &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rect[0], 1.0);
    }

    #[test]
    fn pack_empty_plan_yields_empty_buffer() {
        let mut out = vec![GpuGlyph::from_instance(&glyph(0.0))];
        pack_instances(&[], &[], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn capacity_grows_to_fit() {
        assert_eq!(instance_capacity_for(0, 100), MIN_INSTANCE_CAPACITY);
        assert_eq!(instance_capacity_for(512, 512), 512);
        assert_eq!(instance_capacity_for(512, 600), 1024);
        assert_eq!(instance_capacity_for(512, 2000), 2000);
    }
}
