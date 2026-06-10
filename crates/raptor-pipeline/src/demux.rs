use std::sync::atomic::Ordering;
use std::sync::Arc;

use crossbeam_channel::Sender;
use raptor_ffmpeg::{Demuxer, Packet};

use crate::pipeline::Pipeline;

/// Demux loop — 从 Demuxer 读取 Packet，分发到视频/音频解码通道
#[allow(clippy::collapsible_if)]
pub fn demux_loop(
    pipeline: Arc<Pipeline>,
    mut demuxer: Box<dyn Demuxer>,
    video_pkt_tx: Sender<Packet>,
    audio_pkt_tx: Sender<Packet>,
) -> raptor_core::Result<()> {
    tracing::info!("demux_loop started");

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
            // 递增 seek_generation，通知解码线程 flush
            pipeline.seek_generation.fetch_add(1, Ordering::Release);
            // 重置 AVSync
            pipeline.avsync.reset(req.target);
            // 清空 channel 中的旧数据
            while video_pkt_tx
                .try_send(Packet {
                    data: vec![],
                    stream_index: 0,
                    pts: 0.0,
                    dts: 0.0,
                    is_key: false,
                })
                .is_ok()
            {}
            while audio_pkt_tx
                .try_send(Packet {
                    data: vec![],
                    stream_index: 0,
                    pts: 0.0,
                    dts: 0.0,
                    is_key: false,
                })
                .is_ok()
            {}
        }

        match demuxer.read_packet() {
            Ok(Some(pkt)) => {
                let info = demuxer.info().unwrap();
                let stream_idx = pkt.stream_index;
                if Some(stream_idx) == info.video_stream_index {
                    if video_pkt_tx.send(pkt).is_err() {
                        tracing::debug!("demux: video_pkt_tx closed");
                        break;
                    }
                } else if Some(stream_idx) == info.audio_stream_index {
                    if audio_pkt_tx.send(pkt).is_err() {
                        tracing::debug!("demux: audio_pkt_tx closed");
                        break;
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
