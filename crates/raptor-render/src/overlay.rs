//! Overlay trait and OverlayStack — 视频帧之上的叠加层抽象
//!
//! 合成顺序：video → subtitle → danmaku → (OSD, 预留)
//! 每个 Overlay 在每帧渲染时被调用，使用 wgpu encoder 进行叠加绘制。

use parking_lot::Mutex;
use std::sync::Arc;
use wgpu;

/// 画面矩形 `(x, y, w, h)` — surface 物理像素坐标
///
/// 等比适配（letterbox / pillarbox）后的视频实际显示区域。
pub type VideoRect = (f32, f32, f32, f32);

/// 把视频等比适配到 surface，返回画面矩形
///
/// 视频 pass 与 overlay **必须共用这一份结果**：一旦两边各自算，字幕/弹幕就会
/// 与画面错位（pillarbox 时叠加层落到黑边上）。
///
/// 视频尺寸未知（首帧之前）时退化为整个 surface。
pub fn fit_video_rect(surf_w: u32, surf_h: u32, video_w: u32, video_h: u32) -> VideoRect {
    let (sw, sh) = (surf_w as f32, surf_h as f32);
    if video_w == 0 || video_h == 0 || sw <= 0.0 || sh <= 0.0 {
        return (0.0, 0.0, sw, sh);
    }
    let (vw, vh) = (video_w as f32, video_h as f32);
    // 等比：取两轴较小值，保证画面既不变形也完整可见
    let scale = (sw / vw).min(sh / vh);
    let w = (vw * scale).round();
    let h = (vh * scale).round();
    (((sw - w) / 2.0).round(), ((sh - h) / 2.0).round(), w, h)
}

/// 一帧的画布几何信息
///
/// 刻意区分两类尺寸，字幕/弹幕的两条缩放链各取所需：
///
/// - **surface 尺寸**：整个渲染目标的物理像素。弹幕 shader 用它做"像素 → NDC"。
/// - **画面矩形**：视频等比适配后的区域。**位置**必须映射到这里，否则 pillarbox
///   时叠加层会跑到黑边上。
/// - **视频原生分辨率**：字号的**唯一**基准。窗口缩放、分屏、旋转都不改变它，
///   所以文字的物理大小保持恒定（方案 A：内容基准）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayArea {
    /// surface 实际尺寸（物理像素）
    pub surface_width: u32,
    pub surface_height: u32,
    /// 视频画面矩形（letterbox 之后）
    pub video_rect: VideoRect,
    /// 视频原生分辨率；0 = 未知（首帧之前）
    pub video_width: u32,
    pub video_height: u32,
}

impl OverlayArea {
    /// 由 surface 与视频原生分辨率构造，画面矩形自动等比计算
    pub fn new(
        surface_width: u32,
        surface_height: u32,
        video_width: u32,
        video_height: u32,
    ) -> Self {
        Self {
            surface_width,
            surface_height,
            video_rect: fit_video_rect(surface_width, surface_height, video_width, video_height),
            video_width,
            video_height,
        }
    }

    /// 全屏退化：无视频信息时（或测试里）让画面矩形等于 surface
    pub fn full_surface(surface_width: u32, surface_height: u32) -> Self {
        Self {
            surface_width,
            surface_height,
            video_rect: (0.0, 0.0, surface_width as f32, surface_height as f32),
            video_width: surface_width,
            video_height: surface_height,
        }
    }

    /// 画面矩形宽度
    pub fn rect_width(&self) -> f32 {
        self.video_rect.2
    }

    /// 画面矩形高度
    pub fn rect_height(&self) -> f32 {
        self.video_rect.3
    }

    /// **内容基准**的字号缩放系数：只取决于视频原生高度，与窗口尺寸无关
    ///
    /// 这是"拖动窗口时字幕/弹幕字形大小不变"的唯一来源。用窗口高度当基准会让
    /// 字号跟着窗口缩放；用画面矩形高度当基准则会在 letterbox（竖屏手机播横屏
    /// 视频）时把文字放大到超出画面。
    ///
    /// 视频分辨率未知时返回 1.0（视为参考分辨率）——此时按 surface 推算只会得到
    /// 一个立刻会被作废的中间值，不如直接用基准值稳定一帧。
    pub fn content_scale(&self, reference_height: f32) -> f32 {
        if reference_height <= 0.0 || self.video_height == 0 {
            return 1.0;
        }
        self.video_height as f32 / reference_height
    }
}

