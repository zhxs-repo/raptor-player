use crate::types::*;
use ffmpeg_next::codec::packet::Ref;
use raptor_core::{RaptorError, Result};

/// VideoDecoder trait — 视频解码器抽象
pub trait VideoDecoder: Send {
    /// 提交数据包
    fn submit_packet(&mut self, packet: &Packet) -> Result<()>;

    /// 接收解码帧
    fn receive_frame(&mut self) -> Result<Option<VideoFrame>>;

    /// 刷新解码器（seek 后调用）
    fn flush(&mut self);
}

/// AudioDecoder trait — 音频解码器抽象
pub trait AudioDecoder: Send {
    /// 提交数据包
    fn submit_packet(&mut self, packet: &Packet) -> Result<()>;

    /// 接收解码帧
    fn receive_frame(&mut self) -> Result<Option<AudioFrame>>;

    /// 刷新解码器
    fn flush(&mut self);
}

/// FFmpeg 视频解码器
pub struct FfmpegVideoDecoder {
    decoder: Option<ffmpeg_next::decoder::Video>,
    pixel_format: PixelFormat,
    /// Packet time base — 用于将 Packet.pts(秒) 转换回 AVPacket 的 tick 单位
    pkt_timebase: ffmpeg_next::Rational,
}

impl FfmpegVideoDecoder {
    pub fn new() -> Self {
        Self {
            decoder: None,
            pixel_format: PixelFormat::Unknown,
            pkt_timebase: ffmpeg_next::Rational::new(1, 90000),
        }
    }

