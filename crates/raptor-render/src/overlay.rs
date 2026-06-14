//! Overlay trait and OverlayStack — 视频帧之上的叠加层抽象
//!
//! 合成顺序：video → subtitle → danmaku → (OSD, 预留)
//! 每个 Overlay 在每帧渲染时被调用，使用 wgpu encoder 进行叠加绘制。

use parking_lot::Mutex;
use std::sync::Arc;
use wgpu;

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
    /// `surface_width`/`surface_height` 是 surface 的实际尺寸（像素）。
    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        surface_format: wgpu::TextureFormat,
        surface_width: u32,
        surface_height: u32,
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
        surface_width: u32,
        surface_height: u32,
    ) {
        for overlay in &mut self.overlays {
            if overlay.is_visible() {
                overlay.render(
                    device,
                    queue,
                    encoder,
                    target,
                    surface_format,
                    surface_width,
                    surface_height,
                );
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
        surface_width: u32,
        surface_height: u32,
    ) {
        self.inner.lock().render(
            device,
            queue,
            encoder,
            target,
            surface_format,
            surface_width,
            surface_height,
        );
    }

    fn is_visible(&self) -> bool {
        self.inner.lock().is_visible()
    }
}
