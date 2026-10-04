//! 字幕解码线程 — 从 subtitle packet 提取文本并边解码边注入 SubtitleEngine

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crossbeam_channel::Receiver;
use parking_lot::Mutex;
use raptor_ffmpeg::Packet;
use raptor_subtitle::{SubtitleEngine, SubtitleEvent};

use crate::seek::{recv_current, Stamped};

/// 字幕默认显示时长（秒）
const DEFAULT_SUBTITLE_DURATION: f64 = 3.0;

/// 字幕包接收超时
const RECV_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

/// 字幕解码线程 — 读取 subtitle packets，解析文本，送入 SubtitleEngine
pub fn subtitle_decode_loop(
    pipeline: Arc<crate::pipeline::Pipeline>,
    subtitle_pkt_rx: Receiver<Stamped<Packet>>,
    subtitle_engine: Arc<Mutex<SubtitleEngine>>,
    is_text_subtitle: bool,
) -> raptor_core::Result<()> {
    tracing::info!("subtitle_decode_loop started (text={})", is_text_subtitle);

    let mut prev_pts: Option<f64> = None;
    let mut prev_text: Option<String> = None;
    let mut last_seek_gen: u64 = pipeline.seek_generation.load(Ordering::Acquire);
    // 内嵌字幕包不带时长，结束时间要等下一个包决定；因此一条字幕的事件在收到
    // **后一条**时才完成 —— 完成后立刻注入引擎，不能等循环退出（EOF 之后）
    let mut buffered: Vec<SubtitleEvent> = Vec::new();
    // seek 回退会把同一段字幕重新投递一遍，用已注入集合去重，
    // 否则事件表随 seek 次数无限增长且渲染端重复匹配
    let mut pushed: HashSet<(u64, String)> = HashSet::new();

    loop {
        if pipeline.shutdown.load(Ordering::Acquire) {
            break;
        }

        let seek_gen = pipeline.seek_generation.load(Ordering::Acquire);
        if seek_gen != last_seek_gen {
            last_seek_gen = seek_gen;
            // 上一条字幕的结束时间本应由下一个包决定，seek 后新位置的包不属于它；
            // 按默认时长收尾即可，直接丢弃会少一条字幕
            if let (Some(pts), Some(text)) = (prev_pts.take(), prev_text.take()) {
                push_event(
                    &mut buffered,
                    &mut pushed,
                    pts,
                    pts + DEFAULT_SUBTITLE_DURATION,
                    text,
                );
            }
        }

        match recv_current(&subtitle_pkt_rx, seek_gen, RECV_TIMEOUT) {
            Ok(stamped) => on_packet(
                stamped.item,
                is_text_subtitle,
                &mut prev_pts,
                &mut prev_text,
                &mut buffered,
                &mut pushed,
            ),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }

        // 超时、跳包等分支也必须走到这里：seek 收尾的事件若等下一个包才注入，
        // seek 之后就出现一段无字幕的空窗（极端情况下要等到 EOF 才显示）
        flush_buffered(&subtitle_engine, &mut buffered);
    }

    // flush 最后一条字幕
    if let (Some(pts), Some(text)) = (prev_pts, prev_text) {
        push_event(
            &mut buffered,
            &mut pushed,
            pts,
            pts + DEFAULT_SUBTITLE_DURATION,
            text,
        );
    }
    flush_buffered(&subtitle_engine, &mut buffered);

    tracing::info!("subtitle_decode_loop exiting");
    Ok(())
}

/// 处理一个字幕包
///
/// 内嵌字幕包只有起始时间没有时长，所以一条字幕要等**下一个**包到达才能确定
/// 结束时间；本包负责完成上一条，自己则成为新的"上一条"。
fn on_packet(
    pkt: Packet,
    is_text_subtitle: bool,
    prev_pts: &mut Option<f64>,
    prev_text: &mut Option<String>,
    buffered: &mut Vec<SubtitleEvent>,
    pushed: &mut HashSet<(u64, String)>,
) {
    // 无时间戳的字幕包无法定位显示时间，直接跳过（不得当成 0 秒）
    let Some(pkt_pts) = pkt.pts_secs() else {
        tracing::debug!("subtitle packet without pts, skipped");
        return;
    };
    // 非文本字幕格式（如 DVB bitmap），暂不支持
    if !is_text_subtitle {
        return;
    }
    let text = match extract_text_from_subtitle_packet(&pkt) {
        Some(text) if !text.trim().is_empty() => text,
        _ => return,
    };

    // 上一条字幕在此完成，结束时间取本包 pts
    if let (Some(start), Some(content)) = (prev_pts.take(), prev_text.take()) {
        let end_time = if pkt_pts > start && pkt_pts - start < 30.0 {
            pkt_pts
        } else {
            start + DEFAULT_SUBTITLE_DURATION
        };
        push_event(buffered, pushed, start, end_time, content);
    }
    *prev_pts = Some(pkt_pts);
    *prev_text = Some(text);
}