    /// 从 CodecContext 创建已配置的解码器
    pub fn from_stream_context(ctx: ffmpeg_next::codec::Context) -> Result<Self> {
        let video = ctx
            .decoder()
            .video()
            .map_err(|e| RaptorError::Decode(format!("video decoder open: {e}")))?;
        let pf = PixelFormat::from(video.format());
        let pkt_tb = video.packet_time_base();
        let pkt_timebase = if pkt_tb.numerator() > 0 && pkt_tb.denominator() > 0 {
            pkt_tb
        } else {
            ffmpeg_next::Rational::new(1, 90000)
        };
        tracing::info!(
            "FfmpegVideoDecoder from_stream_context: {}x{} {:?}, pkt_timebase={}/{}",
            video.width(),
            video.height(),
            pf,
            pkt_timebase.numerator(),
            pkt_timebase.denominator()
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

impl VideoDecoder for FfmpegVideoDecoder {
    fn submit_packet(&mut self, packet: &Packet) -> Result<()> {
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| RaptorError::InvalidState("video decoder not configured".into()))?;
        // 将 Packet.pts(秒) 转换回 AVPacket tick 单位
        let pts_ticks = seconds_to_av_time(packet.pts, self.pkt_timebase);
        let dts_ticks = seconds_to_av_time(packet.dts, self.pkt_timebase);
        let borrow = BorrowWithPts::new(&packet.data, pts_ticks, dts_ticks);
        decoder
            .send_packet(&borrow)
            .map_err(|e| RaptorError::Decode(format!("send_packet: {e}")))?;
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Option<VideoFrame>> {
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

                let pts = frame
                    .pts()
                    .map(|p| av_time_to_seconds(p, self.pkt_timebase))
                    .unwrap_or(0.0);

                Ok(Some(VideoFrame {
                    pts,
                    width,
                    height,
                    format: self.pixel_format,
                    planes,
                }))
            }
            Err(e) if is_eagain(&e) => Ok(None),
            Err(e) => Err(RaptorError::Decode(format!("receive_frame: {e}"))),
        }
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
    /// Packet time base — 用于将 Packet.pts(秒) 转换回 AVPacket 的 tick 单位
    pkt_timebase: ffmpeg_next::Rational,
}

impl FfmpegAudioDecoder {
    pub fn new() -> Self {
        Self {
            decoder: None,
            sample_format: SampleFormat::Unknown,
            sample_rate: 0,
            channels: 0,
            pkt_timebase: ffmpeg_next::Rational::new(1, 90000),
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
        let pkt_tb = audio.packet_time_base();
        let pkt_timebase = if pkt_tb.numerator() > 0 && pkt_tb.denominator() > 0 {
            pkt_tb
        } else {
            ffmpeg_next::Rational::new(1, 90000)
        };
        tracing::info!(
            "FfmpegAudioDecoder from_stream_context: {}Hz {}ch {:?}",
            rate,
            ch,
            sf
        );
        Ok(Self {
            decoder: Some(audio),
            sample_format: sf,
            sample_rate: rate,
            channels: ch,
            pkt_timebase,
        })
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
        let pts_ticks = seconds_to_av_time(packet.pts, self.pkt_timebase);
        let dts_ticks = seconds_to_av_time(packet.dts, self.pkt_timebase);
        let borrow = BorrowWithPts::new(&packet.data, pts_ticks, dts_ticks);
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
                let pts = frame
                    .pts()
                    .map(|p| av_time_to_seconds(p, self.pkt_timebase))
                    .unwrap_or(0.0);
                let samples = extract_audio_samples(&frame);
                Ok(Some(AudioFrame {
                    pts,
                    sample_rate: self.sample_rate,
                    channels: self.channels,
                    format: self.sample_format,
                    samples,
                }))
            }
            Err(e) if is_eagain(&e) => Ok(None),
            Err(e) => Err(RaptorError::Decode(format!("receive_frame: {e}"))),
        }
    }

    fn flush(&mut self) {
        if let Some(dec) = self.decoder.as_mut() {
            dec.flush();
        }
    }
}

/// 秒 → AVPacket tick 单位转换
fn seconds_to_av_time(secs: f64, time_base: ffmpeg_next::Rational) -> Option<i64> {
    if time_base.numerator() == 0 || time_base.denominator() == 0 {
        return None;
    }
    Some((secs * time_base.denominator() as f64 / time_base.numerator() as f64) as i64)
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
/// 处理 planar（每声道独立 buffer）和 packed（声道交错在 data(0)）两种布局。
fn extract_audio_samples(frame: &ffmpeg_next::frame::Audio) -> Vec<f32> {
    let channels = frame.channels() as usize;
    let samples = frame.samples();

    let format = frame.format();
    let is_planar = matches!(
        format,
        ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Planar)
            | ffmpeg_next::format::Sample::F64(ffmpeg_next::format::sample::Type::Planar)
            | ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Planar)
            | ffmpeg_next::format::Sample::I32(ffmpeg_next::format::sample::Type::Planar)
            | ffmpeg_next::format::Sample::U8(ffmpeg_next::format::sample::Type::Planar)
    );

    let mut output = Vec::with_capacity(samples * channels);

    if is_planar {
        // Planar: 每个声道有独立的 buffer，逐声道逐采样交错输出
        for s in 0..samples {
            for c in 0..channels {
                let data = frame.data(c);
                if data.len() >= (s + 1) * 4 {
                    let offset = s * 4;
                    let sample = f32::from_ne_bytes([
                        data[offset],
                        data[offset + 1],
                        data[offset + 2],
                        data[offset + 3],
                    ]);
                    output.push(sample);
                } else {
                    output.push(0.0);
                }
            }
        }
    } else {
        // Packed: 所有声道交错存储在 data(0)
        let data = frame.data(0);
        let bytes_per_sample = 4; // f32 = 4 bytes
        let total = samples * channels;
        for i in 0..total {
            let offset = i * bytes_per_sample;
            if offset + bytes_per_sample <= data.len() {
                let sample = f32::from_ne_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]);
                output.push(sample);
            } else {
                output.push(0.0);
            }
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

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
            pts: 0.0,
            dts: 0.0,
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
}
