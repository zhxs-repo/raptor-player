//! raptor-subtitle — 字幕引擎
//!
//! 支持 SRT / ASS / SSA 字幕格式解析与 wgpu overlay 渲染。
//! 实现 raptor_render::Overlay trait，可集成到 OverlayStack。

pub mod font;
pub mod parser;
pub mod renderer;
pub mod types;

pub use font::load_system_font;
pub use parser::{AssParser, SrtParser, SubtitleParser};
pub use renderer::{SubtitleEngine, SubtitleState};
pub use types::{SubtitleConfig, SubtitleEvent, SubtitleTrack};
