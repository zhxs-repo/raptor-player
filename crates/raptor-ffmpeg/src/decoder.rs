use crate::types::*;
use ffmpeg_next::codec::packet::Ref;
use raptor_core::{RaptorError, Result};

/// VideoDecoder trait — 视频解码器抽象
pub trait VideoDecoder: Send {
    /// 提交数据包
    fn submit_packet(&mut self, packet: &Packet) -> Result<()>;

    /// 接收解码帧
    fn receive_frame(&mut self) -> Result<Option<VideoFrame>>;

    /// 结束流：送入空包让解码器进入 drain 模式，之后 `receive_frame` 才能吐出
    /// 内部滞留的尾部帧（B 帧重排缓冲）
    fn send_eof(&mut self) -> Result<()>;

    /// 刷新解码器（seek 后调用）
    fn flush(&mut self);
}

/// AudioDecoder trait — 音频解码器抽象
pub trait AudioDecoder: Send {
    /// 提交数据包
    fn submit_packet(&mut self, packet: &Packet) -> Result<()>;

    /// 接收解码帧
    fn receive_frame(&mut self) -> Result<Option<AudioFrame>>;

    /// 结束流：送入空包让解码器进入 drain 模式。AAC 等解码器会压着最后
    /// 1~2 帧（约 1024 采样），不 drain 就取不到
    fn send_eof(&mut self) -> Result<()>;

    /// 刷新解码器
    fn flush(&mut self);
}

/// FFmpeg 视频解码器
pub struct FfmpegVideoDecoder {
    decoder: Option<ffmpeg_next::decoder::Video>,
    pixel_format: PixelFormat,
    /// Packet/Frame 时间基 — 视频解码帧 PTS 直接沿用输入 Packet 的时间基
    pkt_timebase: ffmpeg_next::Rational,
}

impl FfmpegVideoDecoder {
    pub fn new() -> Self {
        Self {
            decoder: None,
            pixel_format: PixelFormat::Unknown,
            pkt_timebase: ffmpeg_next::Rational::new(0, 1),
        }
    }

    /// 从 CodecContext 创建已配置的解码器
    pub fn from_stream_context(ctx: ffmpeg_next::codec::Context) -> Result<Self> {
        let video = ctx
            .decoder()
            .video()
            .map_err(|e| RaptorError::Decode(format!("video decoder open: {e}")))?;
        let pf = PixelFormat::from(video.format());
        // avctx.pkt_timebase 打开时通常为 0/1（未设置），实际时间基由首个 Packet 携带
        let pkt_timebase = video.packet_time_base();
        tracing::info!(
            "FfmpegVideoDecoder from_stream_context: {}x{} {:?}, pkt_timebase={}",
            video.width(),
            video.height(),
            pf,
            pkt_timebase
        );
        Ok(Self {
            decoder: Some(video),
            pixel_format: pf,
            pkt_timebase,
        })
    }
}

impl Default for FfmpegVideoDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FfmpegVideoDecoder {
    /// 解码器上下文上的色彩元数据 — 帧未标注时的回退来源
    fn context_color(&self) -> (ColorSpace, ColorPrimaries, ColorRange) {
        match self.decoder.as_ref() {
            Some(d) => (
                ColorSpace::from(d.color_space()),
                ColorPrimaries::from(d.color_primaries()),
                ColorRange::from(d.color_range()),
            ),
            None => Default::default(),
        }
    }
}

