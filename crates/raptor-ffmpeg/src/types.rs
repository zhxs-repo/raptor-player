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

/// Y′CbCr → R′G′B′ 矩阵系数（帧上未标注时为 `Unspecified`，见 `VideoFrame::yuv_matrix`）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorSpace {
    #[default]
    Unspecified,
    Bt601,
    Bt709,
    Bt2020,
}

/// 色域 primaries — 只保留矩阵回退判定需要的区分
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorPrimaries {
    #[default]
    Unspecified,
    Bt601,
    Bt709,
    Bt2020,
    /// 其余（Film / SMPTE 240M / P3 等）：不足以决定矩阵，交给分辨率启发式
    Other,
}

/// 采样量程
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorRange {
    #[default]
    Unspecified,
    /// 16..235 / 128±112（MPEG / TV）
    Limited,
    /// 0..255（JPEG / PC）
    Full,
}

/// 解析后的矩阵 — 渲染端唯一需要的形式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
    Bt2020,
}

impl YuvMatrix {
    /// 与 `yuv_to_rgb.wgsl` 里 `ColorParams.matrix` 的取值一一对应
    pub fn shader_index(self) -> u32 {
        match self {
            YuvMatrix::Bt601 => 0,
            YuvMatrix::Bt709 => 1,
            YuvMatrix::Bt2020 => 2,
        }
    }
}

impl From<ffmpeg_next::color::Space> for ColorSpace {
    fn from(space: ffmpeg_next::color::Space) -> Self {
        use ffmpeg_next::color::Space as S;
        match space {
            S::BT709 => ColorSpace::Bt709,
            S::BT470BG | S::SMPTE170M | S::FCC => ColorSpace::Bt601,
            S::BT2020NCL | S::BT2020CL => ColorSpace::Bt2020,
            _ => ColorSpace::Unspecified,
        }
    }
}

impl From<ffmpeg_next::color::Primaries> for ColorPrimaries {
    fn from(p: ffmpeg_next::color::Primaries) -> Self {
        use ffmpeg_next::color::Primaries as P;
        match p {
            P::BT709 => ColorPrimaries::Bt709,
            P::BT470BG | P::SMPTE170M => ColorPrimaries::Bt601,
            P::BT2020 => ColorPrimaries::Bt2020,
            P::Unspecified => ColorPrimaries::Unspecified,
            _ => ColorPrimaries::Other,
        }
    }
}

impl From<ffmpeg_next::color::Range> for ColorRange {
    fn from(r: ffmpeg_next::color::Range) -> Self {
        match r {
            ffmpeg_next::color::Range::MPEG => ColorRange::Limited,
            ffmpeg_next::color::Range::JPEG => ColorRange::Full,
            _ => ColorRange::Unspecified,
        }
    }
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
    /// Y′CbCr 矩阵系数（未标注为 `Unspecified`）
    pub color_space: ColorSpace,
    /// 色域 primaries（未标注为 `Unspecified`）
    pub color_primaries: ColorPrimaries,
    /// 采样量程（未标注为 `Unspecified`）
    pub color_range: ColorRange,
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

    /// 渲染用矩阵：帧上的 matrix → primaries → 分辨率，逐层回退
    ///
    /// 码流经常只写 primaries 不写 matrix，或两者都不写（老 DVD、多数 MP4）。
    /// 这时按 ITU-R BT.1358 的惯例：高清用 BT.709，标清用 BT.601。
    pub fn yuv_matrix(&self) -> YuvMatrix {
        match self.color_space {
            ColorSpace::Bt601 => YuvMatrix::Bt601,
            ColorSpace::Bt709 => YuvMatrix::Bt709,
            ColorSpace::Bt2020 => YuvMatrix::Bt2020,
            ColorSpace::Unspecified => match self.color_primaries {
                ColorPrimaries::Bt601 => YuvMatrix::Bt601,
                ColorPrimaries::Bt709 => YuvMatrix::Bt709,
                ColorPrimaries::Bt2020 => YuvMatrix::Bt2020,
                ColorPrimaries::Unspecified | ColorPrimaries::Other => {
                    if self.width >= 1280 || self.height >= 720 {
                        YuvMatrix::Bt709
                    } else {
                        YuvMatrix::Bt601
                    }
                }
            },
        }
    }

