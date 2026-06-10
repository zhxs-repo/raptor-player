use thiserror::Error;

/// Raptor 错误码 — C ABI 返回值（负数 = 错误）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ErrorCode {
    Ok = 0,
    InvalidArgument = -1,
    InvalidState = -2,
    FileNotFound = -3,
    DemuxError = -4,
    DecodeError = -5,
    RenderError = -6,
    AudioError = -7,
    PipelineError = -8,
    Internal = -99,
}

/// 命令执行结果
#[derive(Debug)]
pub enum CommandResult {
    /// 无返回值
    Empty,
    /// 返回媒体信息
    MediaInfo(MediaInfo),
}

/// 媒体信息
#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub duration: f64,
    pub video: Option<crate::VideoInfo>,
    pub audio: Option<crate::AudioInfo>,
}

/// Raptor 错误类型
#[derive(Debug, Error)]
pub enum RaptorError {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("file not found: {0}")]
    FileNotFound(String),
    #[error("demux error: {0}")]
    Demux(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("render error: {0}")]
    Render(String),
    #[error("audio error: {0}")]
    Audio(String),
    #[error("pipeline error: {0}")]
    Pipeline(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl RaptorError {
    pub fn error_code(&self) -> ErrorCode {
        match self {
            Self::InvalidArgument(_) => ErrorCode::InvalidArgument,
            Self::InvalidState(_) => ErrorCode::InvalidState,
            Self::FileNotFound(_) => ErrorCode::FileNotFound,
            Self::Demux(_) => ErrorCode::DemuxError,
            Self::Decode(_) => ErrorCode::DecodeError,
            Self::Render(_) => ErrorCode::RenderError,
            Self::Audio(_) => ErrorCode::AudioError,
            Self::Pipeline(_) => ErrorCode::PipelineError,
            Self::Internal(_) => ErrorCode::Internal,
        }
    }
}

/// Raptor Result 类型
pub type Result<T> = std::result::Result<T, RaptorError>;