/// Overlay trait — 叠加层抽象
///
/// 实现者需要在 `render()` 中使用 alpha blending 将自己的内容绘制到 target 上。
/// `update()` 在渲染线程每帧调用，用于更新内部状态（如字幕 bitmap、弹幕位置）。
pub trait Overlay: Send {
    /// 每帧更新状态（在 render 之前调用，渲染线程上）
    fn update(&mut self, pts: f64);

    /// 渲染叠加层
    ///
    /// 实现者应该使用 alpha blending 绘制到 target 上。
    ///
    /// `area` 携带本帧的画布几何：surface 尺寸供 shader 做像素 → NDC，画面矩形
    /// 供定位，视频原生分辨率供计算字号（见 [OverlayArea] 的字段说明）。
    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        surface_format: wgpu::TextureFormat,
        area: OverlayArea,
    );

    /// 是否可见（不可见时跳过 render）
    fn is_visible(&self) -> bool {
        true
    }
}

/// OverlayStack — 管理多个叠加层，按序合成
///
/// 合成顺序与添加顺序一致（先添加的先绘制，后添加的覆盖在上面）。
pub struct OverlayStack {
    overlays: Vec<Box<dyn Overlay>>,
}

impl OverlayStack {
    pub fn new() -> Self {
        Self {
            overlays: Vec::new(),
        }
    }

    /// 添加一个叠加层（后添加的在更上层）
    pub fn push(&mut self, overlay: Box<dyn Overlay>) {
        self.overlays.push(overlay);
    }

    /// 获取叠加层数量
    pub fn len(&self) -> usize {
        self.overlays.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.overlays.is_empty()
    }

    /// 更新所有叠加层
    pub fn update_all(&mut self, pts: f64) {
        for overlay in &mut self.overlays {
            overlay.update(pts);
        }
    }

    /// 渲染所有可见的叠加层
    pub fn render_all(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        surface_format: wgpu::TextureFormat,
        area: OverlayArea,
    ) {
        for overlay in &mut self.overlays {
            if overlay.is_visible() {
                overlay.render(device, queue, encoder, target, surface_format, area);
            }
        }
    }
}

impl Default for OverlayStack {
    fn default() -> Self {
        Self::new()
    }
}

/// SharedOverlay — 共享所有权 Overlay 包装器
///
/// 将 `Arc<Mutex<T>>` 包装为 Overlay，允许多个持有者共享同一个引擎实例。
/// FFI 层持有 `Arc<Mutex<T>>` 用于运行时更新（加载文件、切换开关），
/// 同时 SharedOverlay 被移入 OverlayStack 供渲染线程使用。
pub struct SharedOverlay<T: Overlay + 'static> {
    inner: Arc<Mutex<T>>,
}

impl<T: Overlay + 'static> SharedOverlay<T> {
    pub fn new(inner: Arc<Mutex<T>>) -> Self {
        Self { inner }
    }
}

