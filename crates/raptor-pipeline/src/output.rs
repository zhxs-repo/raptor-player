use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use raptor_audio::AudioOutput;
use raptor_core::{EndReason, RaptorEvent};
use raptor_render::{SurfaceHandle, VideoOutput};

use crate::avsync::VideoSyncDecision;
use crate::pipeline::Pipeline;
use crate::seek::{recv_current, Stamped};

/// Surface 生命周期操作的确认通道：渲染线程在完成操作后才回传结果。
///
/// Android 的 `surfaceDestroyed` 在 FFI 返回后立即释放 `ANativeWindow`，
/// 因此 detach 不能只是"命令已入队"，调用方必须等到 Surface 真正被 drop。
pub type SurfaceAck = crossbeam_channel::Sender<raptor_core::Result<()>>;

/// 渲染器命令 — FFI 层通过 channel 发送到 render_loop
///
/// 用于运行时 Surface 管理（Android 生命周期：detach/reattach/swap renderer）。
pub enum RendererCmd {
    /// 替换为外部 Surface 渲染器（Android 首次 set_surface 或运行时切换）
    SetSurface(
        Box<dyn VideoOutput>,
        SurfaceHandle,
        Vec<Box<dyn raptor_render::Overlay>>,
    ),
    /// 分离当前 Surface（Android onPause），ack 表示原生窗口引用已释放
    DetachSurface(SurfaceAck),
    /// 调整 Surface 尺寸（屏幕旋转等）
    ResizeSurface { width: u32, height: u32 },
}

/// Fallback idle limit when no video frames have been displayed (audio-only / zero-frame)
const END_IDLE_LIMIT: u32 = 20; // ~1s at 50ms/cycle

