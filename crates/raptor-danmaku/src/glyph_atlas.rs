//! R8 Alpha Mask Glyph Atlas — 字形图集
//!
//! 将多个字形的 alpha mask 打包到一张 2048xN 的 R8 纹理中（fill + outline 各一张）。
//! 借鉴 Erika 的 PersistentGlyphAtlas 设计：
//! - 行式打包（cursor_x 从左到右，row_height 取当前行最高字形）
//! - 相同字形只打包一次（通过 GlyphKey 去重）
//! - 版本化管理（version 变化时 GPU 侧重新上传纹理）

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use std::collections::HashMap;
use std::sync::Arc;

// TTC 提取逻辑上移至 raptor-render 共用（字幕引擎同样需要）
pub use raptor_render::font_util::extract_font_from_ttc;

/// Atlas 固定宽度（像素）
const ATLAS_WIDTH: u32 = 2048;
/// Atlas 初始高度（像素）
const ATLAS_INITIAL_HEIGHT: u32 = 256;
/// 描边扩展像素
pub(crate) const OUTLINE_RADIUS: f32 = 1.5;

/// 字形缓存键 — 唯一标识一个字形的光栅化结果
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    /// Unicode 字符
    pub ch: char,
    /// 字体大小（毫像素，避免浮点精度问题）
    pub font_size_milli: u32,
}

impl GlyphKey {
    pub fn new(ch: char, font_size: f32) -> Self {
        Self {
            ch,
            font_size_milli: (font_size.max(1.0) * 1000.0).round() as u32,
        }
    }

    pub fn font_size(&self) -> f32 {
        self.font_size_milli as f32 / 1000.0
    }
}

/// 单个字形的光栅化结果（只有 alpha 通道，不含颜色）
#[derive(Debug, Clone)]
pub struct RasterizedGlyph {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    /// 填充区域 alpha（字形本体）
    pub fill_alpha: Vec<u8>,
    /// 描边区域 alpha（扩展 OUTLINE_RADIUS 像素）
    pub outline_alpha: Vec<u8>,
    /// 基线偏移 x
    pub offset_x: f32,
    /// 基线偏移 y
    pub offset_y: f32,
    /// 字符前进宽度
    pub advance: f32,
}

impl RasterizedGlyph {
    fn empty(advance: f32) -> Self {
        Self {
            width: 0,
            height: 0,
            stride: 0,
            fill_alpha: Vec::new(),
            outline_alpha: Vec::new(),
            offset_x: 0.0,
            offset_y: 0.0,
            advance,
        }
    }

    fn has_bitmap(&self) -> bool {
        self.width > 0 && self.height > 0 && self.stride >= self.width as usize
    }

    fn required_len(&self) -> usize {
        self.stride.saturating_mul(self.height as usize)
    }
}

/// 字形在 atlas 中的位置
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedGlyph {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// 字形图集快照 — 可被 Arc 共享给渲染线程
#[derive(Debug, Clone)]
pub struct GlyphAtlasSnapshot {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub fill_alpha: Vec<u8>,
    pub outline_alpha: Vec<u8>,
    pub version: u64,
}

impl GlyphAtlasSnapshot {
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.stride >= self.width as usize
            && self.fill_alpha.len() >= self.stride * self.height as usize
            && self.outline_alpha.len() >= self.stride * self.height as usize
    }
}

/// 持久字形图集 — 行式打包，动态扩展高度
pub struct GlyphAtlas {
    width: u32,
    height: u32,
    stride: usize,
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
    fill_alpha: Vec<u8>,
    outline_alpha: Vec<u8>,
    packed: HashMap<GlyphKey, PackedGlyph>,
    version: u64,
    dirty: bool,
    snapshot: Option<Arc<GlyphAtlasSnapshot>>,
}

impl GlyphAtlas {
    pub fn new() -> Self {
        let width = ATLAS_WIDTH;
        let height = ATLAS_INITIAL_HEIGHT;
        let stride = width as usize;
        Self {
            width,
            height,
            stride,
            cursor_x: 0,
            cursor_y: 0,
            row_height: 0,
            fill_alpha: vec![0; stride * height as usize],
            outline_alpha: vec![0; stride * height as usize],
            packed: HashMap::new(),
            version: 1,
            dirty: true,
            snapshot: None,
        }
    }