impl VideoDecoder for FfmpegVideoDecoder {
    fn submit_packet(&mut self, packet: &Packet) -> Result<()> {
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| RaptorError::InvalidState("video decoder not configured".into()))?;
        // Packet 携带原始 tick + 时间基，直接同步给解码器，不做秒↔tick 往返
        if needs_timebase_update(self.pkt_timebase, packet.time_base) {
            decoder.set_packet_time_base(packet.time_base);
            self.pkt_timebase = packet.time_base;
        }
        let borrow = BorrowWithPts::new(&packet.data, packet.pts, packet.dts);
        decoder
            .send_packet(&borrow)
            .map_err(|e| RaptorError::Decode(format!("send_packet: {e}")))?;
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Option<VideoFrame>> {
        let (ctx_space, ctx_primaries, ctx_range) = self.context_color();
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| RaptorError::InvalidState("video decoder not configured".into()))?;
        let mut frame = ffmpeg_next::frame::Video::empty();
        match decoder.receive_frame(&mut frame) {
            Ok(()) => {
                let width = frame.width();
                let height = frame.height();
                let mut planes = Vec::new();

                let num_planes = match self.pixel_format {
                    PixelFormat::Yuv420p => 3,
                    PixelFormat::Nv12 => 2,
                    _ => 1,
                };

                for i in 0..num_planes {
                    let data = frame.data(i).to_vec();
                    let stride = frame.stride(i);
                    planes.push(PlaneData { data, stride });
                }

                // 视频解码帧 PTS 是输入 Packet PTS 的透传，时间基同为 pkt_timebase；
                // 无时间戳（NOPTS）时保持 None，不伪装成 0
                let pts = frame.pts();
                // 色彩元数据：帧上没写就退回解码器上下文（H.264/HEVC 的 SPS VUI
                // 解析结果通常只落在 ctx 上，帧未必继承）
                let color_space = match ColorSpace::from(frame.color_space()) {
                    ColorSpace::Unspecified => ctx_space,
                    s => s,
                };
                let color_primaries = match ColorPrimaries::from(frame.color_primaries()) {
                    ColorPrimaries::Unspecified => ctx_primaries,
                    p => p,
                };
                let color_range = match ColorRange::from(frame.color_range()) {
                    ColorRange::Unspecified => ctx_range,
                    r => r,
                };

                Ok(Some(VideoFrame {
                    pts,
                    time_base: self.pkt_timebase,
                    width,
                    height,
                    format: self.pixel_format,
                    color_space,
                    color_primaries,
                    color_range,
                    planes,
                }))
            }
            Err(e) if is_eagain(&e) => Ok(None),
            Err(e) => Err(RaptorError::Decode(format!("receive_frame: {e}"))),
        }
    }

    fn send_eof(&mut self) -> Result<()> {
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| RaptorError::InvalidState("video decoder not configured".into()))?;
        decoder
            .send_eof()
            .map_err(|e| RaptorError::Decode(format!("send_eof: {e}")))?;
        Ok(())
    }

    fn flush(&mut self) {
        if let Some(dec) = self.decoder.as_mut() {
            dec.flush();
        }
    }
}

/// FFmpeg 音频解码器
pub struct FfmpegAudioDecoder {
    decoder: Option<ffmpeg_next::decoder::Audio>,
    sample_format: SampleFormat,
    sample_rate: u32,
    channels: u32,
    /// Packet 时间基 — 由首个有效 Packet.time_base 同步到 avctx.pkt_timebase
    pkt_timebase: ffmpeg_next::Rational,
}

impl FfmpegAudioDecoder {
    pub fn new() -> Self {
        Self {
            decoder: None,
            sample_format: SampleFormat::Unknown,
            sample_rate: 0,
            channels: 0,
            pkt_timebase: ffmpeg_next::Rational::new(0, 1),
        }
    }

    /// 从 CodecContext 创建已配置的解码器
    pub fn from_stream_context(ctx: ffmpeg_next::codec::Context) -> Result<Self> {
        let audio = ctx
            .decoder()
            .audio()
            .map_err(|e| RaptorError::Decode(format!("audio decoder open: {e}")))?;
        let rate = audio.rate();
        let ch = audio.channels() as u32;
        let sf = SampleFormat::from(audio.format());
        let pkt_timebase = audio.packet_time_base();
        tracing::info!(
            "FfmpegAudioDecoder from_stream_context: {}Hz {}ch {:?}, pkt_timebase={}",
            rate,
            ch,
            sf,
            pkt_timebase
        );
        Ok(Self {
            decoder: Some(audio),
            sample_format: sf,
            sample_rate: rate,
            channels: ch,
            pkt_timebase,
        })
    }

