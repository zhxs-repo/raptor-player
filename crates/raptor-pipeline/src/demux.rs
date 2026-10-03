use std::sync::atomic::Ordering;
use std::sync::Arc;

use crossbeam_channel::Sender;
use raptor_ffmpeg::{Demuxer, Packet};

use crate::pipeline::Pipeline;
use crate::seek::Stamped;

/// Demux loop — 从 Demuxer 读取 Packet，分发到视频/音频/字幕解码通道
#[allow(clippy::collapsible_if)]
pub fn demux_loop(
    pipeline: Arc<Pipeline>,
    mut demuxer: Box<dyn Demuxer>,
    video_pkt_tx: Sender<Stamped<Packet>>,
    audio_pkt_tx: Sender<Stamped<Packet>>,
    subtitle_pkt_tx: Option<Sender<Stamped<Packet>>>,
) -> raptor_core::Result<()> {
    tracing::info!("demux_loop started");

    let mut pkt_count: u64 = 0;
    loop {
        if pipeline.shutdown.load(Ordering::Acquire) {
            tracing::info!("demux_loop: shutdown requested");
            break;
        }

        // 检查 seek 请求
        if let Some(req) = {
            let mut lock = pipeline.seek_request.lock();
            lock.take()
        } {
            tracing::info!("demux: seeking to {:.3}s", req.target);
            if let Err(e) = demuxer.seek(req.target) {
                tracing::error!("demux: seek failed: {}", e);
            }
            // 递增 seek_generation：下游按 generation 丢弃 seek 之前入队的残留数据，
            // 解码线程同时据此触发 decoder.flush()
            pipeline.seek_generation.fetch_add(1, Ordering::Release);
            // 重置 AVSync
            pipeline.avsync.reset(req.target);
            // position 立即对齐到目标，避免残留旧帧把进度条往回拽
            pipeline
                .position_us
                .store((req.target * 1_000_000.0) as u64, Ordering::Release);
        }

        // 先记录 generation 再读包：只有 seek 之后读到的包才属于新的 generation，
        // seek 之前读到、之后才入队的包仍带旧标记，会被下游丢弃
        let generation = pipeline.seek_generation.load(Ordering::Acquire);

        match demuxer.read_packet() {
            Ok(Some(pkt)) => {
                pkt_count += 1;
                if pkt_count.is_multiple_of(100) {
                    tracing::info!("demux: read {} packets", pkt_count);
                }
                let info = demuxer.info().unwrap();
                let stream_idx = pkt.stream_index;
                let stamped = Stamped::new(generation, pkt);
                if Some(stream_idx) == info.video_stream_index {
                    if video_pkt_tx.send(stamped).is_err() {
                        tracing::debug!("demux: video_pkt_tx closed");
                        break;
                    }
                } else if Some(stream_idx) == info.audio_stream_index {
                    if audio_pkt_tx.send(stamped).is_err() {
                        tracing::debug!("demux: audio_pkt_tx closed");
                        break;
                    }
                } else if Some(stream_idx) == info.subtitle_stream_index {
                    if let Some(ref tx) = subtitle_pkt_tx {
                        if tx.send(stamped).is_err() {
                            tracing::debug!("demux: subtitle_pkt_tx closed");
                            break;
                        }
                    }
                }
            }
            Ok(None) => {
                // EOF — 设置 demux_complete flag，由 render_loop 判定结束
                tracing::info!("demux: EOF");
                pipeline.demux_complete.store(true, Ordering::Release);
                break;
            }
            Err(e) => {
                tracing::error!("demux: read_packet error: {}", e);
                break;
            }
        }
    }

    tracing::info!("demux_loop exiting");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{bounded, Receiver as CReceiver, Sender as CSender};
    use raptor_core::RaptorEvent;
    use raptor_ffmpeg::{MediaInfo, PixelFormat, SampleFormat};

    /// 按脚本供货的 demuxer：每次 `read_packet` 都先报告"已进入"再等待放行，
    /// 使"seek 请求何时置位""generation 何时读取"在测试里完全确定
    struct ScriptedDemuxer {
        info: MediaInfo,
        entered: CSender<usize>,
        release: CReceiver<()>,
        calls: usize,
    }

    impl Demuxer for ScriptedDemuxer {
        fn open(&mut self, _url: &str) -> raptor_core::Result<()> {
            Ok(())
        }

        fn read_packet(&mut self) -> raptor_core::Result<Option<Packet>> {
            self.calls += 1;
            let call = self.calls;
            if call > 2 {
                return Ok(None); // EOF
            }
            let _ = self.entered.send(call);
            let _ = self.release.recv();
            Ok(Some(pkt(call)))
        }

        fn seek(&mut self, _target: f64) -> raptor_core::Result<()> {
            Ok(())
        }

        fn info(&self) -> Option<&MediaInfo> {
            Some(&self.info)
        }

        fn take_video_codec_context(&mut self) -> Option<ffmpeg_next::codec::Context> {
            None
        }

        fn take_audio_codec_context(&mut self) -> Option<ffmpeg_next::codec::Context> {
            None
        }

        fn take_subtitle_codec_context(&mut self) -> Option<ffmpeg_next::codec::Context> {
            None
        }

        fn close(&mut self) {}
    }

    fn pkt(call: usize) -> Packet {
        Packet {
            data: vec![call as u8],
            stream_index: 0,
            pts: Some(call as i64 * 1000),
            dts: Some(call as i64 * 1000),
            time_base: raptor_ffmpeg::time_base(1, 1000),
            is_key: true,
        }
    }

    fn video_info() -> MediaInfo {
        MediaInfo {
            duration: 100.0,
            video_stream_index: Some(0),
            audio_stream_index: None,
            subtitle_stream_index: None,
            video_codec_id: None,
            audio_codec_id: None,
            subtitle_codec_id: None,
            width: 320,
            height: 180,
            pixel_format: PixelFormat::Yuv420p,
            fps: 25.0,
            sample_rate: 0,
            channels: 0,
            sample_format: SampleFormat::Unknown,
        }
    }

    /// 回归：packet 的 generation 取自"读取之前"观察到的值，seek 之后读到的包才属于新 generation
    #[test]
    fn stamps_packets_with_generation_observed_before_read() {
        let (event_tx, _event_rx) = bounded::<RaptorEvent>(16);
        let pipeline = Arc::new(Pipeline::new(event_tx));
        let (entered_tx, entered_rx) = bounded::<usize>(4);
        let (release_tx, release_rx) = bounded::<()>(4);
        let demuxer: Box<dyn Demuxer> = Box::new(ScriptedDemuxer {
            info: video_info(),
            entered: entered_tx,
            release: release_rx,
            calls: 0,
        });
        let (video_pkt_tx, video_pkt_rx) = bounded::<Stamped<Packet>>(8);
        let (audio_pkt_tx, _audio_pkt_rx) = bounded::<Stamped<Packet>>(8);

        let p = pipeline.clone();
        let handle = std::thread::spawn(move || {
            demux_loop(p, demuxer, video_pkt_tx, audio_pkt_tx, None).unwrap()
        });

        // 第 1 次 read 发生在 seek 之前 → 必须标记为旧的 generation 0
        assert_eq!(entered_rx.recv().unwrap(), 1);
        // 循环正阻塞在 read_packet 内部，此时置位的 seek 只会在下一轮顶部被处理
        *pipeline.seek_request.lock() = Some(crate::pipeline::SeekRequest {
            target: 42.0,
            from: 1.0,
        });
        release_tx.send(()).unwrap();
        let first = video_pkt_rx.recv().unwrap();
        assert_eq!(first.generation, 0);
        assert_eq!(first.item.data, vec![1]);

        // 第 2 次 read 时循环顶部已处理完 seek → 属于新的 generation 1
        assert_eq!(entered_rx.recv().unwrap(), 2);
        assert_eq!(pipeline.seek_generation.load(Ordering::Acquire), 1);
        release_tx.send(()).unwrap();
        let second = video_pkt_rx.recv().unwrap();
        assert_eq!(second.generation, 1);
        assert_eq!(second.item.data, vec![2]);

        handle.join().unwrap();
        // seek 立即把 position 对齐到目标，残留旧帧无法再把进度往回拽
        assert_eq!(pipeline.position_us.load(Ordering::Acquire), 42_000_000);
        assert!(pipeline.demux_complete.load(Ordering::Acquire));
    }
}