    /// 打包一个字形到 atlas，返回其在 atlas 中的位置
    pub fn pack(&mut self, glyph: &RasterizedGlyph, key: &GlyphKey) -> PackedGlyph {
        // 已打包过则直接返回
        if let Some(packed) = self.packed.get(key).copied() {
            return packed;
        }

        let gw = glyph.width.min(self.width);
        let gh = glyph.height;

        if gw == 0 || gh == 0 {
            return PackedGlyph {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            };
        }

        // 当前行放不下，换行
        if self.cursor_x + gw > self.width {
            self.cursor_x = 0;
            self.cursor_y += self.row_height;
            self.row_height = 0;
        }

        // 高度不够，翻倍扩展
        while self.cursor_y + gh > self.height {
            let new_height = self.height * 2;
            let new_len = self.stride * new_height as usize;
            self.fill_alpha.resize(new_len, 0);
            self.outline_alpha.resize(new_len, 0);
            self.height = new_height;
        }

        let px = self.cursor_x;
        let py = self.cursor_y;

        // 复制 fill alpha
        if glyph.has_bitmap() && glyph.fill_alpha.len() >= glyph.required_len() {
            for row in 0..gh {
                let src_start = row as usize * glyph.stride;
                let src_end = src_start + gw as usize;
                let dst_start = (py + row) as usize * self.stride + px as usize;
                let dst_end = dst_start + gw as usize;
                if src_end <= glyph.fill_alpha.len() && dst_end <= self.fill_alpha.len() {
                    self.fill_alpha[dst_start..dst_end]
                        .copy_from_slice(&glyph.fill_alpha[src_start..src_end]);
                }
            }
        }

        // 复制 outline alpha
        if glyph.has_bitmap() && glyph.outline_alpha.len() >= glyph.required_len() {
            for row in 0..gh {
                let src_start = row as usize * glyph.stride;
                let src_end = src_start + gw as usize;
                let dst_start = (py + row) as usize * self.stride + px as usize;
                let dst_end = dst_start + gw as usize;
                if src_end <= glyph.outline_alpha.len() && dst_end <= self.outline_alpha.len() {
                    self.outline_alpha[dst_start..dst_end]
                        .copy_from_slice(&glyph.outline_alpha[src_start..src_end]);
                }
            }
        }

        self.cursor_x += gw;
        self.row_height = self.row_height.max(gh);
        self.dirty = true;

        let packed = PackedGlyph {
            x: px,
            y: py,
            width: gw,
            height: gh,
        };
        self.packed.insert(key.clone(), packed);
        packed
    }

    /// 生成快照（仅在 dirty 时生成新的 Arc）
    pub fn snapshot(&mut self) -> Arc<GlyphAtlasSnapshot> {
        if self.dirty || self.snapshot.is_none() {
            self.snapshot = Some(Arc::new(GlyphAtlasSnapshot {
                width: self.width,
                height: self.height,
                stride: self.stride,
                fill_alpha: self.fill_alpha.clone(),
                outline_alpha: self.outline_alpha.clone(),
                version: self.version,
            }));
            self.dirty = false;
            self.version += 1;
        }
        self.snapshot.clone().unwrap()
    }

    /// 清空 atlas（seek/stop 时调用）
    pub fn clear(&mut self) {
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.row_height = 0;
        self.fill_alpha.fill(0);
        self.outline_alpha.fill(0);
        self.packed.clear();
        self.snapshot = None;
        self.dirty = true;
        self.version += 1;
    }
}

impl Default for GlyphAtlas {
    fn default() -> Self {
        Self::new()
    }
}

// ─── 字形光栅化器 ─────────────────────────────────────────

/// 字形光栅化器 — 使用 ab_glyph 将字符光栅化为 fill/outline alpha
pub struct TextRasterizer {
    /// 字形缓存（已光栅化但未打包到 atlas）
    glyph_cache: HashMap<GlyphKey, Arc<RasterizedGlyph>>,
}

impl TextRasterizer {
    pub fn new() -> Self {
        Self {
            glyph_cache: HashMap::new(),
        }
    }

