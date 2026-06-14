//! 字幕解析器 — 支持 SRT 和 ASS/SSA 格式

use crate::types::{parse_ass_time, parse_srt_time, strip_ass_tags, SubtitleEvent};

/// 字幕解析器 trait
pub trait SubtitleParser: Send {
    fn parse(&self, data: &[u8]) -> Vec<SubtitleEvent>;
}

/// SRT 字幕解析器
///
/// 格式：
/// ```text
/// 1
/// 00:00:01,000 --> 00:00:05,000
/// 第一行字幕
/// 第二行字幕
///
/// 2
/// ...
/// ```
pub struct SrtParser;

impl SrtParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SrtParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SubtitleParser for SrtParser {
    fn parse(&self, data: &[u8]) -> Vec<SubtitleEvent> {
        let text = match std::str::from_utf8(data) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let mut events = Vec::new();
        let mut blocks = text.split("\n\n").peekable();

        for block in &mut blocks {
            let lines: Vec<&str> = block.lines().collect();
            if lines.len() < 3 {
                continue;
            }

            // 第 1 行：序号（可忽略）
            // 第 2 行：时间轴 "HH:MM:SS,mmm --> HH:MM:SS,mmm"
            let time_line = lines[1];
            let time_parts: Vec<&str> = time_line.split("-->").collect();
            if time_parts.len() != 2 {
                continue;
            }

            let start = match parse_srt_time(time_parts[0].trim()) {
                Some(t) => t,
                None => continue,
            };
            let end = match parse_srt_time(time_parts[1].trim()) {
                Some(t) => t,
                None => continue,
            };

            // 第 3 行及之后：字幕文本（可能多行）
            let subtitle_text: String = lines[2..].join("\n");
            if !subtitle_text.is_empty() {
                events.push(SubtitleEvent {
                    start_time: start,
                    end_time: end,
                    text: subtitle_text,
                    style: "Default".to_string(),
                });
            }
        }

        tracing::info!("SRT parser: {} events parsed", events.len());
        events
    }
}

/// ASS/SSA 字幕解析器
///
/// 解析 [Events] 部分中的 Dialogue 行：
/// `Dialogue: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text`
pub struct AssParser;

impl AssParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AssParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SubtitleParser for AssParser {
    fn parse(&self, data: &[u8]) -> Vec<SubtitleEvent> {
        let text = match std::str::from_utf8(data) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let mut events = Vec::new();
        let mut in_events = false;
        let mut format_fields: Vec<String> = Vec::new();

        for line in text.lines() {
            let trimmed = line.trim();

            // 检测 [Events] 段
            if trimmed.eq_ignore_ascii_case("[events]") {
                in_events = true;
                continue;
            }

            // 检测其他段（退出 Events 段）
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                in_events = false;
                continue;
            }

            if !in_events {
                continue;
            }

            // 解析 Format 行
            if trimmed.starts_with("Format:") {
                format_fields = trimmed[7..]
                    .split(',')
                    .map(|s| s.trim().to_lowercase())
                    .collect();
                continue;
            }

            // 解析 Dialogue 行
            if trimmed.starts_with("Dialogue:") {
                let data = &trimmed[9..];
                let fields: Vec<&str> = data.splitn(10, ',').collect(); // ASS 有 10 个字段

                if fields.len() < 10 {
                    continue;
                }

                // 查找 Start, End, Style, Text 的索引
                let start_idx = format_fields.iter().position(|f| f == "start").unwrap_or(1);
                let end_idx = format_fields.iter().position(|f| f == "end").unwrap_or(2);
                let style_idx = format_fields.iter().position(|f| f == "style").unwrap_or(3);
                let text_idx = format_fields.iter().position(|f| f == "text").unwrap_or(9);

                let start = match parse_ass_time(fields[start_idx].trim()) {
                    Some(t) => t,
                    None => continue,
                };
                let end = match parse_ass_time(fields[end_idx].trim()) {
                    Some(t) => t,
                    None => continue,
                };

                let style = fields[style_idx].trim().to_string();
                let raw_text = fields[text_idx].trim();
                let text = strip_ass_tags(raw_text);

                if !text.is_empty() {
                    events.push(SubtitleEvent {
                        start_time: start,
                        end_time: end,
                        text: text.replace("\\N", "\n"), // ASS 换行符
                        style,
                    });
                }
            }
        }

        tracing::info!("ASS parser: {} events parsed", events.len());
        events
    }
}

/// 根据文件扩展名选择解析器
pub fn parser_for_extension(ext: &str) -> Box<dyn SubtitleParser> {
    match ext.to_lowercase().as_str() {
        "srt" => Box::new(SrtParser::new()),
        "ass" | "ssa" => Box::new(AssParser::new()),
        _ => Box::new(SrtParser::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_srt_parser() {
        let srt = "1\n00:00:01,000 --> 00:00:05,000\nHello World\n\n2\n00:00:06,000 --> 00:00:10,000\nSecond subtitle\n";
        let parser = SrtParser::new();
        let events = parser.parse(srt.as_bytes());
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].start_time, 1.0);
        assert_eq!(events[0].end_time, 5.0);
        assert_eq!(events[0].text, "Hello World");
        assert_eq!(events[1].text, "Second subtitle");
    }

    #[test]
    fn test_srt_parser_multiline() {
        let srt = "1\n00:00:01,000 --> 00:00:05,000\nLine 1\nLine 2\n";
        let parser = SrtParser::new();
        let events = parser.parse(srt.as_bytes());
        assert_eq!(events.len(), 1);
        assert!(events[0].text.contains("Line 1"));
        assert!(events[0].text.contains("Line 2"));
    }

    #[test]
    fn test_ass_parser() {
        let ass = r#"[Script Info]
Title: Test

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour
Style: Default,Arial,20,&H00FFFFFF

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,Hello World
Dialogue: 0,0:00:06.00,0:00:10.00,Default,,0,0,0,,{\b1}Bold text
"#;
        let parser = AssParser::new();
        let events = parser.parse(ass.as_bytes());
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].text, "Hello World");
        assert_eq!(events[1].text, "Bold text"); // tags stripped
        assert_eq!(events[0].style, "Default");
    }

    #[test]
    fn test_ass_parser_line_break() {
        let ass = r#"[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,Line 1\NLine 2
"#;
        let parser = AssParser::new();
        let events = parser.parse(ass.as_bytes());
        assert_eq!(events.len(), 1);
        assert!(events[0].text.contains('\n'));
    }

    #[test]
    fn test_parser_for_extension() {
        let _srt = parser_for_extension("srt");
        let _ass = parser_for_extension("ass");
        let _ssa = parser_for_extension("ssa");
    }

    #[test]
    fn test_srt_parser_chinese() {
        let srt = "1\n00:00:01,000 --> 00:00:05,000\n你好世界\n";
        let parser = SrtParser::new();
        let events = parser.parse(srt.as_bytes());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].text, "你好世界");
    }
}