    /// 解码音频帧的时间基：FFmpeg 音频解码器以采样为单位输出 PTS
    fn frame_timebase(
        sample_rate: u32,
        pkt_timebase: ffmpeg_next::Rational,
    ) -> ffmpeg_next::Rational {
        if sample_rate > 0 {
            ffmpeg_next::Rational::new(1, sample_rate as i32)
        } else {
            pkt_timebase
        }
    }
}

impl Default for FfmpegAudioDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioDecoder for FfmpegAudioDecoder {
    fn submit_packet(&mut self, packet: &Packet) -> Result<()> {
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| RaptorError::InvalidState("audio decoder not configured".into()))?;
        // 解码器需要正确的 pkt_timebase 才能把 Packet PTS 换算成采样单位的帧 PTS
        if needs_timebase_update(self.pkt_timebase, packet.time_base) {
            decoder.set_packet_time_base(packet.time_base);
            self.pkt_timebase = packet.time_base;
        }
        let borrow = BorrowWithPts::new(&packet.data, packet.pts, packet.dts);
        decoder
            .send_packet(&borrow)
            .map_err(|e| RaptorError::Decode(format!("send_packet: {e}")))?;
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Option<AudioFrame>> {
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| RaptorError::InvalidState("audio decoder not configured".into()))?;
        let mut frame = ffmpeg_next::frame::Audio::empty();
        match decoder.receive_frame(&mut frame) {
            Ok(()) => {
                // 帧 PTS 单位是 1/sample_rate（不是 Packet 时间基），无时间戳保持 None
                let pts = frame.pts();
                // AAC 等解码器输出的 AVFrame 可能不填 ch_layout / sample_rate，
                // 此时必须以解码器上下文为准，否则声道数算成 0、整帧采样被当成空
                let channels = match frame.channels() as usize {
                    0 => self.channels as usize,
                    n => n,
                };
                let sample_rate = match frame.rate() {
                    0 => self.sample_rate,
                    r => r,
                };
                let time_base = Self::frame_timebase(sample_rate, self.pkt_timebase);
                let samples = extract_audio_samples(&frame, channels);
                Ok(Some(AudioFrame {
                    pts,
                    time_base,
                    sample_rate,
                    channels: channels as u32,
                    format: self.sample_format,
                    samples,
                }))
            }
            Err(e) if is_eagain(&e) => Ok(None),
            Err(e) => Err(RaptorError::Decode(format!("receive_frame: {e}"))),
        }
    }

    fn send_eof(&mut self) -> Result<()> {
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| RaptorError::InvalidState("audio decoder not configured".into()))?;
        decoder
            .send_eof()
            .map_err(|e| RaptorError::Decode(format!("send_eof: {e}")))?;
        Ok(())
    }

    fn flush(&mut self) {
        if let Some(dec) = self.decoder.as_mut() {
            dec.flush();
        }
    }
}

/// Packet 时间基有效且与解码器已记录值不同 → 需要同步到 avctx.pkt_timebase
fn needs_timebase_update(current: ffmpeg_next::Rational, incoming: ffmpeg_next::Rational) -> bool {
    incoming.numerator() != 0
        && incoming.denominator() != 0
        && (current.numerator() != incoming.numerator()
            || current.denominator() != incoming.denominator())
}

/// BorrowWithPts — 类似 ffmpeg_next::packet::Borrow，但在 AVPacket 中设置 PTS/DTS
///
/// ffmpeg_next 的 Borrow::new() 只传递数据字节，将 AVPacket.pts 初始化为 0
/// （不等于 AV_NOPTS_VALUE），导致解码帧的 PTS 始终为 0。
/// 此结构体额外设置 pts/dts，确保解码帧继承正确的时间戳。
struct BorrowWithPts<'a> {
    packet: ffmpeg_next::ffi::AVPacket,
    _data: &'a [u8],
}

