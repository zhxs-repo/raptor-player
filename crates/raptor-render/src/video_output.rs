//! 视频输出的平台无关抽象
//!
//! 这些类型原本和 `WgpuRenderer` 放在同一个模块里，但那是个 winit 专属模块：
//! Android 上完全用不到 winit（Surface 来自 Flutter 的 `SurfaceView`），所以
//! `wgpu_renderer` 被整体 `#[cfg(not(target_os = "android"))]` 掉了。
//! `VideoOutput` / `SurfaceHandle` / `HudStats` 是 Android 也要用的（由
//! `ExternalRenderer` 实现、由 FFI 层调用），必须留在与平台无关的模块里。

use raptor_core::Result;
use raptor_ffmpeg::VideoFrame;

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