/// Video render loop — 从 video_frame_rx 接收帧，经 AV 同步后提交到 VideoOutput
#[allow(clippy::too_many_arguments)]
pub fn render_loop(
    pipeline: Arc<Pipeline>,
    renderer: Arc<Mutex<Box<dyn VideoOutput>>>,
    video_frame_rx: crossbeam_channel::Receiver<Stamped<raptor_ffmpeg::VideoFrame>>,
    event_tx: crossbeam_channel::Sender<RaptorEvent>,
    duration_secs: f64,
    has_video: bool,
    video_info: Option<raptor_core::VideoInfo>,
    audio_info: Option<raptor_core::AudioInfo>,
    renderer_cmd_rx: Option<crossbeam_channel::Receiver<RendererCmd>>,
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
    let mut last_hud_update = std::time::Instant::now();
    let mut hud_last_rendered: u64 = 0; // 上次 HUD更新时的已渲染帧数

    let video_codec = video_info
        .as_ref()
        .map(|v| v.codec.clone())
        .unwrap_or_default();
    let video_w = video_info.as_ref().map(|v| v.width).unwrap_or(0);
    let video_h = video_info.as_ref().map(|v| v.height).unwrap_or(0);
    let video_fps = video_info.as_ref().map(|v| v.fps).unwrap_or(0.0);
    let audio_codec = audio_info
        .as_ref()
        .map(|a| a.codec.clone())
        .unwrap_or_default();
    let audio_desc = if audio_codec.is_empty() {
        String::new()
    } else {
        let ch = audio_info.as_ref().map(|a| a.channels).unwrap_or(0);
        let sr = audio_info.as_ref().map(|a| a.sample_rate).unwrap_or(0);
        format!(" | {} {}ch {}Hz", audio_codec, ch, sr)
    };

    'outer: loop {
        if pipeline.shutdown.load(Ordering::Acquire) {
            tracing::info!("render_loop: shutdown");
            break;
        }

        // 处理渲染器命令（Surface 管理 — Android 生命周期）
        // 在 poll 之前处理，确保 renderer 处于正确状态
        if let Some(ref cmd_rx) = renderer_cmd_rx {
            while let Ok(cmd) = cmd_rx.try_recv() {
                let mut r = renderer.lock();
                match cmd {
                    RendererCmd::DetachSurface(ack) => {
                        tracing::info!("render_loop: DetachSurface");
                        let result = r.detach_surface();
                        if let Err(e) = &result {
                            tracing::error!("render_loop: detach_surface failed: {e}");
                        }
                        let _ = ack.send(result);
                    }
                    RendererCmd::SetSurface(mut new_renderer, handle, overlays) => {
                        tracing::info!("render_loop: SetSurface — swapping renderer");
                        // 初始化新 renderer
                        if let Some(ref vi) = video_info {
                            let _ = new_renderer.init(vi.width, vi.height);
                        }
                        // 设置 overlays（字幕、弹幕）
                        if !overlays.is_empty() {
                            new_renderer.set_overlays(overlays);
                        }
                        // 使用新 surface 初始化
                        if let Err(e) = new_renderer.reattach_surface(handle) {
                            tracing::error!("render_loop: SetSurface reattach failed: {e}");
                        }
                        // 原子替换 renderer（旧 renderer 被 drop 时会自行清理）
                        *r = new_renderer;
                    }
                    RendererCmd::ResizeSurface { width, height } => {
                        tracing::info!("render_loop: ResizeSurface({}x{})", width, height);
                        let _ = r.set_size(width, height);
                    }
                }
            }
        }

        // 轮询窗口事件（即使在暂停状态也要保持窗口响应）
        {
            let mut r = renderer.lock();
            r.poll();
            if r.should_stop() {
                tracing::info!("render_loop: window closed");
                let _ = event_tx.send(RaptorEvent::EndFile {
                    reason: EndReason::Stop,
                });
                break;
            }
        }

        // 暂停检查（在窗口 poll 之后，保证窗口不会卡死）
        // 同时冻结叠加层挂钟：渲染线程会在两个视频帧之间按挂钟外推叠加层时间，
        // 不冻结就会出现"画面静止、弹幕继续滑动"
        if pipeline.is_paused() {
            renderer.lock().freeze_overlay_clock();
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }

        // 只接受当前 seek generation 的帧；seek 之前入队的残留帧直接丢弃，
        // 否则会把旧位置的画面/声音播出来，并把 position 往回拽
        match recv_current(
            &video_frame_rx,
            pipeline.seek_generation.load(Ordering::Acquire),
            Duration::from_millis(50),
        ) {
            Ok(stamped) => {
                let frame = stamped.item;
                idle_count = 0;

                // 无时间戳（NOPTS）帧：直接上屏，但不参与 AV 同步、不推进 position，
                // 否则会被当成 ts=0 的落后帧全部丢弃
                let Some(pts) = frame.pts_secs() else {
                    tracing::debug!("render_loop: frame without pts, displaying untimed");
                    let _ = renderer.lock().submit_frame(&frame);
                    continue;
                };

                // 首帧：设置 AVSync 基准
                if first_frame {
                    first_frame = false;
                    pipeline.avsync.set_first_frame_time(pts);
                    render_start = Some(std::time::Instant::now());
                }

                match pipeline.avsync.video_sync_decision(pts) {
                    VideoSyncDecision::Display => {
                        rendered_frames += 1;
                        if rendered_frames.is_multiple_of(30) {
                            tracing::info!("render_loop: rendered {} frames", rendered_frames);
                        }
                        pipeline
                            .position_us
                            .store((pts * 1_000_000.0) as u64, Ordering::Release);
                        let _ = renderer.lock().submit_frame(&frame);
                        pipeline.avsync.update_video_clock(pts);
                    }
                    VideoSyncDecision::Wait(mut secs) => {
                        // sleep 后重新决策：若主时钟已超前很多，该帧可能已过时
                        let mut attempts = 0;
                        loop {
                            std::thread::sleep(Duration::from_secs_f64(secs));
                            match pipeline.avsync.video_sync_decision(pts) {
                                VideoSyncDecision::Display => break,
                                VideoSyncDecision::Wait(s) => {
                                    secs = s;
                                    attempts += 1;
                                    if attempts >= 3 {
                                        // 避免无限等待，强制显示
                                        break;
                                    }
                                }
                                VideoSyncDecision::Drop => {
                                    dropped_frames += 1;
                                    pipeline
                                        .position_us
                                        .store((pts * 1_000_000.0) as u64, Ordering::Release);
                                    // 跳过渲染，直接进入下一帧
                                    continue 'outer;
                                }
                            }
                        }
                        rendered_frames += 1;
                        pipeline
                            .position_us
                            .store((pts * 1_000_000.0) as u64, Ordering::Release);
                        let _ = renderer.lock().submit_frame(&frame);
                        pipeline.avsync.update_video_clock(pts);
                    }
                    VideoSyncDecision::Drop => {
                        dropped_frames += 1;
                        if dropped_frames.is_multiple_of(30) {
                            tracing::warn!(
                                "render_loop: {} frames dropped (rendered={}, pts={:.3})",
                                dropped_frames,
                                rendered_frames,
                                pts
                            );
                        }
                        // 即使丢帧也更新 position，让前端进度条持续推进
                        pipeline
                            .position_us
                            .store((pts * 1_000_000.0) as u64, Ordering::Release);
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

                // 定期更新 HUD 标题栏（每 500ms）
                if last_hud_update.elapsed() >= std::time::Duration::from_millis(500) {
                    let now = std::time::Instant::now();
                    let elapsed = now.duration_since(last_hud_update).as_secs_f64();
                    // 使用窗口线程的实际渲染帧数（包含弹幕等 overlay 渲染）
                    let gpu_frame_count = renderer.lock().render_frame_count();
                    let frames_in_window = gpu_frame_count - hud_last_rendered;
                    let realtime_fps = frames_in_window as f64 / elapsed;
                    hud_last_rendered = gpu_frame_count;
                    last_hud_update = now;

                    let pos = pipeline.current_position_secs();
                    let status = if pipeline.is_paused() {
                        "PAUSE"
                    } else {
                        "PLAY"
                    };
                    let title = format!(
                        "Raptor | {} | {} {}x{} {:.1}fps | {:.1}rfps | SW | {}/{:.0}s | F:{} D:{}{}",
                        status,
                        if video_codec.is_empty() { "-".to_string() } else { video_codec.clone() },
                        video_w, video_h, video_fps,
                        realtime_fps,
                        pos, duration_secs,
                        rendered_frames, dropped_frames,
                        audio_desc,
                    );
                    renderer.lock().set_title(&title);
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

    // EOF / 窗口关闭 / shutdown 等所有退出路径都要冻结叠加层挂钟，
    // 否则渲染线程会继续按挂钟把弹幕推到超出最后一帧的位置
    renderer.lock().freeze_overlay_clock();
    tracing::info!("render_loop exiting");
    Ok(())
}

/// Audio output loop — 从 audio_frame_rx 接收帧，写入 AudioOutput
pub fn audio_output_loop(
    pipeline: Arc<Pipeline>,
    mut audio_output: Box<dyn AudioOutput>,
    audio_frame_rx: crossbeam_channel::Receiver<Stamped<raptor_ffmpeg::AudioFrame>>,
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

        let current_gen = pipeline.seek_generation.load(Ordering::Acquire);
        match recv_current(&audio_frame_rx, current_gen, Duration::from_millis(50)) {
            Ok(stamped) => {
                // 读取当前音量并应用
                let vol = pipeline.get_volume();
                audio_output.set_volume(vol as f32 / 100.0);

                if let Err(e) = audio_output.write(&stamped.item) {
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
