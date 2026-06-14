//! 字幕解码线程 — 从 subtitle packet 提取文本并送入 SubtitleEngine

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crossbeam_channel::Receiver;
use parking_lot::Mutex;
use raptor_ffmpeg::Packet;
use raptor_subtitle::{SubtitleEngine, SubtitleEvent};

/// 字幕默认显示时长（秒）
const DEFAULT_SUBTITLE_DURATION: f64 = 3.0;

/// 字幕解码线程 — 读取 subtitle packets，解析文本，送入 SubtitleEngine
pub fn subtitle_decode_loop(
    pipeline: Arc<crate::pipeline::Pipeline>,
    subtitle_pkt_rx: Receiver<Packet>,
    subtitle_engine: Arc<Mutex<SubtitleEngine>>,
    is_text_subtitle: bool,
) -> raptor_core::Result<()> {
    tracing::info!("subtitle_decode_loop started (text={})", is_text_subtitle);

    let mut events: Vec<SubtitleEvent> = Vec::new();
    let mut prev_pts: Option<f64> = None;
    let mut prev_text: Option<String> = None;

    loop {
        if pipeline.shutdown.load(Ordering::Acquire) {
            break;
        }

        match subtitle_pkt_rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(pkt) => {
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
                            let end_time = if pkt.pts > prev_pts && pkt.pts - prev_pts < 30.0 {
                                pkt.pts
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
                        prev_pts = Some(pkt.pts);
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

    #[test]
    fn test_extract_mov_text() {
        // mov_text: 2 bytes length (big-endian) + text
        let text = "Hello World";
        let len = text.len() as u16;
        let mut data = vec![(len >> 8) as u8, (len & 0xFF) as u8];
        data.extend_from_slice(text.as_bytes());

        let pkt = Packet {
            data,
            stream_index: 0,
            pts: 1.0,
            dts: 1.0,
            is_key: false,
        };

        let result = extract_text_from_subtitle_packet(&pkt);
        assert_eq!(result, Some("Hello World".to_string()));
    }

    #[test]
    fn test_extract_plain_text() {
        let pkt = Packet {
            data: b"plain subtitle text".to_vec(),
            stream_index: 0,
            pts: 2.0,
            dts: 2.0,
            is_key: false,
        };

        let result = extract_text_from_subtitle_packet(&pkt);
        assert_eq!(result, Some("plain subtitle text".to_string()));
    }

    #[test]
    fn test_extract_empty_packet() {
        let pkt = Packet {
            data: vec![],
            stream_index: 0,
            pts: 0.0,
            dts: 0.0,
            is_key: false,
        };

        let result = extract_text_from_subtitle_packet(&pkt);
        assert_eq!(result, None);
    }
}
