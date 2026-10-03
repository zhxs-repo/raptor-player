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
    /// PTS（`time_base` 单位的 tick）；`None` 表示无时间戳（AV_NOPTS_VALUE）
    pub pts: Option<i64>,
    /// `pts` 的时间基 — 视频解码帧透传输入 Packet 的时间基
    pub time_base: ffmpeg_next::Rational,
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
    /// PTS（`time_base` 单位的 tick）；`None` 表示无时间戳（AV_NOPTS_VALUE）
    pub pts: Option<i64>,
    /// `pts` 的时间基 — FFmpeg 音频解码器以 `1/sample_rate` 输出帧时间戳，
    /// 与 Packet 的时间基无关
    pub time_base: ffmpeg_next::Rational,
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
    /// PTS（`time_base` 单位的 tick）；`None` 表示无时间戳（AV_NOPTS_VALUE）
    pub pts: Option<i64>,
    /// DTS（`time_base` 单位的 tick）；`None` 表示无时间戳（AV_NOPTS_VALUE）
    pub dts: Option<i64>,
    /// `pts`/`dts` 的时间基（流的 time_base）
    pub time_base: ffmpeg_next::Rational,
    /// 是否为关键帧
    pub is_key: bool,
}

/// 字幕编解码器 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubtitleCodecId {
    /// 文本字幕（SRT / SubRip）
    SubRip,
    /// MP4 容器内文本字幕
    MovText,
    /// ASS/SSA 字幕
    Ass,
    /// 其他 / 未知
    Unknown,
}

impl From<ffmpeg_next::codec::Id> for SubtitleCodecId {
    fn from(id: ffmpeg_next::codec::Id) -> Self {
        match id {
            ffmpeg_next::codec::Id::SUBRIP => SubtitleCodecId::SubRip,
            ffmpeg_next::codec::Id::MOV_TEXT => SubtitleCodecId::MovText,
            ffmpeg_next::codec::Id::ASS => SubtitleCodecId::Ass,
            _ => SubtitleCodecId::Unknown,
        }
    }
}

/// 媒体文件信息
#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub duration: f64,
    pub video_stream_index: Option<usize>,
    pub audio_stream_index: Option<usize>,
    pub subtitle_stream_index: Option<usize>,
    pub video_codec_id: Option<VideoCodecId>,
    pub audio_codec_id: Option<AudioCodecId>,
    pub subtitle_codec_id: Option<SubtitleCodecId>,
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

/// 构造时间基（num/den），避免下游 crate 直接依赖 ffmpeg-next 类型
pub fn time_base(num: i32, den: i32) -> ffmpeg_next::Rational {
    ffmpeg_next::Rational::new(num, den)
}

/// tick → 秒；时间基无效（分子或分母为 0）时返回 `None`
pub fn ticks_to_seconds(ticks: i64, time_base: ffmpeg_next::Rational) -> Option<f64> {
    if time_base.numerator() == 0 || time_base.denominator() == 0 {
        return None;
    }
    Some(av_time_to_seconds(ticks, time_base))
}

/// 秒 → tick；时间基无效时返回 `None`
pub fn seconds_to_ticks(secs: f64, time_base: ffmpeg_next::Rational) -> Option<i64> {
    if time_base.numerator() == 0 || time_base.denominator() == 0 {
        return None;
    }
    Some((secs * time_base.denominator() as f64 / time_base.numerator() as f64) as i64)
}

impl VideoFrame {
    /// 以秒表示的 PTS；无时间戳或时间基无效时为 `None`
    pub fn pts_secs(&self) -> Option<f64> {
        self.pts.and_then(|t| ticks_to_seconds(t, self.time_base))
    }
}

impl AudioFrame {
    /// 以秒表示的 PTS；无时间戳或时间基无效时为 `None`
    pub fn pts_secs(&self) -> Option<f64> {
        self.pts.and_then(|t| ticks_to_seconds(t, self.time_base))
    }
}

impl Packet {
    /// 以秒表示的 PTS；无时间戳或时间基无效时为 `None`
    pub fn pts_secs(&self) -> Option<f64> {
        self.pts.and_then(|t| ticks_to_seconds(t, self.time_base))
    }

    /// 以秒表示的 DTS；无时间戳或时间基无效时为 `None`
    pub fn dts_secs(&self) -> Option<f64> {
        self.dts.and_then(|t| ticks_to_seconds(t, self.time_base))
    }
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