    /// 光栅化单个字形（fill + outline 分离）
    pub fn rasterize_glyph(&mut self, font_data: &[u8], key: GlyphKey) -> Arc<RasterizedGlyph> {
        // 缓存命中
        if let Some(glyph) = self.glyph_cache.get(&key) {
            return glyph.clone();
        }

        let font_size = key.font_size();
        let glyph = rasterize_single_glyph(font_data, key.ch, font_size);
        let glyph = Arc::new(glyph);
        self.glyph_cache.insert(key, glyph.clone());
        glyph
    }

    /// 光栅化整段文本并打包到 atlas，返回字形放置信息列表
    pub fn rasterize_and_pack_text(
        &mut self,
        font_data: &[u8],
        text: &str,
        font_size: f32,
        atlas: &mut GlyphAtlas,
    ) -> Vec<TextGlyphPlacement> {
        let scale = PxScale::from(font_size);
        let font = match FontRef::try_from_slice(font_data) {
            Ok(f) => f,
            Err(e) => {
                // 仅首次输出错误日志，避免刷屏
                if !FONT_PARSE_ERROR_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    tracing::error!(
                        "rasterize_and_pack_text: FontRef::try_from_slice FAILED: {} \
                         (font_data_len={}, first_4_bytes={:?})",
                        e,
                        font_data.len(),
                        if font_data.len() >= 4 {
                            &font_data[0..4]
                        } else {
                            font_data
                        },
                    );
                }
                return Vec::new();
            }
        };
        let scaled = font.as_scaled(scale);

        let mut pen_x = 0.0f32;
        let mut placements = Vec::new();

        for ch in text.chars() {
            let advance = scaled.h_advance(scaled.glyph_id(ch));
            let key = GlyphKey::new(ch, font_size);
            let rasterized = self.rasterize_glyph(font_data, key.clone());
            let packed = atlas.pack(&rasterized, &key);

            placements.push(TextGlyphPlacement {
                glyph: rasterized,
                packed,
                pen_x,
            });
            pen_x += advance;
        }

        placements
    }

    /// 清空缓存
    pub fn clear_cache(&mut self) {
        self.glyph_cache.clear();
    }
}

impl Default for TextRasterizer {
    fn default() -> Self {
        Self::new()
    }
}

/// 单字形光栅化结果缓存标志（全局单次，用于避免重复输出错误日志）
static FONT_PARSE_ERROR_LOGGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 文本中一个字形的放置信息
#[derive(Debug, Clone)]
pub struct TextGlyphPlacement {
    /// 字形光栅化结果
    pub glyph: Arc<RasterizedGlyph>,
    /// 字形在 atlas 中的位置
    pub packed: PackedGlyph,
    /// 字形在文本中的 x 偏移
    pub pen_x: f32,
}

/// 单帧渲染实例 — 对应一次 draw call
#[derive(Debug, Clone)]
pub struct GlyphInstance {
    /// 屏幕像素位置 [x, y, w, h]
    pub rect: [f32; 4],
    /// atlas 中的 UV 区域 [u, v, uw, vh]（归一化坐标）
    pub tex_rect: [f32; 4],
    /// RGBA 颜色
    pub color_rgba: [f32; 4],
}

/// 单帧渲染计划
#[derive(Debug, Clone)]
pub struct RenderPlan {
    /// Atlas 快照（GPU 纹理来源）
    pub atlas: Option<Arc<GlyphAtlasSnapshot>>,
    /// 描边实例（整批先画，压在填充之下）
    pub outline: Vec<GlyphInstance>,
    /// 填充实例
    pub fill: Vec<GlyphInstance>,
}

