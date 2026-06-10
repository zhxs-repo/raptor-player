use crate::types::*;
use raptor_core::{RaptorError, Result};

/// Demuxer trait — 解复用器抽象
pub trait Demuxer: Send {
    /// 打开媒体文件
    fn open(&mut self, url: &str) -> Result<()>;

    /// 读取下一个数据包
    fn read_packet(&mut self) -> Result<Option<Packet>>;

    /// 跳转到指定时间（秒）
    fn seek(&mut self, target: f64) -> Result<()>;

    /// 获取媒体信息（open 后可用）
    fn info(&self) -> Option<&MediaInfo>;

    /// 获取视频流的 CodecContext（用于解码器初始化）
    fn take_video_codec_context(&mut self) -> Option<ffmpeg_next::codec::Context>;

    /// 获取音频流的 CodecContext
    fn take_audio_codec_context(&mut self) -> Option<ffmpeg_next::codec::Context>;

    /// 关闭文件
    fn close(&mut self);
}

/// FFmpeg 解复用器实现
pub struct FfmpegDemuxer {
    input: Option<ffmpeg_next::format::context::Input>,
    info: Option<MediaInfo>,
    video_codec_context: Option<ffmpeg_next::codec::Context>,
    audio_codec_context: Option<ffmpeg_next::codec::Context>,
    packet_iter: Option<ffmpeg_next::format::context::input::PacketIter<'static>>,
}

// Safety: FfmpegDemuxer is only used from one thread at a time
unsafe impl Send for FfmpegDemuxer {}

impl FfmpegDemuxer {
    pub fn new() -> Self {
        Self {
            input: None,
            info: None,
            video_codec_context: None,
            audio_codec_context: None,
            packet_iter: None,
        }
    }
}

impl Default for FfmpegDemuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl Demuxer for FfmpegDemuxer {
    fn open(&mut self, url: &str) -> Result<()> {
        tracing::info!("FfmpegDemuxer::open({})", url);

        let mut input = ffmpeg_next::format::input(&url)
            .map_err(|e| RaptorError::Demux(format!("failed to open {}: {}", url, e)))?;

        // duration is in AV_TIME_BASE (microseconds)
        let duration = input.duration() as f64 / 1_000_000.0;
        let mut video_stream_index = None;
        let mut audio_stream_index = None;
        let mut width = 0u32;
        let mut height = 0u32;
        let mut pixel_format = PixelFormat::Unknown;
        let mut fps = 0.0;
        let mut video_codec_id = None;
        let mut audio_codec_id = None;
        let mut sample_rate = 0u32;
        let mut channels = 0u32;
        let mut sample_format = SampleFormat::Unknown;

        for (i, stream) in input.streams().enumerate() {
            let params = stream.parameters();
            match params.medium() {
                ffmpeg_next::media::Type::Video => {
                    if video_stream_index.is_none() {
                        video_stream_index = Some(i);
                        video_codec_id = Some(VideoCodecId::from(params.id()));

                        // Extract video params from raw AVCodecParameters
                        unsafe {
                            let codecpar = (*stream.as_ptr()).codecpar;
                            width = (*codecpar).width as u32;
                            height = (*codecpar).height as u32;
                            let av_fmt: ffmpeg_next::ffi::AVPixelFormat =
                                std::mem::transmute((*codecpar).format);
                            pixel_format =
                                PixelFormat::from(ffmpeg_next::format::Pixel::from(av_fmt));
                        }

                        fps = stream.avg_frame_rate().numerator() as f64
                            / stream.avg_frame_rate().denominator().max(1) as f64;

                        // Create codec context for decoder initialization
                        let ctx = ffmpeg_next::codec::Context::from_parameters(params)
                            .map_err(|e| RaptorError::Demux(format!("video context: {e}")))?;
                        self.video_codec_context = Some(ctx);
                    }
                }
                ffmpeg_next::media::Type::Audio if audio_stream_index.is_none() => {
                    audio_stream_index = Some(i);
                    audio_codec_id = Some(AudioCodecId::from(params.id()));

                    // Extract audio params from raw AVCodecParameters
                    unsafe {
                        let codecpar = (*stream.as_ptr()).codecpar;
                        sample_rate = (*codecpar).sample_rate as u32;
                        channels = (*codecpar).ch_layout.nb_channels as u32;
                        let av_fmt: ffmpeg_next::ffi::AVSampleFormat =
                            std::mem::transmute((*codecpar).format);
                        sample_format =
                            SampleFormat::from(ffmpeg_next::format::Sample::from(av_fmt));
                    }

                    let ctx = ffmpeg_next::codec::Context::from_parameters(params)
                        .map_err(|e| RaptorError::Demux(format!("audio context: {e}")))?;
                    self.audio_codec_context = Some(ctx);
                }
                _ => {}
            }
        }

        let info = MediaInfo {
            duration,
            video_stream_index,
            audio_stream_index,
            video_codec_id,
            audio_codec_id,
            width,
            height,
            pixel_format,
            fps,
            sample_rate,
            channels,
            sample_format,
        };

        tracing::info!(
            "opened media: duration={:.2}s, video={}x{}, audio={}Hz {}ch",
            info.duration,
            if video_stream_index.is_some() {
                width
            } else {
                0
            },
            if video_stream_index.is_some() {
                height
            } else {
                0
            },
            if audio_stream_index.is_some() {
                sample_rate
            } else {
                0
            },
            if audio_stream_index.is_some() {
                channels
            } else {
                0
            },
        );

        self.info = Some(info);

        // Create packet iterator from input
        // Safety: we transmute the lifetime to 'static, but we ensure the iterator
        // is dropped before input by storing both in the same struct.
        let iter = input.packets();
        self.packet_iter = Some(unsafe {
            std::mem::transmute::<
                ffmpeg_next::format::context::input::PacketIter<'_>,
                ffmpeg_next::format::context::input::PacketIter<'static>,
            >(iter)
        });
        self.input = Some(input);
        Ok(())
    }

