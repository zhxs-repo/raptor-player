use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use raptor_audio::AudioOutput;
use raptor_core::{EndReason, RaptorEvent};
use raptor_render::VideoOutput;

use crate::avsync::VideoSyncDecision;
use crate::pipeline::Pipeline;

/// Fallback idle limit when no video frames have been displayed (audio-only / zero-frame)
const END_IDLE_LIMIT: u32 = 20; // ~1s at 50ms/cycle

/// Video render loop — 从 video_frame_rx 接收帧，经 AV 同步后提交到 VideoOutput
///
/// 使用**挂钟时间**判断播放结束：记录首帧显示时刻，当经过时间 >= 文件时长时发送 EndFile。
pub fn render_loop(
    pipeline: Arc<Pipeline>,
    mut renderer: Box<dyn VideoOutput>,
    video_frame_rx: crossbeam_channel::Receiver<raptor_ffmpeg::VideoFrame>,
    event_tx: crossbeam_channel::Sender<RaptorEvent>,
    duration_secs: f64,
    has_video: bool,
) -> raptor_core::Result<()> {
    tracing::info!(
        "render_loop started, duration={:.2}s, has_video={}",
        duration_secs,
        has_video
    );

    let mut render_start: Option<std::time::Instant> = None;
    let duration = Duration::from_secs_f64(duration_secs);
    let mut idle_count: u32 = 0;
    let mut first_frame = true;
    let mut rendered_frames: u64 = 0;
    let mut dropped_frames: u64 = 0;

    loop {
        if pipeline.shutdown.load(Ordering::Acquire) {
            tracing::info!("render_loop: shutdown");
            break;
        }

        // 轮询窗口事件（即使在暂停状态也要保持窗口响应）
        renderer.poll();
        if renderer.should_stop() {
            tracing::info!("render_loop: window closed");
            let _ = event_tx.send(RaptorEvent::EndFile {
                reason: EndReason::Stop,
            });
            break;
        }

        // 暂停检查（在窗口 poll 之后，保证窗口不会卡死）
        if pipeline.is_paused() {
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }

        match video_frame_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(frame) => {
                idle_count = 0;

                // 首帧：设置 AVSync 基准
                if first_frame {
                    first_frame = false;
                    pipeline.avsync.set_first_frame_time(frame.pts);
                    render_start = Some(std::time::Instant::now());
                }

                match pipeline.avsync.video_sync_decision(frame.pts) {
                    VideoSyncDecision::Display => {
                        rendered_frames += 1;
                        if rendered_frames.is_multiple_of(30) {
                            tracing::info!("render_loop: rendered {} frames", rendered_frames);
                        }
                        pipeline
                            .position_us
                            .store((frame.pts * 1_000_000.0) as u64, Ordering::Release);
                        let _ = renderer.submit_frame(&frame);
                        pipeline.avsync.update_video_clock(frame.pts);
                    }
                    VideoSyncDecision::Wait(secs) => {
                        std::thread::sleep(Duration::from_secs_f64(secs));
                        pipeline
                            .position_us
                            .store((frame.pts * 1_000_000.0) as u64, Ordering::Release);
                        let _ = renderer.submit_frame(&frame);
                        pipeline.avsync.update_video_clock(frame.pts);
                    }
                    VideoSyncDecision::Drop => {
                        dropped_frames += 1;
                        if dropped_frames.is_multiple_of(30) {
                            tracing::warn!(
                                "render_loop: {} frames dropped (rendered={}, pts={:.3})",
                                dropped_frames,
                                rendered_frames,
                                frame.pts
                            );
                        }
                        // 即使丢帧也更新 position，让前端进度条持续推进
                        pipeline
                            .position_us
                            .store((frame.pts * 1_000_000.0) as u64, Ordering::Release);
                    }
                }

                // 挂钟截止检查
                if let Some(start) = render_start {
                    if start.elapsed() >= duration {
                        tracing::info!("render_loop: wall-clock duration reached, EOF");
                        let _ = event_tx.send(RaptorEvent::EndFile {
                            reason: EndReason::Eof,
                        });
                        break;
                    }
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !has_video {
                    // 纯音频文件：没有视频帧，仅保持窗口响应，等待 shutdown
                    continue;
                }
                if pipeline.demux_complete.load(Ordering::Acquire) {
                    // demux 已完成且没有更多帧
                    if let Some(start) = render_start {
                        if start.elapsed() >= duration {
                            tracing::info!("render_loop: wall-clock EOF (timeout)");
                            let _ = event_tx.send(RaptorEvent::EndFile {
                                reason: EndReason::Eof,
                            });
                            break;
                        }
                    }
                    // 兜底：长时间无帧
                    idle_count += 1;
                    if idle_count >= END_IDLE_LIMIT {
                        tracing::info!("render_loop: idle limit reached, EOF");
                        let _ = event_tx.send(RaptorEvent::EndFile {
                            reason: EndReason::Eof,
                        });
                        break;
                    }
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                tracing::debug!("video_frame_rx disconnected");
                break;
            }
        }
    }

    tracing::info!("render_loop exiting");
    Ok(())
}

/// Audio output loop — 从 audio_frame_rx 接收帧，写入 AudioOutput
pub fn audio_output_loop(
    pipeline: Arc<Pipeline>,
    mut audio_output: Box<dyn AudioOutput>,
    audio_frame_rx: crossbeam_channel::Receiver<raptor_ffmpeg::AudioFrame>,
) -> raptor_core::Result<()> {
    tracing::info!("audio_output_loop started");

    loop {
        if pipeline.shutdown.load(Ordering::Acquire) {
            break;
        }

        // 暂停检查
        if pipeline.is_paused() {
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }

        match audio_frame_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(frame) => {
                // 读取当前音量并应用
                let vol = pipeline.get_volume();
                audio_output.set_volume(vol as f32 / 100.0);

                if let Err(e) = audio_output.write(&frame) {
                    tracing::warn!("audio_output write error: {}", e);
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                tracing::debug!("audio_frame_rx disconnected");
                break;
            }
        }
    }

    tracing::info!("audio_output_loop exiting");
    Ok(())
}