impl<'a> BorrowWithPts<'a> {
    fn new(data: &'a [u8], pts: Option<i64>, dts: Option<i64>) -> Self {
        use ffmpeg_next::ffi::*;
        unsafe {
            let mut packet: AVPacket = std::mem::zeroed();
            packet.data = data.as_ptr() as *mut _;
            packet.size = data.len() as i32;
            packet.pts = pts.unwrap_or(AV_NOPTS_VALUE);
            packet.dts = dts.unwrap_or(AV_NOPTS_VALUE);
            BorrowWithPts {
                packet,
                _data: data,
            }
        }
    }
}

impl<'a> Ref for BorrowWithPts<'a> {
    fn as_ptr(&self) -> *const ffmpeg_next::ffi::AVPacket {
        &self.packet
    }
}

impl<'a> Drop for BorrowWithPts<'a> {
    fn drop(&mut self) {
        unsafe {
            self.packet.data = std::ptr::null_mut();
            self.packet.size = 0;
            ffmpeg_next::ffi::av_packet_unref(&mut self.packet);
        }
    }
}

/// Check if ffmpeg error is EAGAIN (resource temporarily unavailable)
fn is_eagain(err: &ffmpeg_next::Error) -> bool {
    matches!(err, ffmpeg_next::Error::Other { errno } if *errno == ffmpeg_next::error::EAGAIN)
}

/// 从 FFmpeg 音频帧提取 f32 采样
///
/// `ctx_channels`：解码器上下文的声道数。AAC 等解码器输出的 AVFrame 不填
/// `ch_layout`，只按帧上的字段算声道会得到 0，整帧采样就被当成空；帧的安全
/// 访问器（`planes()` / `data()`）同样依赖 `ch_layout`，因此直接读取
/// `AVFrame` 的 `data[]` / `linesize[]`。
fn extract_audio_samples(frame: &ffmpeg_next::frame::Audio, ctx_channels: usize) -> Vec<f32> {
    let channels = match frame.channels() as usize {
        0 => ctx_channels.max(1),
        n => n,
    };
    unsafe {
        let p = frame.as_ptr();
        extract_from_planes(
            &(*p).data,
            &(*p).linesize,
            frame.format(),
            frame.samples(),
            channels,
        )
    }
}

