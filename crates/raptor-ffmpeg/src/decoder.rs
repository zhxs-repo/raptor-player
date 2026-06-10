use crate::types::*;
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
}

impl FfmpegVideoDecoder {
    pub fn new() -> Self {
        Self {
            decoder: None,
            pixel_format: PixelFormat::Unknown,
        }
    }

    /// 从 CodecContext 创建已配置的解码器
    pub fn from_stream_context(ctx: ffmpeg_next::codec::Context) -> Result<Self> {
        let video = ctx
            .decoder()
            .video()
            .map_err(|e| RaptorError::Decode(format!("video decoder open: {e}")))?;
        let pf = PixelFormat::from(video.format());
        tracing::info!(
            "FfmpegVideoDecoder from_stream_context: {}x{} {:?}",
            video.width(),
            video.height(),
            pf
        );
        Ok(Self {
            decoder: Some(video),
            pixel_format: pf,
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
        let borrow = ffmpeg_next::packet::Borrow::new(&packet.data);
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

                let pts = frame.pts().map(|p| p as f64 / 1_000_000.0).unwrap_or(0.0);

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
}

impl FfmpegAudioDecoder {
    pub fn new() -> Self {
        Self {
            decoder: None,
            sample_format: SampleFormat::Unknown,
            sample_rate: 0,
            channels: 0,
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
        let borrow = ffmpeg_next::packet::Borrow::new(&packet.data);
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
                let pts = frame.pts().map(|p| p as f64 / 1_000_000.0).unwrap_or(0.0);
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

/// Check if ffmpeg error is EAGAIN (resource temporarily unavailable)
fn is_eagain(err: &ffmpeg_next::Error) -> bool {
    matches!(err, ffmpeg_next::Error::Other { errno } if *errno == ffmpeg_next::error::EAGAIN)
}

/// 从 FFmpeg 音频帧提取 f32 采样（处理 planar/packed 格式）
fn extract_audio_samples(frame: &ffmpeg_next::frame::Audio) -> Vec<f32> {
    let channels = frame.channels() as usize;
    let samples = frame.samples();

    let mut output = Vec::with_capacity(samples * channels);

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