    /// 是否 full range（0..255）。YUV 内容未标注时按 limited（16..235），
    /// 与 FFmpeg / ITU-R 的默认一致
    pub fn is_full_range(&self) -> bool {
        matches!(self.color_range, ColorRange::Full)
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
            color_space: ColorSpace::Bt709,
            color_primaries: ColorPrimaries::Bt709,
            color_range: ColorRange::Limited,
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
            color_space: ColorSpace::Unspecified,
            color_primaries: ColorPrimaries::Unspecified,
            color_range: ColorRange::Unspecified,
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
            color_space: ColorSpace::Unspecified,
            color_primaries: ColorPrimaries::Unspecified,
            color_range: ColorRange::Unspecified,
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

    fn frame_with(
        width: u32,
        height: u32,
        space: ColorSpace,
        primaries: ColorPrimaries,
        range: ColorRange,
    ) -> VideoFrame {
        VideoFrame {
            pts: None,
            time_base: time_base(1, 90000),
            width,
            height,
            format: PixelFormat::Yuv420p,
            color_space: space,
            color_primaries: primaries,
            color_range: range,
            planes: vec![],
        }
    }

    /// 帧上写了 matrix 就直接用，不被 primaries 或分辨率覆盖
    #[test]
    fn yuv_matrix_prefers_frame_matrix() {
        let f = frame_with(
            1920,
            1080,
            ColorSpace::Bt601,
            ColorPrimaries::Bt709,
            ColorRange::Limited,
        );
        assert_eq!(f.yuv_matrix(), YuvMatrix::Bt601);
    }

    /// 缺 matrix 时按 primaries 回退（HD 流常只写 primaries）
    #[test]
    fn yuv_matrix_falls_back_to_primaries() {
        let f = frame_with(
            640,
            360,
            ColorSpace::Unspecified,
            ColorPrimaries::Bt709,
            ColorRange::Unspecified,
        );
        assert_eq!(f.yuv_matrix(), YuvMatrix::Bt709);

        let f = frame_with(
            1920,
            1080,
            ColorSpace::Unspecified,
            ColorPrimaries::Bt2020,
            ColorRange::Unspecified,
        );
        assert_eq!(f.yuv_matrix(), YuvMatrix::Bt2020);
    }

    /// matrix 与 primaries 都没写时用分辨率启发式：高清 709、标清 601
    #[test]
    fn yuv_matrix_falls_back_to_resolution() {
        let hd = frame_with(
            1280,
            720,
            ColorSpace::Unspecified,
            ColorPrimaries::Unspecified,
            ColorRange::Unspecified,
        );
        assert_eq!(hd.yuv_matrix(), YuvMatrix::Bt709);

        let sd = frame_with(
            640,
            480,
            ColorSpace::Unspecified,
            ColorPrimaries::Unspecified,
            ColorRange::Unspecified,
        );
        assert_eq!(sd.yuv_matrix(), YuvMatrix::Bt601);
    }

    /// 未标注量程按 limited（与 FFmpeg / ITU-R 默认一致），只有明确标 JPEG 才是 full
    #[test]
    fn full_range_only_when_marked() {
        for range in [ColorRange::Unspecified, ColorRange::Limited] {
            let f = frame_with(640, 480, ColorSpace::Bt601, ColorPrimaries::Bt601, range);
            assert!(!f.is_full_range(), "{range:?} 应按 limited 解释");
        }
        let f = frame_with(
            640,
            480,
            ColorSpace::Bt601,
            ColorPrimaries::Bt601,
            ColorRange::Full,
        );
        assert!(f.is_full_range());
    }

    /// FFmpeg 色彩枚举到内部枚举的映射：未覆盖的一律 Unspecified，交给回退链
    #[test]
    fn ffmpeg_color_enums_map_to_internal() {
        use ffmpeg_next::color::{Primaries as FP, Range as FR, Space as FS};
        assert_eq!(ColorSpace::from(FS::BT709), ColorSpace::Bt709);
        assert_eq!(ColorSpace::from(FS::SMPTE170M), ColorSpace::Bt601);
        assert_eq!(ColorSpace::from(FS::BT470BG), ColorSpace::Bt601);
        assert_eq!(ColorSpace::from(FS::BT2020NCL), ColorSpace::Bt2020);
        assert_eq!(ColorSpace::from(FS::BT2020CL), ColorSpace::Bt2020);
        assert_eq!(ColorSpace::from(FS::SMPTE240M), ColorSpace::Unspecified);
        assert_eq!(ColorSpace::from(FS::YCGCO), ColorSpace::Unspecified);

        assert_eq!(
            ColorPrimaries::from(FP::BT470BG),
            ColorPrimaries::Bt601,
            "PAL primaries 属 BT.601 系"
        );
        assert_eq!(ColorPrimaries::from(FP::SMPTE432), ColorPrimaries::Other);
        assert_eq!(
            ColorPrimaries::from(FP::Unspecified),
            ColorPrimaries::Unspecified
        );

        assert_eq!(ColorRange::from(FR::MPEG), ColorRange::Limited);
        assert_eq!(ColorRange::from(FR::JPEG), ColorRange::Full);
        assert_eq!(ColorRange::from(FR::Unspecified), ColorRange::Unspecified);
    }

    /// shader_index 是与 WGSL `ColorParams.matrix` 的约定，取值不得改动
    #[test]
    fn shader_index_matches_wgsl_contract() {
        assert_eq!(YuvMatrix::Bt601.shader_index(), 0);
        assert_eq!(YuvMatrix::Bt709.shader_index(), 1);
        assert_eq!(YuvMatrix::Bt2020.shader_index(), 2);
    }
}
