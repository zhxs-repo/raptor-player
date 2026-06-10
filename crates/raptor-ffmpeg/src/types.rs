use serde::{Deserialize, Serialize};

/// 像素格式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    Yuv420p,
    Nv12,
    Rgb24,
    Bgra,
    Unknown,
}

impl From<ffmpeg_next::format::Pixel> for PixelFormat {
    fn from(p: ffmpeg_next::format::Pixel) -> Self {
        match p {
            ffmpeg_next::format::Pixel::YUV420P => PixelFormat::Yuv420p,
            ffmpeg_next::format::Pixel::NV12 => PixelFormat::Nv12,
            ffmpeg_next::format::Pixel::RGB24 => PixelFormat::Rgb24,
            ffmpeg_next::format::Pixel::BGRA => PixelFormat::Bgra,
            _ => PixelFormat::Unknown,
        }
    }
}

impl From<PixelFormat> for ffmpeg_next::format::Pixel {
    fn from(p: PixelFormat) -> Self {
        match p {
            PixelFormat::Yuv420p => ffmpeg_next::format::Pixel::YUV420P,
            PixelFormat::Nv12 => ffmpeg_next::format::Pixel::NV12,
            PixelFormat::Rgb24 => ffmpeg_next::format::Pixel::RGB24,
            PixelFormat::Bgra => ffmpeg_next::format::Pixel::BGRA,
            PixelFormat::Unknown => ffmpeg_next::format::Pixel::None,
        }
    }
}

/// 视频编解码器 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodecId {
    H264,
    H265,
    Vp9,
    Av1,
    Unknown,
}

impl From<ffmpeg_next::codec::Id> for VideoCodecId {
    fn from(id: ffmpeg_next::codec::Id) -> Self {
        match id {
            ffmpeg_next::codec::Id::H264 => VideoCodecId::H264,
            ffmpeg_next::codec::Id::H265 => VideoCodecId::H265,
            ffmpeg_next::codec::Id::VP9 => VideoCodecId::Vp9,
            ffmpeg_next::codec::Id::AV1 => VideoCodecId::Av1,
            _ => VideoCodecId::Unknown,
        }
    }
}

impl From<VideoCodecId> for ffmpeg_next::codec::Id {
    fn from(id: VideoCodecId) -> Self {
        match id {
            VideoCodecId::H264 => ffmpeg_next::codec::Id::H264,
            VideoCodecId::H265 => ffmpeg_next::codec::Id::H265,
            VideoCodecId::Vp9 => ffmpeg_next::codec::Id::VP9,
            VideoCodecId::Av1 => ffmpeg_next::codec::Id::AV1,
            VideoCodecId::Unknown => ffmpeg_next::codec::Id::None,
        }
    }
}

/// 音频编解码器 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioCodecId {
    Aac,
    Mp3,
    Opus,
    Flac,
    Unknown,
}

impl From<ffmpeg_next::codec::Id> for AudioCodecId {
    fn from(id: ffmpeg_next::codec::Id) -> Self {
        match id {
            ffmpeg_next::codec::Id::AAC => AudioCodecId::Aac,
            ffmpeg_next::codec::Id::MP3 => AudioCodecId::Mp3,
            ffmpeg_next::codec::Id::OPUS => AudioCodecId::Opus,
            ffmpeg_next::codec::Id::FLAC => AudioCodecId::Flac,
            _ => AudioCodecId::Unknown,
        }
    }
}

impl From<AudioCodecId> for ffmpeg_next::codec::Id {
    fn from(id: AudioCodecId) -> Self {
        match id {
            AudioCodecId::Aac => ffmpeg_next::codec::Id::AAC,
            AudioCodecId::Mp3 => ffmpeg_next::codec::Id::MP3,
            AudioCodecId::Opus => ffmpeg_next::codec::Id::OPUS,
            AudioCodecId::Flac => ffmpeg_next::codec::Id::FLAC,
            AudioCodecId::Unknown => ffmpeg_next::codec::Id::None,
        }
    }
}

/// 采样格式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SampleFormat {
    U8,
    I16,
    I32,
    F32,
    F64,
    U8Planar,
    I16Planar,
    I32Planar,
    F32Planar,
    F64Planar,
    Unknown,
}