/// 从 `AVFrame` 的数据平面提取 f32 采样
///
/// 处理 planar（每声道独立 buffer）和 packed（声道交错在 data[0]）两种布局。
/// 支持 F32/I16/I32/U8/F64 格式，统一归一化到 [-1.0, 1.0] f32 范围。
///
/// # Safety
/// `plane_ptrs` / `linesize` 必须是某个有效 `AVFrame` 的 8 项数据平面与行宽。
unsafe fn extract_from_planes(
    plane_ptrs: &[*mut u8; 8],
    linesize: &[i32; 8],
    format: ffmpeg_next::format::Sample,
    samples: usize,
    channels: usize,
) -> Vec<f32> {
    let channels = channels.max(1);
    let is_planar = format.is_planar();
    let plane_bytes = if is_planar {
        samples * format.bytes()
    } else {
        samples * channels * format.bytes()
    };
    let plane_data = |index: usize| -> &[u8] {
        match plane_ptrs.get(index).copied() {
            Some(ptr) if !ptr.is_null() => {
                let line = linesize.get(index).copied().unwrap_or(0).max(0) as usize;
                std::slice::from_raw_parts(ptr as *const u8, plane_bytes.min(line))
            }
            _ => &[],
        }
    };

    let mut output = Vec::with_capacity(samples * channels);

    if is_planar {
        // Planar: 每个声道有独立的 buffer，逐声道逐采样交错输出
        let planes: Vec<&[u8]> = (0..channels).map(plane_data).collect();
        for s in 0..samples {
            for data in planes.iter() {
                let sample = match format {
                    ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Planar) => {
                        let offset = s * 4;
                        if offset + 4 <= data.len() {
                            f32::from_ne_bytes([
                                data[offset],
                                data[offset + 1],
                                data[offset + 2],
                                data[offset + 3],
                            ])
                        } else {
                            0.0
                        }
                    }
                    ffmpeg_next::format::Sample::F64(ffmpeg_next::format::sample::Type::Planar) => {
                        let offset = s * 8;
                        if offset + 8 <= data.len() {
                            f64::from_ne_bytes([
                                data[offset],
                                data[offset + 1],
                                data[offset + 2],
                                data[offset + 3],
                                data[offset + 4],
                                data[offset + 5],
                                data[offset + 6],
                                data[offset + 7],
                            ]) as f32
                        } else {
                            0.0
                        }
                    }
                    ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Planar) => {
                        let offset = s * 2;
                        if offset + 2 <= data.len() {
                            i16::from_ne_bytes([data[offset], data[offset + 1]]) as f32
                                / i16::MAX as f32
                        } else {
                            0.0
                        }
                    }
                    ffmpeg_next::format::Sample::I32(ffmpeg_next::format::sample::Type::Planar) => {
                        let offset = s * 4;
                        if offset + 4 <= data.len() {
                            i32::from_ne_bytes([
                                data[offset],
                                data[offset + 1],
                                data[offset + 2],
                                data[offset + 3],
                            ]) as f32
                                / i32::MAX as f32
                        } else {
                            0.0
                        }
                    }
                    ffmpeg_next::format::Sample::U8(ffmpeg_next::format::sample::Type::Planar)
                        if s < data.len() =>
                    {
                        (data[s] as f32 - 128.0) / 128.0
                    }
                    _ => 0.0,
                };
                output.push(sample);
            }
        }
    } else {
        // Packed: 所有声道交错存储在 data(0)
        let data = plane_data(0);
        let total = samples * channels;
        match format {
            ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed) => {
                for i in 0..total {
                    let offset = i * 4;
                    if offset + 4 <= data.len() {
                        output.push(f32::from_ne_bytes([
                            data[offset],
                            data[offset + 1],
                            data[offset + 2],
                            data[offset + 3],
                        ]));
                    } else {
                        output.push(0.0);
                    }
                }
            }
            ffmpeg_next::format::Sample::F64(ffmpeg_next::format::sample::Type::Packed) => {
                for i in 0..total {
                    let offset = i * 8;
                    if offset + 8 <= data.len() {
                        output.push(f64::from_ne_bytes([
                            data[offset],
                            data[offset + 1],
                            data[offset + 2],
                            data[offset + 3],
                            data[offset + 4],
                            data[offset + 5],
                            data[offset + 6],
                            data[offset + 7],
                        ]) as f32);
                    } else {
                        output.push(0.0);
                    }
                }
            }
            ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed) => {
                for i in 0..total {
                    let offset = i * 2;
                    if offset + 2 <= data.len() {
                        output.push(
                            i16::from_ne_bytes([data[offset], data[offset + 1]]) as f32
                                / i16::MAX as f32,
                        );
                    } else {
                        output.push(0.0);
                    }
                }
            }
            ffmpeg_next::format::Sample::I32(ffmpeg_next::format::sample::Type::Packed) => {
                for i in 0..total {
                    let offset = i * 4;
                    if offset + 4 <= data.len() {
                        output.push(
                            i32::from_ne_bytes([
                                data[offset],
                                data[offset + 1],
                                data[offset + 2],
                                data[offset + 3],
                            ]) as f32
                                / i32::MAX as f32,
                        );
                    } else {
                        output.push(0.0);
                    }
                }
            }
            ffmpeg_next::format::Sample::U8(ffmpeg_next::format::sample::Type::Packed) => {
                for i in 0..total {
                    if i < data.len() {
                        output.push((data[i] as f32 - 128.0) / 128.0);
                    } else {
                        output.push(0.0);
                    }
                }
            }
            _ => {
                // 未知格式：输出零采样，避免噪声
                tracing::warn!(
                    "extract_audio_samples: unsupported packed format {:?}",
                    format
                );
                output.resize(total, 0.0);
            }
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 声道数缺失的解码帧（AAC 不填 ch_layout）必须按上下文声道数提取，
    /// 否则整帧采样被当成空，播放端只有静音
    #[test]
    fn extract_from_planes_ignores_frame_channel_layout() {
        use ffmpeg_next::format::sample::Type;
        use ffmpeg_next::format::Sample;

        let left: [f32; 2] = [0.5, -0.25];
        let right: [f32; 2] = [0.125, -0.5];
        let mut plane_ptrs: [*mut u8; 8] = [std::ptr::null_mut(); 8];
        let mut linesize: [i32; 8] = [0; 8];
        let plane_bytes = (left.len() * std::mem::size_of::<f32>()) as i32;
        plane_ptrs[0] = left.as_ptr() as *mut u8;
        linesize[0] = plane_bytes;

        let mono =
            unsafe { extract_from_planes(&plane_ptrs, &linesize, Sample::F32(Type::Planar), 2, 1) };
        assert_eq!(mono, vec![0.5, -0.25]);

        plane_ptrs[1] = right.as_ptr() as *mut u8;
        linesize[1] = plane_bytes;
        let stereo =
            unsafe { extract_from_planes(&plane_ptrs, &linesize, Sample::F32(Type::Planar), 2, 2) };
        assert_eq!(stereo, vec![0.5, 0.125, -0.25, -0.5], "planar 应交错输出");
    }

    #[test]
    fn test_video_decoder_new() {
        let d = FfmpegVideoDecoder::new();
        assert!(d.decoder.is_none());
    }

    #[test]
    fn test_video_decoder_receive_frame_not_ready() {
        let mut d = FfmpegVideoDecoder::new();
        let result = d.receive_frame();
        assert!(result.is_err());
    }

    #[test]
    fn test_video_decoder_submit_packet_not_ready() {
        let mut d = FfmpegVideoDecoder::new();
        let pkt = Packet {
            data: vec![],
            stream_index: 0,
            pts: None,
            dts: None,
            time_base: ffmpeg_next::Rational::new(1, 90000),
            is_key: false,
        };
        let result = d.submit_packet(&pkt);
        assert!(result.is_err());
    }

    #[test]
    fn test_video_decoder_flush_no_panic() {
        let mut d = FfmpegVideoDecoder::new();
        d.flush();
    }

    #[test]
    fn test_audio_decoder_new() {
        let d = FfmpegAudioDecoder::new();
        assert!(d.decoder.is_none());
    }

    #[test]
    fn test_audio_decoder_flush_no_panic() {
        let mut d = FfmpegAudioDecoder::new();
        d.flush();
    }

    #[test]
    fn test_needs_timebase_update() {
        let tb = ffmpeg_next::Rational::new(1, 15360);
        // 未设置（0/1）→ 需要更新
        assert!(needs_timebase_update(ffmpeg_next::Rational::new(0, 1), tb));
        // 相同 → 不更新
        assert!(!needs_timebase_update(tb, tb));
        // 无效时间基 → 不更新（不得用 0/1 覆盖已有正确基）
        assert!(!needs_timebase_update(tb, ffmpeg_next::Rational::new(0, 1)));
        assert!(!needs_timebase_update(tb, ffmpeg_next::Rational::new(5, 0)));
    }

    /// 回归：NOPTS Packet 进入 AVPacket 时必须是 AV_NOPTS_VALUE，而不是 0
    #[test]
    fn test_borrow_with_pts_keeps_nopts() {
        use ffmpeg_next::ffi::AV_NOPTS_VALUE;
        let data = vec![1u8, 2, 3];

        let borrow = BorrowWithPts::new(&data, None, None);
        let raw = unsafe { &*borrow.as_ptr() };
        assert_eq!(raw.pts, AV_NOPTS_VALUE);
        assert_eq!(raw.dts, AV_NOPTS_VALUE);

        // 真实 0 时间戳必须原样保留，与 NOPTS 区分
        let zero = BorrowWithPts::new(&data, Some(0), Some(0));
        let zero_raw = unsafe { &*zero.as_ptr() };
        assert_eq!(zero_raw.pts, 0);
        assert_eq!(zero_raw.dts, 0);
    }
}