/// 记录一条字幕事件；重复的（seek 回退后重新投递）直接忽略
fn push_event(
    buffered: &mut Vec<SubtitleEvent>,
    pushed: &mut HashSet<(u64, String)>,
    start_time: f64,
    end_time: f64,
    text: String,
) {
    // 去重键不含 end_time：重新投递时相邻包序一致，结束时间必然相同，
    // 若把 end_time 也纳入键则 seek 后会插入视觉重复的同一条字幕
    if !pushed.insert((start_time.to_bits(), text.clone())) {
        return;
    }
    buffered.push(SubtitleEvent {
        start_time,
        end_time,
        text,
        style: "Default".to_string(),
    });
}

/// 把已完成的事件送入引擎，使其立即可被渲染端取到
fn flush_buffered(engine: &Arc<Mutex<SubtitleEngine>>, buffered: &mut Vec<SubtitleEvent>) {
    if buffered.is_empty() {
        return;
    }
    let events = std::mem::take(buffered);
    let count = events.len();
    engine.lock().append_events(events);
    tracing::debug!("subtitle_decode_loop: appended {} events", count);
}

/// 从字幕 packet 中提取文本内容
///
/// 支持格式：
/// - **mov_text**: 前 2 字节为 big-endian 长度前缀，后接 UTF-8 文本
/// - **subrip / SRT**: 直接 UTF-8 文本
/// - **ASS/SSA**: 直接 UTF-8 文本（含样式标记）
fn extract_text_from_subtitle_packet(pkt: &Packet) -> Option<String> {
    if pkt.data.is_empty() {
        return None;
    }

    // 尝试 mov_text 格式（2 字节长度前缀）
    if pkt.data.len() >= 2 {
        let len = ((pkt.data[0] as u16) << 8 | pkt.data[1] as u16) as usize;
        if len > 0 && len + 2 <= pkt.data.len() {
            if let Ok(text) = std::str::from_utf8(&pkt.data[2..2 + len]) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    // 回退：尝试直接解析为 UTF-8 文本
    if let Ok(text) = std::str::from_utf8(&pkt.data) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use raptor_subtitle::SubtitleConfig;

    /// 构造测试用字幕包（时间基 1/1000，pts 单位为毫秒）
    fn sub_packet(pts: Option<i64>, data: Vec<u8>) -> Packet {
        Packet {
            data,
            stream_index: 0,
            pts,
            dts: pts,
            time_base: raptor_ffmpeg::time_base(1, 1000),
            is_key: false,
        }
    }

    /// 轮询等待条件成立（用于观察"流未结束时的引擎状态"）
    fn wait_for(mut cond: impl FnMut() -> bool, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        cond()
    }

    /// 防回归：内嵌字幕必须在 **流结束之前** 就进入引擎。
    /// 旧实现把事件累积到局部 Vec、仅在循环退出后 load_events，
    /// 导致内嵌字幕轨在整个播放过程中一条都不显示。
    #[test]
    fn test_events_reach_engine_before_stream_end() {
        let (event_tx, _event_rx) = crossbeam_channel::unbounded();
        let pipeline = Arc::new(crate::pipeline::Pipeline::new(event_tx));
        let (pkt_tx, pkt_rx) = crossbeam_channel::bounded::<Stamped<Packet>>(8);
        let engine = Arc::new(Mutex::new(SubtitleEngine::new(SubtitleConfig::default())));
        let state = engine.lock().shared_state();

        let p = pipeline.clone();
        let e = engine.clone();
        let handle = std::thread::spawn(move || subtitle_decode_loop(p, pkt_rx, e, true));

        // 第一条的结束时间由第二条的 pts 决定，所以送入第二条后第一条才"完成"
        pkt_tx
            .send(Stamped::new(0, sub_packet(Some(1000), b"first".to_vec())))
            .unwrap();
        pkt_tx
            .send(Stamped::new(0, sub_packet(Some(2000), b"second".to_vec())))
            .unwrap();

        let arrived = wait_for(
            || !state.lock().events.is_empty(),
            std::time::Duration::from_secs(2),
        );
        assert!(
            arrived,
            "内嵌字幕必须在 EOF 之前就进入引擎，而不是等解码循环退出"
        );

        // 此时流仍未结束
        drop(pkt_tx);
        handle.join().unwrap().unwrap();

        let s = state.lock();
        assert_eq!(
            s.events.len(),
            2,
            "两条字幕都应注入（含最后一条按默认时长收尾）"
        );
        assert_eq!(s.events[0].start_time, 1.0);
        assert_eq!(s.events[0].end_time, 2.0, "结束时间应取后一条的 pts");
        assert_eq!(
            s.ass_events.len(),
            2,
            "渲染端以 ass_events 为数据源，必须同步追加"
        );
    }

    /// 防回归：seek 回退后重新投递的同一条字幕不得重复堆积
    #[test]
    fn test_seek_reattach_does_not_duplicate_events() {
        let (event_tx, _event_rx) = crossbeam_channel::unbounded();
        let pipeline = Arc::new(crate::pipeline::Pipeline::new(event_tx));
        let (pkt_tx, pkt_rx) = crossbeam_channel::bounded::<Stamped<Packet>>(16);
        let engine = Arc::new(Mutex::new(SubtitleEngine::new(SubtitleConfig::default())));
        let state = engine.lock().shared_state();

        let p = pipeline.clone();
        let e = engine.clone();
        let handle = std::thread::spawn(move || subtitle_decode_loop(p, pkt_rx, e, true));

        // 位置 A：1s → 2s
        for (gen, pts, text) in [(0u64, 1000i64, "first"), (0, 2000, "second")] {
            pipeline.seek_generation.store(gen, Ordering::Release);
            pkt_tx
                .send(Stamped::new(
                    gen,
                    sub_packet(Some(pts), text.as_bytes().to_vec()),
                ))
                .unwrap();
        }
        assert!(wait_for(
            || !state.lock().events.is_empty(),
            std::time::Duration::from_secs(2)
        ));

        // seek 回退：同一段字幕被重新投递（generation 递增）
        for (gen, pts, text) in [(1u64, 1000i64, "first"), (1, 2000, "second")] {
            pipeline.seek_generation.store(gen, Ordering::Release);
            pkt_tx
                .send(Stamped::new(
                    gen,
                    sub_packet(Some(pts), text.as_bytes().to_vec()),
                ))
                .unwrap();
        }

        // 等两轮都处理完
        assert!(wait_for(
            || state.lock().events.len() >= 2,
            std::time::Duration::from_millis(400)
        ));
        let dup = state.lock().events.len();
        drop(pkt_tx);
        handle.join().unwrap().unwrap();

        let s = state.lock();
        assert_eq!(
            s.events.len(),
            dup,
            "seek 后重新投递不应继续增加事件（去重生效）"
        );
        assert_eq!(s.events.len(), 2, "只应保留两条唯一字幕");
    }

    #[test]
    fn test_extract_mov_text() {
        // mov_text: 2 bytes length (big-endian) + text
        let text = "Hello World";
        let len = text.len() as u16;
        let mut data = vec![(len >> 8) as u8, (len & 0xFF) as u8];
        data.extend_from_slice(text.as_bytes());

        let pkt = sub_packet(Some(1000), data);

        let result = extract_text_from_subtitle_packet(&pkt);
        assert_eq!(result, Some("Hello World".to_string()));
        assert!((pkt.pts_secs().unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_extract_plain_text() {
        let pkt = sub_packet(Some(2000), b"plain subtitle text".to_vec());

        let result = extract_text_from_subtitle_packet(&pkt);
        assert_eq!(result, Some("plain subtitle text".to_string()));
    }

    #[test]
    fn test_extract_empty_packet() {
        let pkt = sub_packet(None, vec![]);

        let result = extract_text_from_subtitle_packet(&pkt);
        assert_eq!(result, None);
        // NOPTS 包不得被解释成 0 秒
        assert_eq!(pkt.pts_secs(), None);
    }
}