impl From<ffmpeg_next::format::Sample> for SampleFormat {
    fn from(s: ffmpeg_next::format::Sample) -> Self {
        use ffmpeg_next::format::sample::Type;
        match s {
            ffmpeg_next::format::Sample::U8(Type::Packed) => SampleFormat::U8,
            ffmpeg_next::format::Sample::I16(Type::Packed) => SampleFormat::I16,
            ffmpeg_next::format::Sample::I32(Type::Packed) => SampleFormat::I32,
            ffmpeg_next::format::Sample::F32(Type::Packed) => SampleFormat::F32,
            ffmpeg_next::format::Sample::F64(Type::Packed) => SampleFormat::F64,
            ffmpeg_next::format::Sample::U8(Type::Planar) => SampleFormat::U8Planar,
            ffmpeg_next::format::Sample::I16(Type::Planar) => SampleFormat::I16Planar,
            ffmpeg_next::format::Sample::I32(Type::Planar) => SampleFormat::I32Planar,
            ffmpeg_next::format::Sample::F32(Type::Planar) => SampleFormat::F32Planar,
            ffmpeg_next::format::Sample::F64(Type::Planar) => SampleFormat::F64Planar,
            _ => SampleFormat::Unknown,
        }
    }
}

/// 视频帧平面数据
#[derive(Debug, Clone)]
pub struct PlaneData {
    pub data: Vec<u8>,
    pub stride: usize,
}

/// 解码后的视频帧
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// PTS（秒）
    pub pts: f64,
    /// 宽度
    pub width: u32,
    /// 高度
    pub height: u32,
    /// 像素格式
    pub format: PixelFormat,
    /// 平面数据（Y, U, V 或 Y, UV）
    pub planes: Vec<PlaneData>,
}

/// 解码后的音频帧
#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// PTS（秒）
    pub pts: f64,
    /// 采样率
    pub sample_rate: u32,
    /// 声道数
    pub channels: u32,
    /// 采样格式
    pub format: SampleFormat,
    /// 交错采样数据（f32）
    pub samples: Vec<f32>,
}

/// 压缩数据包
#[derive(Debug, Clone)]
pub struct Packet {
    /// 原始数据
    pub data: Vec<u8>,
    /// 流索引
    pub stream_index: usize,
    /// PTS（秒）
    pub pts: f64,
    /// DTS（秒）
    pub dts: f64,
    /// 是否为关键帧
    pub is_key: bool,
}

/// 媒体文件信息
#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub duration: f64,
    pub video_stream_index: Option<usize>,
    pub audio_stream_index: Option<usize>,
    pub video_codec_id: Option<VideoCodecId>,
    pub audio_codec_id: Option<AudioCodecId>,
    pub width: u32,
    pub height: u32,
    pub pixel_format: PixelFormat,
    pub fps: f64,
    pub sample_rate: u32,
    pub channels: u32,
    pub sample_format: SampleFormat,
}

/// AV 时间基转换
pub fn av_time_to_seconds(ts: i64, time_base: ffmpeg_next::Rational) -> f64 {
    ts as f64 * time_base.numerator() as f64 / time_base.denominator() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_codec_id_conversion() {
        assert_eq!(
            VideoCodecId::from(ffmpeg_next::codec::Id::H264),
            VideoCodecId::H264
        );
        assert_eq!(
            VideoCodecId::from(ffmpeg_next::codec::Id::H265),
            VideoCodecId::H265
        );
        assert_eq!(
            VideoCodecId::from(ffmpeg_next::codec::Id::None),
            VideoCodecId::Unknown
        );
    }

    #[test]
    fn test_reverse_video_codec_id_conversion() {
        let id: ffmpeg_next::codec::Id = VideoCodecId::H264.into();
        assert_eq!(id, ffmpeg_next::codec::Id::H264);
    }

    #[test]
    fn test_audio_codec_id_conversion() {
        assert_eq!(
            AudioCodecId::from(ffmpeg_next::codec::Id::AAC),
            AudioCodecId::Aac
        );
        assert_eq!(
            AudioCodecId::from(ffmpeg_next::codec::Id::MP3),
            AudioCodecId::Mp3
        );
    }

    #[test]
    fn test_reverse_audio_codec_id_conversion() {
        let id: ffmpeg_next::codec::Id = AudioCodecId::Aac.into();
        assert_eq!(id, ffmpeg_next::codec::Id::AAC);
    }

    #[test]
    fn test_pixel_format_conversion() {
        assert_eq!(
            PixelFormat::from(ffmpeg_next::format::Pixel::YUV420P),
            PixelFormat::Yuv420p
        );
        assert_eq!(
            PixelFormat::from(ffmpeg_next::format::Pixel::NV12),
            PixelFormat::Nv12
        );
    }

    #[test]
    fn test_sample_format_conversion() {
        assert_eq!(
            SampleFormat::from(ffmpeg_next::format::Sample::F32(
                ffmpeg_next::format::sample::Type::Packed
            )),
            SampleFormat::F32
        );
        assert_eq!(
            SampleFormat::from(ffmpeg_next::format::Sample::F32(
                ffmpeg_next::format::sample::Type::Planar
            )),
            SampleFormat::F32Planar
        );
    }

    #[test]
    fn test_av_time_to_seconds() {
        let tb = ffmpeg_next::Rational::new(1, 1000);
        let secs = av_time_to_seconds(5000, tb);
        assert!((secs - 5.0).abs() < 0.001);
    }
}
