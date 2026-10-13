pub mod clock;
pub mod external_renderer;
pub mod font_util;
pub mod overlay;
pub mod video_output;
pub mod wgpu_instance;
mod yuv_pipeline;

/// Winit/window renderer — desktop only.
///
/// Android gets its surface from Flutter's SurfaceView (see [external_renderer]),
/// so winit is pure dead weight there. It is not merely unused: winit's
/// `android-native-activity` feature drags in `android-activity`, whose glue
/// references `android_main` -- the entry point of a native-activity *app*, which
/// a cdylib plugin never is. Keeping it in the Android build leaves the .so with
/// an unresolvable symbol that only shows up as a runtime dlopen failure, and it
/// makes `-Wl,--no-undefined` (the guard that catches mistyped FFI declarations
/// at link time) unusable. See crates/raptor-ffi/build.rs.
#[cfg(not(target_os = "android"))]
pub mod wgpu_renderer;

pub use clock::OverlayClock;
pub use external_renderer::ExternalRenderer;
pub use font_util::extract_font_from_ttc;
pub use overlay::{fit_video_rect, Overlay, OverlayArea, OverlayStack, SharedOverlay, VideoRect};
pub use video_output::{HudStats, SurfaceHandle, VideoOutput};
pub use wgpu_instance::install_nonfatal_error_handler;

#[cfg(not(target_os = "android"))]
pub use wgpu_renderer::WgpuRenderer;
