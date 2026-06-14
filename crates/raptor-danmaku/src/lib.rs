//! raptor-danmaku — 弹幕引擎
//!
//! 参考 Bilibili DanmakuFlameMaster (DFM) 架构：
//! - B 站 XML / JSON 弹幕格式解析
//! - 轨道碰撞布局算法
//! - ab_glyph + wgpu 文本渲染
//! - 实现 raptor_render::Overlay trait

pub mod layout;
pub mod parser;
pub mod renderer;
pub mod types;

pub use layout::LayoutEngine;
pub use parser::{BilibiliXmlParser, DanmakuParser, JsonParser};
pub use renderer::{DanmakuEngine, DanmakuState};
pub use types::{DanmakuConfig, DanmakuInstance, DanmakuItem, DanmakuMode};