impl RenderPlan {
    pub fn empty() -> Self {
        Self {
            atlas: None,
            outline: Vec::new(),
            fill: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.fill.is_empty()
    }

    /// 实例总数（outline + fill），即本帧要上传的 GPU 实例数
    pub fn instance_count(&self) -> usize {
        self.outline.len() + self.fill.len()
    }
}

// ─── 内部辅助函数 ─────────────────────────────────────────

/// 光栅化单个字形为 fill + outline alpha
fn rasterize_single_glyph(font_data: &[u8], ch: char, font_size: f32) -> RasterizedGlyph {
    let font = match FontRef::try_from_slice(font_data) {
        Ok(f) => f,
        Err(_) => return RasterizedGlyph::empty(0.0),
    };

    let scale = PxScale::from(font_size);
    let scaled = font.as_scaled(scale);
    let glyph = scaled.scaled_glyph(ch);
    let advance = scaled.h_advance(glyph.id);

    let Some(outlined) = scaled.outline_glyph(glyph) else {
        return RasterizedGlyph::empty(advance);
    };

    let bounds = outlined.px_bounds();
    let glyph_width = (bounds.max.x - bounds.min.x).ceil().max(0.0) as u32;
    let glyph_height = (bounds.max.y - bounds.min.y).ceil().max(0.0) as u32;

    if glyph_width == 0 || glyph_height == 0 {
        return RasterizedGlyph::empty(advance);
    }

    // outline 区域扩展 OUTLINE_RADIUS 像素
    let outline_ext = OUTLINE_RADIUS.ceil() as u32;
    let outline_w = glyph_width + outline_ext * 2;
    let outline_h = glyph_height + outline_ext * 2;

    let fill_stride = glyph_width as usize;
    let outline_stride = outline_w as usize;

    let mut fill_alpha = vec![0u8; fill_stride * glyph_height as usize];
    let mut outline_alpha = vec![0u8; outline_stride * outline_h as usize];

    // 填充 alpha
    outlined.draw(|gx, gy, coverage| {
        let px = gx as usize;
        let py = gy as usize;
        if px < fill_stride && py < glyph_height as usize {
            let idx = py * fill_stride + px;
            let alpha = (coverage * 255.0).round() as u8;
            fill_alpha[idx] = fill_alpha[idx].max(alpha);
        }
    });

    // 描边 alpha：扩展字形区域（用 dilation 模拟）
    // 简单实现：将 fill_alpha 向四周扩展 OUTLINE_RADIUS 像素
    let ext = OUTLINE_RADIUS.ceil() as usize;
    outlined.draw(|gx, gy, coverage| {
        let base_px = gx as i32;
        let base_py = gy as i32;
        let alpha = (coverage * 255.0).round() as u8;

        // 在扩展区域内绘制
        for dy in -(ext as i32)..=(ext as i32) {
            for dx in -(ext as i32)..=(ext as i32) {
                // 曼哈顿距离近似（圆形扩展）
                let dist_sq = (dx * dx + dy * dy) as f32;
                if dist_sq > OUTLINE_RADIUS * OUTLINE_RADIUS {
                    continue;
                }
                let px = (base_px + dx + ext as i32) as usize;
                let py = (base_py + dy + ext as i32) as usize;
                if px < outline_stride && py < outline_h as usize {
                    let idx = py * outline_stride + px;
                    outline_alpha[idx] = outline_alpha[idx].max(alpha);
                }
            }
        }
    });

    // 将 fill bitmap 零填充到 outline 尺寸（两者使用相同尺寸打包到 atlas）
    // fill 字形居中放置在 (ext, ext) 偏移处，外围为零（透明）
    // outline 保留完整的 dilation 扩展（由上面的 draw 循环生成）
    let pack_w = outline_w;
    let pack_h = outline_h;
    let pack_stride = outline_stride;

    let mut padded_fill = vec![0u8; pack_stride * pack_h as usize];
    for y in 0..glyph_height as usize {
        let src_start = y * fill_stride;
        let src_end = src_start + fill_stride;
        let dst_start = (y + ext) * pack_stride + ext;
        let dst_end = dst_start + fill_stride;
        if src_end <= fill_alpha.len() && dst_end <= padded_fill.len() {
            padded_fill[dst_start..dst_end].copy_from_slice(&fill_alpha[src_start..src_end]);
        }
    }

    RasterizedGlyph {
        width: pack_w,
        height: pack_h,
        stride: pack_stride,
        fill_alpha: padded_fill,
        outline_alpha,
        // offset 需要减去 ext，因为 bitmap 现在比原始字形大了 ext 像素
        offset_x: bounds.min.x - ext as f32,
        offset_y: bounds.min.y - ext as f32,
        advance,
    }
}

// ─── 测试 ─────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glyph_key_creation() {
        let key = GlyphKey::new('中', 25.0);
        assert_eq!(key.ch, '中');
        assert_eq!(key.font_size_milli, 25000);
        assert!((key.font_size() - 25.0).abs() < 0.001);
    }

    #[test]
    fn test_atlas_new() {
        let atlas = GlyphAtlas::new();
        assert_eq!(atlas.width, ATLAS_WIDTH);
        assert_eq!(atlas.height, ATLAS_INITIAL_HEIGHT);
        assert_eq!(atlas.cursor_x, 0);
        assert_eq!(atlas.cursor_y, 0);
    }

    #[test]
    fn test_atlas_pack_empty_glyph() {
        let mut atlas = GlyphAtlas::new();
        let empty = RasterizedGlyph::empty(10.0);
        let key = GlyphKey::new(' ', 25.0);
        let packed = atlas.pack(&empty, &key);
        assert_eq!(packed.width, 0);
        assert_eq!(packed.height, 0);
    }

    #[test]
    fn test_atlas_pack_and_dedup() {
        let mut atlas = GlyphAtlas::new();

        // 创建一个简单的 4x4 字形
        let glyph = RasterizedGlyph {
            width: 4,
            height: 4,
            stride: 4,
            fill_alpha: vec![128; 16],
            outline_alpha: vec![64; 16],
            offset_x: 0.0,
            offset_y: 0.0,
            advance: 5.0,
        };

        let key = GlyphKey::new('A', 20.0);
        let p1 = atlas.pack(&glyph, &key);
        let p2 = atlas.pack(&glyph, &key);

        // 相同 key 应该返回相同位置
        assert_eq!(p1, p2);
        assert_eq!(p1.x, 0);
        assert_eq!(p1.y, 0);
        assert_eq!(p1.width, 4);
        assert_eq!(p1.height, 4);
    }

    #[test]
    fn test_atlas_row_packing() {
        let mut atlas = GlyphAtlas::new();

        // 打包多个字形，应该按行排列
        for i in 0..3 {
            let glyph = RasterizedGlyph {
                width: 10,
                height: 10,
                stride: 10,
                fill_alpha: vec![200; 100],
                outline_alpha: vec![100; 100],
                offset_x: 0.0,
                offset_y: 0.0,
                advance: 12.0,
            };
            let key = GlyphKey::new(char::from_u32('A' as u32 + i).unwrap(), 20.0);
            let packed = atlas.pack(&glyph, &key);
            // 每个字形应该在同一行（y=0），x 依次递增
            assert_eq!(packed.y, 0);
            assert_eq!(packed.x, i * 10);
        }
    }

    #[test]
    fn test_atlas_snapshot() {
        let mut atlas = GlyphAtlas::new();
        let s1 = atlas.snapshot();
        assert_eq!(s1.version, 1);

        // 无变化时返回相同快照
        let s2 = atlas.snapshot();
        assert_eq!(s2.version, 1);
        assert!(Arc::ptr_eq(&s1, &s2));

        // 打包后变 dirty，生成新快照
        let glyph = RasterizedGlyph {
            width: 2,
            height: 2,
            stride: 2,
            fill_alpha: vec![255; 4],
            outline_alpha: vec![128; 4],
            offset_x: 0.0,
            offset_y: 0.0,
            advance: 3.0,
        };
        atlas.pack(&glyph, &GlyphKey::new('X', 10.0));
        let s3 = atlas.snapshot();
        assert_eq!(s3.version, 2);
        assert!(!Arc::ptr_eq(&s1, &s3));
    }

    #[test]
    fn test_atlas_clear() {
        let mut atlas = GlyphAtlas::new();
        let glyph = RasterizedGlyph {
            width: 2,
            height: 2,
            stride: 2,
            fill_alpha: vec![255; 4],
            outline_alpha: vec![128; 4],
            offset_x: 0.0,
            offset_y: 0.0,
            advance: 3.0,
        };
        atlas.pack(&glyph, &GlyphKey::new('A', 10.0));
        assert_eq!(atlas.cursor_x, 2);

        atlas.clear();
        assert_eq!(atlas.cursor_x, 0);
        assert_eq!(atlas.cursor_y, 0);
        assert!(atlas.packed.is_empty());
    }

    #[test]
    fn test_render_plan_empty() {
        let plan = RenderPlan::empty();
        assert!(plan.is_empty());
        assert!(plan.atlas.is_none());
    }
}
