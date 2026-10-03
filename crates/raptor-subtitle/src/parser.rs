//! 字幕解析器 — 支持 SRT 和 ASS/SSA 格式

use crate::ass::{
    parse_ass_colour, split_override_segments, Alignment, AssDocument, AssEvent, AssStyle,
};
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
/// 完整解析 [Script Info]（PlayResX/Y）、[V4+ Styles]（样式表）与
/// [Events]（Dialogue 行，含覆盖标签）；见 `AssParser::parse_document`。
pub struct AssParser;

#[derive(PartialEq)]
enum Section {
    None,
    ScriptInfo,
    Styles,
    Events,
}

fn default_style_fields() -> Vec<String> {
    "Name,Fontname,Fontsize,PrimaryColour,SecondaryColour,OutlineColour,BackColour,\
     Bold,Italic,Underline,StrikeOut,ScaleX,ScaleY,Spacing,Angle,BorderStyle,Outline,\
     Shadow,Alignment,MarginL,MarginR,MarginV,Encoding"
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .collect()
}

fn default_event_fields() -> Vec<String> {
    "Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text"
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .collect()
}

fn parse_field_list(rest: &str) -> Vec<String> {
    rest.split(',').map(|s| s.trim().to_lowercase()).collect()
}

fn parse_style_line(rest: &str, format: &[String]) -> Option<AssStyle> {
    let fields: Vec<&str> = rest.splitn(format.len().max(1), ',').collect();
    let get = |name: &str| {
        format
            .iter()
            .position(|f| f == name)
            .and_then(|i| fields.get(i).map(|s| s.trim()))
    };

    let alignment = get("alignment")
        .and_then(|v| v.parse::<u8>().ok())
        .map(Alignment)
        .filter(|a| a.is_valid())
        .unwrap_or_else(Alignment::default_bottom_center);

    Some(AssStyle {
        name: get("name")?.to_string(),
        font_name: get("fontname").unwrap_or("").to_string(),
        font_size: get("fontsize").and_then(|v| v.parse().ok()).unwrap_or(0.0),
        primary_colour: get("primarycolour")
            .and_then(parse_ass_colour)
            .unwrap_or([1.0, 1.0, 1.0, 1.0]),
        alignment,
        margin_l: get("marginl").and_then(|v| v.parse().ok()).unwrap_or(0.0),
        margin_r: get("marginr").and_then(|v| v.parse().ok()).unwrap_or(0.0),
        margin_v: get("marginv").and_then(|v| v.parse().ok()).unwrap_or(0.0),
    })
}

fn parse_dialogue(rest: &str, format: &[String]) -> Option<AssEvent> {
    let fields: Vec<&str> = rest.splitn(10.max(format.len()), ',').collect();
    let get = |name: &str, fallback: usize| -> &str {
        let i = format.iter().position(|f| f == name).unwrap_or(fallback);
        fields.get(i).copied().unwrap_or("").trim()
    };

    let start = parse_ass_time(get("start", 1))?;
    let end = parse_ass_time(get("end", 2))?;
    let style = get("style", 3).to_string();
    let raw_text = get("text", 9);
    let plain = strip_ass_tags(raw_text).replace("\\N", "\n");
    if plain.trim().is_empty() {
        return None;
    }

    Some(AssEvent {
        base: SubtitleEvent {
            start_time: start,
            end_time: end,
            text: plain,
            style,
        },
        segments: split_override_segments(raw_text),
        layer: get("layer", 0).parse().unwrap_or(0),
        margin_l: get("marginl", 5).parse().unwrap_or(0.0),
        margin_r: get("marginr", 6).parse().unwrap_or(0.0),
        margin_v: get("marginv", 7).parse().unwrap_or(0.0),
    })
}

impl AssParser {
    pub fn new() -> Self {
        Self
    }

    /// 解析完整 ASS 文档：画布分辨率、样式表、事件（含覆盖标签段）
    pub fn parse_document(&self, data: &[u8]) -> AssDocument {
        let text = match std::str::from_utf8(data) {
            Ok(s) => s,
            Err(_) => return AssDocument::default(),
        };

        let mut doc = AssDocument::default();
        let mut section = Section::None;
        let mut style_format = default_style_fields();
        let mut event_format = default_event_fields();

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with(';') {
                continue;
            }

            if trimmed.starts_with('[') {
                let lower = trimmed.to_lowercase();
                section = if lower == "[script info]" {
                    Section::ScriptInfo
                } else if lower.starts_with("[v4") && lower.contains("styles") {
                    Section::Styles
                } else if lower == "[events]" {
                    Section::Events
                } else {
                    Section::None
                };
                continue;
            }

            match section {
                Section::ScriptInfo => {
                    if let Some((key, value)) = trimmed.split_once(':') {
                        let value = value.trim();
                        match key.trim().to_lowercase().as_str() {
                            "playresx" => doc.play_res_x = value.parse().unwrap_or(0.0),
                            "playresy" => doc.play_res_y = value.parse().unwrap_or(0.0),
                            _ => {}
                        }
                    }
                }
                Section::Styles => {
                    if let Some(rest) = trimmed.strip_prefix("Format:") {
                        style_format = parse_field_list(rest);
                    } else if let Some(rest) = trimmed.strip_prefix("Style:") {
                        if let Some(style) = parse_style_line(rest, &style_format) {
                            doc.styles.insert(style.name.clone(), style);
                        }
                    }
                }
                Section::Events => {
                    if let Some(rest) = trimmed.strip_prefix("Format:") {
                        event_format = parse_field_list(rest);
                    } else if let Some(rest) = trimmed.strip_prefix("Dialogue:") {
                        if let Some(event) = parse_dialogue(rest, &event_format) {
                            doc.events.push(event);
                        }
                    }
                }
                Section::None => {}
            }
        }

        tracing::info!(
            "ASS parser: {} events, {} styles, play_res={:.0}x{:.0}",
            doc.events.len(),
            doc.styles.len(),
            doc.play_res_x,
            doc.play_res_y
        );
        doc
    }
}

impl Default for AssParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SubtitleParser for AssParser {
    /// 向后兼容入口：只返回去标签的纯文本事件
    fn parse(&self, data: &[u8]) -> Vec<SubtitleEvent> {
        self.parse_document(data)
            .events
            .into_iter()
            .map(|e| e.base)
            .collect()
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
