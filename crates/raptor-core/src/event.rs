use serde::{Deserialize, Serialize};

/// 播放器事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RaptorEvent {
    /// 文件加载完成
    FileLoaded {
        duration: f64,
        video: Option<VideoInfo>,
        audio: Option<AudioInfo>,
    },
    /// 文件播放结束
    EndFile { reason: EndReason },
    /// 错误
    Error { code: i32, message: String },
    /// Seek 完成
    Seek { from: f64, to: f64 },
    /// 属性变更
    PropertyChange { name: String, value: String },
    /// 播放重启（resume 后触发）
    PlaybackRestart,
    /// 播放器终止
    End,
}

/// 文件结束原因
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum EndReason {
    Eof,
    Error,
    Stop,
}

/// 视频流信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub fps: f64,
}

/// 音频流信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioInfo {
    pub codec: String,
    pub channels: u32,
    pub sample_rate: u32,
}
