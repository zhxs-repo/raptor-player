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

/// 看门狗预算 · 音频输出：设备连续这么久不接手任何采样即视为停摆
///
/// 10 s 而不是更短：USB/蓝牙设备重连、系统休眠唤醒都会短暂停止消费，
/// 这类恢复不该被判成错误。
const OUTPUT_STALL_TIMEOUT: Duration = Duration::from_secs(10);

/// 帧已尽（取帧超时或 rx 已关闭）时的播放结束判定，返回 `true` 表示循环应退出
///
/// demux 尚未完成时返回 `false`：帧只是暂时没到，继续等下一个周期。
/// 主时钟还没走到片长说明设备里仍有内容要放（`eof_due` 以它为准），
/// 此时靠 `END_IDLE_LIMIT` 兜底，避免设备停摆把线程永远卡在结束判定上。
///
/// `idle_fallback` 只对视频轨成立：纯音频的包通道能装下几十秒，demux 往往在
/// 播放开始前就已完成，此刻"没有新帧"完全不代表播放结束。音频只认 `eof_due`，
/// 时钟失灵时由其中的挂钟判据接管。
fn no_more_frames(
    eof_due: bool,
    idle_count: &mut u32,
    pipeline: &Pipeline,
    event_tx: &crossbeam_channel::Sender<RaptorEvent>,
    idle_fallback: bool,
) -> bool {
    if !pipeline.demux_complete.load(Ordering::Acquire) {
        return false;
    }
    if !eof_due {
        if !idle_fallback {
            return false;
        }
        *idle_count += 1;
        if *idle_count < END_IDLE_LIMIT {
            return false;
        }
        tracing::info!("render_loop: idle limit reached, EOF");
    } else {
        tracing::info!("render_loop: EOF (media clock reached duration)");
    }
    let _ = event_tx.send(RaptorEvent::EndFile {
        reason: EndReason::Eof,
    });
    true
}

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

    // 累计的"正在播放"时长（不含暂停），作为 EOF 的挂钟兜底
    let mut playing = Duration::ZERO;
    let mut play_tick = std::time::Instant::now();
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

        // EOF 截止只累计"正在播放"的时间：暂停期间画面本就静止，挂钟若继续走，
        // 暂停久了回来会被直接判定为播放结束
        //
        // 纯音频没有首帧可锚：线程一开始跑就算播放已开始，否则时钟一旦失灵就
        // 再也等不到 EOF
        let started = !first_frame || !has_video;
        if started {
            playing += play_tick.elapsed();
        }
        play_tick = std::time::Instant::now();

        // EOF 以媒体主时钟为准：position 是最后上屏的视频帧 PTS，它领先设备里
        // 尚未放完的音频（最多 1 秒），按它判结束会把结尾的声音连同线程一起切掉。
        // 挂钟仅在无音频时兜底，并留 2 秒余量防止设备停摆永远等不到
        let master_clock = pipeline.avsync.master_clock();
        let eof_due = started
            && ((duration_secs > 0.0 && master_clock + 0.05 >= duration_secs)
                || playing >= duration + Duration::from_secs(2));

        // 纯音频文件没有视频帧来写 position：主时钟就是当前位置。不补这一步，
        // 前端进度条会一直停在 0，暂停后 resume 也会把时钟重置回起点
        if !has_video {
            pipeline
                .position_us
                .store((master_clock * 1_000_000.0) as u64, Ordering::Release);
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
                    play_tick = std::time::Instant::now();
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
                if no_more_frames(eof_due, &mut idle_count, &pipeline, &event_tx, has_video) {
                    break;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                tracing::debug!("video_frame_rx disconnected");
                // 解码线程结束不等于播放结束：设备里还排着最后一秒音频。转入与
                // "超时无帧"相同的判定，等主时钟走到片长再报 EOF
                std::thread::sleep(Duration::from_millis(50));
                if no_more_frames(eof_due, &mut idle_count, &pipeline, &event_tx, has_video) {
                    break;
                }
                if !has_video {
                    // 无视频轨时 tx 从建线起就没人持有，这里的"断开"不代表任何事，
                    // 只能继续等设备把音频放完
                    continue;
                }
                // rx 关闭且 demux 未完成 = 管线被拆（stop / 重载），正常退出
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

    let mut device_paused = false;
    let mut last_seek_gen = pipeline.seek_generation.load(Ordering::Acquire);
    // 主时钟读数要带着它属于哪一次 seek，视频线程才能拒绝 flush 前入队的旧位置采样
    let clock = audio_output.clock();
    if let Some(clock) = &clock {
        clock.set_generation(last_seek_gen);
    }
    // 设备一时装不下的那一帧留在这里，下一轮重试
    let mut pending: Option<raptor_ffmpeg::AudioFrame> = None;
    // 音频流读完（发送端关闭）后等设备把缓冲排空，见循环末尾
    let mut stream_eof = false;
    let mut drain_deadline: Option<std::time::Instant> = None;
    // 设备停摆一次只报一次，写入成功即重新武装
    let mut stall_reported = false;

    loop {
        if pipeline.shutdown.load(Ordering::Acquire) {
            break;
        }

        // 暂停检查 — 同时冻结设备消费
        //
        // ring buffer 里最多积压 1 秒采样，不暂停 stream 的话这些内容会在
        // "已暂停"期间继续放完，音频主时钟也会跟着走完这段并不存在的时间
        if pipeline.is_paused() {
            if !device_paused {
                if let Err(e) = audio_output.pause() {
                    tracing::warn!("audio_output pause error: {}", e);
                }
                device_paused = true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }
        if device_paused {
            if let Err(e) = audio_output.resume() {
                tracing::warn!("audio_output resume error: {}", e);
            }
            device_paused = false;
        }

        // seek：缓冲里剩下的是旧位置的音频，既会先响一小段，
        // 也会把音频主时钟锚在刚跳走的时间上
        let current_gen = pipeline.seek_generation.load(Ordering::Acquire);
        if current_gen != last_seek_gen {
            last_seek_gen = current_gen;
            audio_output.flush();
            if let Some(clock) = &clock {
                clock.set_generation(current_gen);
            }
            pending = None;
        }

        // 背压：设备没空间就攥着这一帧等它，既不丢采样（丢采样会让音频一路冲到
        // 文件末尾、主时钟越过视频好几秒），也不在 write 里长阻塞
        // （暂停 / seek / shutdown 必须能被及时看到）
        if pending.is_none() && !stream_eof {
            match recv_current(&audio_frame_rx, current_gen, Duration::from_millis(50)) {
                Ok(stamped) => pending = Some(stamped.item),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    tracing::debug!("audio_frame_rx disconnected, 等待设备排空缓冲");
                    stream_eof = true;
                    drain_deadline = Some(std::time::Instant::now() + Duration::from_millis(2000));
                }
            }
        }

        if let Some(frame) = &pending {
            if !audio_output.accepts(frame) {
                // 看门狗：设备不收采样时主时钟也不会走，画面就此静止。
                // 时钟读数的 age 正是"多久没有一次消费"，超过预算就报一次
                if !stall_reported {
                    if let Some(age) = clock.as_ref().and_then(|c| c.observe()).map(|o| o.age) {
                        if age >= OUTPUT_STALL_TIMEOUT {
                            stall_reported = true;
                            pipeline.report_stall("audio output device", age);
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
            // 读取当前音量并应用（静音时增益为 0，主时钟照常推进）
            audio_output.set_volume(pipeline.effective_volume());
            if let Err(e) = audio_output.write(frame) {
                tracing::warn!("audio_output write error: {}", e);
            }
            pending = None;
            stall_reported = false;
        }

        // 音频流读到末尾不等于播放结束：设备里还排着最多 1 秒采样，立刻退出会
        // 连同输出设备一起丢掉它们（结尾被截断，主时钟也停在片长之前，EOF 判据
        // 永远等不到）。排空后再走，超时兜底防止设备停摆把线程卡在这里
        if stream_eof {
            let drained = !audio_output.has_pending_audio();
            let timed_out = drain_deadline
                .map(|deadline| std::time::Instant::now() >= deadline)
                .unwrap_or(false);
            if drained || timed_out {
                tracing::debug!(
                    "audio_output_loop: stream EOF，缓冲{}（等待排空）",
                    if drained { "已排空" } else { "排空超时" }
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    tracing::info!("audio_output_loop exiting");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;

    fn pipeline() -> (Pipeline, crossbeam_channel::Receiver<RaptorEvent>) {
        let (tx, rx) = bounded::<RaptorEvent>(16);
        (Pipeline::new(tx), rx)
    }

    /// 纯音频的包通道能装下几十秒，demux 先读完不代表播放结束：只认主时钟
    #[test]
    fn audio_only_waits_for_the_media_clock() {
        let (pipeline, rx) = pipeline();
        pipeline.demux_complete.store(true, Ordering::Release);
        let mut idle = 0u32;
        for _ in 0..END_IDLE_LIMIT {
            assert!(
                !no_more_frames(false, &mut idle, &pipeline, &pipeline.event_tx, false),
                "主时钟还没走到片长就报了 EOF"
            );
        }
        assert!(rx.try_recv().is_err());

        assert!(no_more_frames(
            true,
            &mut idle,
            &pipeline,
            &pipeline.event_tx,
            false
        ));
        match rx.try_recv() {
            Ok(RaptorEvent::EndFile {
                reason: EndReason::Eof,
            }) => {}
            other => panic!("期望 EndFile(Eof)，got {other:?}"),
        }
    }

    /// 视频轨保留 idle 计数兜底：设备停摆时时钟永不推进，不能让线程卡在结束判定上
    #[test]
    fn video_track_keeps_idle_fallback() {
        let (pipeline, rx) = pipeline();
        pipeline.demux_complete.store(true, Ordering::Release);
        let mut idle = 0u32;
        for _ in 0..END_IDLE_LIMIT - 1 {
            assert!(!no_more_frames(
                false,
                &mut idle,
                &pipeline,
                &pipeline.event_tx,
                true
            ));
        }
        assert!(no_more_frames(
            false,
            &mut idle,
            &pipeline,
            &pipeline.event_tx,
            true
        ));
        assert!(rx.try_recv().is_ok());
    }

    /// demux 未完成时帧只是暂时没到
    #[test]
    fn no_eof_before_demux_completes() {
        let (pipeline, rx) = pipeline();
        let mut idle = 0u32;
        assert!(!no_more_frames(
            true,
            &mut idle,
            &pipeline,
            &pipeline.event_tx,
            true
        ));
        assert!(rx.try_recv().is_err());
    }
}