    #[test]
    fn test_av_time_to_seconds_zero() {
        let tb = ffmpeg_next::Rational::new(1, 90000);
        assert!((av_time_to_seconds(0, tb)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_reverse_pixel_format() {
        let p: ffmpeg_next::format::Pixel = PixelFormat::Yuv420p.into();
        assert_eq!(p, ffmpeg_next::format::Pixel::YUV420P);
        let p2: ffmpeg_next::format::Pixel = PixelFormat::Unknown.into();
        assert_eq!(p2, ffmpeg_next::format::Pixel::None);
    }

    #[test]
    fn test_subtitle_codec_id_conversion() {
        assert_eq!(
            SubtitleCodecId::from(ffmpeg_next::codec::Id::SUBRIP),
            SubtitleCodecId::SubRip
        );
        assert_eq!(
            SubtitleCodecId::from(ffmpeg_next::codec::Id::MOV_TEXT),
            SubtitleCodecId::MovText
        );
        assert_eq!(
            SubtitleCodecId::from(ffmpeg_next::codec::Id::None),
            SubtitleCodecId::Unknown
        );
    }

    #[test]
    fn test_video_frame_construction() {
        let frame = VideoFrame {
            pts: Some(18432),
            time_base: ffmpeg_next::Rational::new(1, 12288),
            width: 1920,
            height: 1080,
            format: PixelFormat::Yuv420p,
            planes: vec![
                PlaneData {
                    data: vec![0; 1920 * 1080],
                    stride: 1920,
                },
                PlaneData {
                    data: vec![0; 960 * 540],
                    stride: 960,
                },
                PlaneData {
                    data: vec![0; 960 * 540],
                    stride: 960,
                },
            ],
        };
        assert_eq!(frame.planes.len(), 3);
        assert_eq!(frame.width, 1920);
        assert!((frame.pts_secs().unwrap() - 1.5).abs() < 0.001);
    }

    #[test]
    fn test_audio_frame_construction() {
        let frame = AudioFrame {
            pts: Some(4410),
            time_base: ffmpeg_next::Rational::new(1, 44100),
            sample_rate: 44100,
            channels: 2,
            format: SampleFormat::F32,
            samples: vec![0.0; 1024],
        };
        assert_eq!(frame.channels, 2);
        assert_eq!(frame.samples.len(), 1024);
        assert!((frame.pts_secs().unwrap() - 0.1).abs() < 0.001);
    }

    #[test]
    fn test_packet_construction() {
        let pkt = Packet {
            data: vec![1, 2, 3],
            stream_index: 0,
            pts: Some(6144),
            dts: Some(5120),
            time_base: ffmpeg_next::Rational::new(1, 12288),
            is_key: true,
        };
        assert!(pkt.is_key);
        assert_eq!(pkt.data.len(), 3);
        assert!((pkt.pts_secs().unwrap() - 0.5).abs() < 0.001);
        assert!((pkt.dts_secs().unwrap() - 5120.0 / 12288.0).abs() < 0.001);
    }

    /// 回归：NOPTS 帧不能被兜底成 ts=0
    #[test]
    fn nopts_frame_has_no_seconds() {
        let frame = VideoFrame {
            pts: None,
            time_base: ffmpeg_next::Rational::new(1, 90000),
            width: 320,
            height: 240,
            format: PixelFormat::Yuv420p,
            planes: vec![],
        };
        assert_eq!(frame.pts, None);
        assert_eq!(frame.pts_secs(), None);

        let pkt = Packet {
            data: vec![0xAA],
            stream_index: 0,
            pts: None,
            dts: None,
            time_base: ffmpeg_next::Rational::new(1, 90000),
            is_key: false,
        };
        assert_eq!(pkt.pts_secs(), None);
        assert_eq!(pkt.dts_secs(), None);
    }

    /// 真实 0 时间戳必须与 NOPTS 区分开
    #[test]
    fn zero_timestamp_is_not_nopts() {
        let frame = VideoFrame {
            pts: Some(0),
            time_base: ffmpeg_next::Rational::new(1, 90000),
            width: 320,
            height: 240,
            format: PixelFormat::Yuv420p,
            planes: vec![],
        };
        assert_eq!(frame.pts_secs(), Some(0.0));
    }

    /// 时间基无效（0/1，裸流常见）时不得产出 NaN/伪秒数
    #[test]
    fn invalid_time_base_yields_none() {
        let tb = ffmpeg_next::Rational::new(0, 1);
        assert_eq!(ticks_to_seconds(1000, tb), None);
        assert_eq!(seconds_to_ticks(1.0, tb), None);

        let pkt = Packet {
            data: vec![],
            stream_index: 0,
            pts: Some(1000),
            dts: Some(1000),
            time_base: tb,
            is_key: false,
        };
        assert_eq!(pkt.pts_secs(), None);
        assert_eq!(pkt.dts_secs(), None);
    }

    #[test]
    fn ticks_seconds_roundtrip() {
        let tb = ffmpeg_next::Rational::new(1, 15360);
        let secs = ticks_to_seconds(1536, tb).unwrap();
        assert!((secs - 0.1).abs() < 1e-9);
        assert_eq!(seconds_to_ticks(secs, tb), Some(1536));
    }

    #[test]
    fn test_pixel_format_serde_roundtrip() {
        let fmt = PixelFormat::Nv12;
        let json = serde_json::to_string(&fmt).unwrap();
        let fmt2: PixelFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(fmt, fmt2);
    }

    #[test]
    fn test_video_codec_id_serde_roundtrip() {
        let id = VideoCodecId::H264;
        let json = serde_json::to_string(&id).unwrap();
        let id2: VideoCodecId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, id2);
    }

    #[test]
    fn test_unknown_codec_fallback() {
        let id = VideoCodecId::from(ffmpeg_next::codec::Id::MPEG4);
        assert_eq!(id, VideoCodecId::Unknown);
    }
}