impl<T: Overlay + 'static> Overlay for SharedOverlay<T> {
    fn update(&mut self, pts: f64) {
        self.inner.lock().update(pts);
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
        self.inner
            .lock()
            .render(device, queue, encoder, target, surface_format, area);
    }

    fn is_visible(&self) -> bool {
        self.inner.lock().is_visible()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16:9 视频铺满 16:9 surface：画面矩形 = 整个 surface
    #[test]
    fn rect_matches_surface_when_aspects_agree() {
        let r = fit_video_rect(1920, 1080, 1920, 1080);
        assert_eq!(r, (0.0, 0.0, 1920.0, 1080.0));
    }

    /// 超宽窗口放高视频：左右留黑边（pillarbox），画面必须垂直居中
    #[test]
    fn pillarbox_centers_vertically() {
        // 1920x600 的扁窗口播 1080p：受高度限制，画面不满宽 → 左右黑边
        let (x, y, w, h) = fit_video_rect(1920, 600, 1920, 1080);
        assert_eq!(h, 600.0, "受高度限制时画面应占满高度");
        assert!(w < 1920.0, "宽视频在扁窗口应左右留边，实际 w={w}");
        // 两侧留边最多差 1px：偏移是对留白宽度取整后落地的
        assert!((x - (1920.0 - x - w)).abs() <= 1.0, "x={x} w={w}");
        assert_eq!(y, 0.0);
    }

    /// 竖屏 surface 播横屏视频：上下留黑边（letterbox）
    #[test]
    fn letterbox_centers_vertically() {
        let (_x, y, w, h) = fit_video_rect(1080, 2340, 1920, 1080);
        assert!(y > 0.0, "横视频在竖屏应上下留边，实际 y={y}");
        assert!((y - (2340.0 - y - h)).abs() <= 1.0, "y={y} h={h}");
        // 宽高比必须保持 —— 分轴缩放会把画面压扁。
        // 容忍 ±1px：fit_video_rect 对边长取整，绝对误差至多半个像素。
        let aspect = w / h;
        let expected = 1920.0 / 1080.0;
        assert!(
            (aspect - expected).abs() < 0.005,
            "aspect={aspect} expected≈{expected}（误差应仅为取整）"
        );
    }

    /// 画面矩形必须完整落在 surface 内
    #[test]
    fn rect_always_inside_surface() {
        for (sw, sh) in [(1920u32, 1080u32), (1080, 2340), (2560, 1080), (640, 480)] {
            for (vw, vh) in [(1920u32, 1080u32), (1280, 720), (640, 480), (3840, 2160)] {
                let (x, y, w, h) = fit_video_rect(sw, sh, vw, vh);
                assert!(
                    x >= 0.0 && y >= 0.0,
                    "surface {sw}x{sh} video {vw}x{vh}: x={x} y={y}"
                );
                assert!(x + w <= sw as f32 + 0.5, "surface {sw}x{sh}: x+w={}", x + w);
                assert!(y + h <= sh as f32 + 0.5, "surface {sw}x{sh}: y+h={}", y + h);
            }
        }
    }

    /// 视频尺寸未知（首帧之前）时退化到全屏，不能返回 0 尺寸
    #[test]
    fn zero_video_falls_back_to_full_surface() {
        let r = fit_video_rect(1080, 2340, 0, 0);
        assert_eq!(r, (0.0, 0.0, 1080.0, 2340.0));
    }

    /// 核心约束：字号只跟视频原生分辨率走，窗口怎么变都不动
    #[test]
    fn content_scale_is_independent_of_surface_and_rect() {
        let reference = 1080.0;

        // 同一个 1080p 视频，放在各种尺寸的窗口/画面矩形里
        let windows = [(1920u32, 1080u32), (1080, 2340), (2560, 1440), (640, 360)];
        let scales: Vec<f32> = windows
            .iter()
            .map(|&(sw, sh)| OverlayArea::new(sw, sh, 1920, 1080).content_scale(reference))
            .collect();
        for s in &scales {
            assert!(
                (*s - 1.0).abs() < 1e-6,
                "1080p 视频在任意窗口下都应得到 1.0，实际 {s}"
            );
        }

        // 4K 视频在任何窗口下都是 2.0（放大显示也不会变大）
        assert!(
            (OverlayArea::new(1920, 1080, 3840, 2160).content_scale(reference) - 2.0).abs() < 1e-6
        );
        // 720p 视频在任何窗口下都是 720/1080
        let s720 = OverlayArea::new(3840, 2160, 1280, 720).content_scale(reference);
        assert!((s720 - 720.0 / 1080.0).abs() < 1e-6, "s720={s720}");
    }

    /// 视频分辨率未知时按基准分辨率处理，不能除零也不能退化到跟随窗口
    #[test]
    fn content_scale_guards_unknown_video() {
        let area = OverlayArea::new(1920, 2340, 0, 0);
        assert_eq!(area.content_scale(1080.0), 1.0);
        assert_eq!(area.content_scale(0.0), 1.0, "reference <= 0 必须防除零");
    }
}