    fn read_packet(&mut self) -> Result<Option<Packet>> {
        let iter = self
            .packet_iter
            .as_mut()
            .ok_or_else(|| RaptorError::InvalidState("demuxer not opened".into()))?;

        match iter.next() {
            Some((stream, packet)) => {
                let time_base = stream.time_base();
                let pkt = Packet {
                    data: packet.data().unwrap_or_default().to_vec(),
                    stream_index: stream.index(),
                    pts: av_time_to_seconds(packet.pts().unwrap_or(0), time_base),
                    dts: av_time_to_seconds(packet.dts().unwrap_or(0), time_base),
                    is_key: packet.is_key(),
                };
                Ok(Some(pkt))
            }
            None => Ok(None), // EOF
        }
    }

    fn seek(&mut self, target: f64) -> Result<()> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| RaptorError::InvalidState("demuxer not opened".into()))?;

        let ts = (target * 1_000_000.0) as i64;
        input
            .seek(ts, i64::MIN..i64::MAX)
            .map_err(|e| RaptorError::Demux(format!("seek: {e}")))?;
        Ok(())
    }

    fn info(&self) -> Option<&MediaInfo> {
        self.info.as_ref()
    }

    fn take_video_codec_context(&mut self) -> Option<ffmpeg_next::codec::Context> {
        self.video_codec_context.take()
    }

    fn take_audio_codec_context(&mut self) -> Option<ffmpeg_next::codec::Context> {
        self.audio_codec_context.take()
    }

    fn close(&mut self) {
        tracing::info!("FfmpegDemuxer::close");
        self.packet_iter = None;
        self.input = None;
        self.info = None;
        self.video_codec_context = None;
        self.audio_codec_context = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_demuxer_new() {
        let d = FfmpegDemuxer::new();
        assert!(d.info.is_none());
        assert!(d.input.is_none());
    }

    #[test]
    fn test_demuxer_default() {
        let d = FfmpegDemuxer::default();
        assert!(d.info.is_none());
    }

    #[test]
    fn test_demuxer_open_nonexistent() {
        let mut d = FfmpegDemuxer::new();
        let result = d.open("nonexistent_file_12345.mp4");
        assert!(result.is_err());
    }

    #[test]
    fn test_demuxer_read_packet_not_ready() {
        let mut d = FfmpegDemuxer::new();
        let result = d.read_packet();
        assert!(result.is_err());
    }

    #[test]
    fn test_demuxer_seek_not_ready() {
        let mut d = FfmpegDemuxer::new();
        let result = d.seek(1.0);
        assert!(result.is_err());
    }
}
