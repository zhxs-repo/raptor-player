use std::sync::atomic::Ordering;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use raptor_ffmpeg::{AudioDecoder, AudioFrame, Packet, VideoDecoder, VideoFrame};

use crate::pipeline::Pipeline;

/// Video decode loop — 从 video_pkt_rx 接收数据包，解码后送入 video_frame_tx
pub fn video_decode_loop(
    pipeline: Arc<Pipeline>,
    mut decoder: Box<dyn VideoDecoder>,
    video_pkt_rx: Receiver<Packet>,
    video_frame_tx: Sender<VideoFrame>,
) -> raptor_core::Result<()> {
    tracing::info!("video_decode_loop started");

    let mut last_seek_gen: u64 = 0;

    loop {
        if pipeline.shutdown.load(Ordering::Acquire) {
            break;
        }

        // 暂停检查
        if pipeline.is_paused() {
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }

        // 检查 seek generation，有变化则 flush 解码器
        let seek_gen = pipeline.seek_generation.load(Ordering::Acquire);
        if seek_gen != last_seek_gen {
            last_seek_gen = seek_gen;
            decoder.flush();
            tracing::debug!("video decoder flushed (seek_gen={})", seek_gen);
        }

        match video_pkt_rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(pkt) => {
                if pkt.data.is_empty() {
                    continue; // seek drain marker
                }
                if let Err(e) = decoder.submit_packet(&pkt) {
                    tracing::warn!("video decode submit_packet: {}", e);
                    continue;
                }
                loop {
                    match decoder.receive_frame() {
                        Ok(Some(frame)) => {
                            if video_frame_tx.send(frame).is_err() {
                                tracing::debug!("video_frame_tx closed");
                                return Ok(());
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!("video receive_frame: {}", e);
                            break;
                        }
                    }
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                tracing::debug!("video_pkt_rx disconnected");
                break;
            }
        }
    }

    tracing::info!("video_decode_loop exiting");
    Ok(())
}

/// Audio decode loop — 从 audio_pkt_rx 接收数据包，解码后送入 audio_frame_tx
pub fn audio_decode_loop(
    pipeline: Arc<Pipeline>,
    mut decoder: Box<dyn AudioDecoder>,
    audio_pkt_rx: Receiver<Packet>,
    audio_frame_tx: Sender<AudioFrame>,
) -> raptor_core::Result<()> {
    tracing::info!("audio_decode_loop started");

    let mut last_seek_gen: u64 = 0;

    loop {
        if pipeline.shutdown.load(Ordering::Acquire) {
            break;
        }

        // 暂停检查
        if pipeline.is_paused() {
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }

        let seek_gen = pipeline.seek_generation.load(Ordering::Acquire);
        if seek_gen != last_seek_gen {
            last_seek_gen = seek_gen;
            decoder.flush();
            tracing::debug!("audio decoder flushed (seek_gen={})", seek_gen);
        }

        match audio_pkt_rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(pkt) => {
                if pkt.data.is_empty() {
                    continue;
                }
                if let Err(e) = decoder.submit_packet(&pkt) {
                    tracing::warn!("audio decode submit_packet: {}", e);
                    continue;
                }
                loop {
                    match decoder.receive_frame() {
                        Ok(Some(frame)) => {
                            if audio_frame_tx.send(frame).is_err() {
                                tracing::debug!("audio_frame_tx closed");
                                return Ok(());
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!("audio receive_frame: {}", e);
                            break;
                        }
                    }
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                tracing::debug!("audio_pkt_rx disconnected");
                break;
            }
        }
    }

    tracing::info!("audio_decode_loop exiting");
    Ok(())
}
