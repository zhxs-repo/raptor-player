//! 字幕解码线程 — 从 subtitle packet 提取文本并送入 SubtitleEngine

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

    let mut events: Vec<SubtitleEvent> = Vec::new();
    let mut prev_pts: Option<f64> = None;
    let mut prev_text: Option<String> = None;
    let mut last_seek_gen: u64 = pipeline.seek_generation.load(Ordering::Acquire);

    loop {
        if pipeline.shutdown.load(Ordering::Acquire) {
            break;
        }

        let seek_gen = pipeline.seek_generation.load(Ordering::Acquire);
        if seek_gen != last_seek_gen {
            last_seek_gen = seek_gen;
            // 上一条字幕的结束时间本应由下一个包决定，seek 后不会再来了；
            // 事件带绝对时间戳，按默认时长收尾即可，直接丢弃会少一条字幕
            if let (Some(pts), Some(text)) = (prev_pts.take(), prev_text.take()) {
                events.push(SubtitleEvent {
                    start_time: pts,
                    end_time: pts + DEFAULT_SUBTITLE_DURATION,
                    text,
                    style: "Default".to_string(),
                });
            }
        }

        match recv_current(&subtitle_pkt_rx, seek_gen, RECV_TIMEOUT) {
            Ok(stamped) => {
                let pkt = stamped.item;
                // 无时间戳的字幕包无法定位显示时间，直接跳过（不得当成 0 秒）
                let Some(pkt_pts) = pkt.pts_secs() else {
                    tracing::debug!("subtitle packet without pts, skipped");
                    continue;
                };
                let text = if is_text_subtitle {
                    extract_text_from_subtitle_packet(&pkt)
                } else {
                    // 非文本字幕格式（如 DVB bitmap），暂不支持
                    continue;
                };

                if let Some(text) = text {
                    if !text.trim().is_empty() {
                        // 如果有上一条，用当前 pts 作为上一条的结束时间
                        if let (Some(prev_pts), Some(prev_text)) =
                            (prev_pts.take(), prev_text.take())
                        {
                            let end_time = if pkt_pts > prev_pts && pkt_pts - prev_pts < 30.0 {
                                pkt_pts
                            } else {
                                prev_pts + DEFAULT_SUBTITLE_DURATION
                            };
                            events.push(SubtitleEvent {
                                start_time: prev_pts,
                                end_time,
                                text: prev_text,
                                style: "Default".to_string(),
                            });
                        }
                        prev_pts = Some(pkt_pts);
                        prev_text = Some(text);
                    }
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    // flush 最后一条字幕
    if let (Some(pts), Some(text)) = (prev_pts, prev_text) {
        events.push(SubtitleEvent {
            start_time: pts,
            end_time: pts + DEFAULT_SUBTITLE_DURATION,
            text,
            style: "Default".to_string(),
        });
    }

    // 送入 SubtitleEngine
    if !events.is_empty() {
        tracing::info!(
            "subtitle_decode_loop: loaded {} events into engine",
            events.len()
        );
        subtitle_engine.lock().load_events(events);
    }

    tracing::info!("subtitle_decode_loop exiting");
    Ok(())
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
