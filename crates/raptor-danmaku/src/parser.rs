//! 弹幕解析器 — 支持 B 站 XML 格式和 JSON 格式

use crate::types::{DanmakuItem, DanmakuMode};

/// 弹幕解析器 trait — 可扩展的弹幕格式解析接口
pub trait DanmakuParser: Send {
    /// 解析弹幕数据，返回弹幕列表（按 time_ms 升序）
    fn parse(&self, data: &[u8]) -> Vec<DanmakuItem>;
}

/// B 站 XML 弹幕解析器
///
/// 格式示例：
/// ```xml
/// <i>
///   <d p="1.23,1,25,16777215,1234567890,0,abc123,12345">弹幕文本</d>
/// </i>
/// ```
/// p 属性字段：time(秒),mode,size,color,timestamp,pool,userid,rowid
pub struct BilibiliXmlParser;

impl BilibiliXmlParser {
    pub fn new() -> Self {
        Self
    }

    /// 从 XML 字符串中解析弹幕（不依赖完整 XML 库，使用简单字符串解析）
    fn parse_xml_string(&self, xml: &str) -> Vec<DanmakuItem> {
        let mut items = Vec::new();

        // 查找所有 <d p="...">...</d> 标签
        let mut pos = 0;
        while let Some(d_start) = xml[pos..].find("<d p=\"") {
            let d_start = pos + d_start;
            let p_start = d_start + 6; // 跳过 <d p="
            let p_end = match xml[p_start..].find('"') {
                Some(i) => p_start + i,
                None => break,
            };
            let p_str = &xml[p_start..p_end];

            // 查找文本内容：> ... </d>
            let text_start = match xml[p_end..].find('>') {
                Some(i) => p_end + i + 1,
                None => break,
            };
            let text_end = match xml[text_start..].find("</d>") {
                Some(i) => text_start + i,
                None => break,
            };
            let text = xml[text_start..text_end].trim().to_string();

            // 解析 p 属性：time,mode,size,color,timestamp,pool,userid,rowid
            let fields: Vec<&str> = p_str.split(',').collect();
            if fields.len() >= 4 {
                let time_secs: f64 = fields[0].parse().unwrap_or(0.0);
                let mode_num: u8 = fields[1].parse().unwrap_or(1);
                let size: u32 = fields[2].parse().unwrap_or(25);
                let color: u32 = fields[3].parse::<u32>().unwrap_or(0xFFFFFF) & 0xFFFFFF;

                if !text.is_empty() {
                    items.push(DanmakuItem {
                        time_ms: (time_secs * 1000.0) as u64,
                        mode: DanmakuMode::from_bilibili_mode(mode_num),
                        font_size: size,
                        color,
                        text,
                    });
                }
            }

            pos = text_end + 4; // 跳过 </d>
        }

        // 按时间升序排序
        items.sort_by_key(|item| item.time_ms);
        items
    }
}

impl Default for BilibiliXmlParser {
    fn default() -> Self {
        Self::new()
    }
}

impl DanmakuParser for BilibiliXmlParser {
    fn parse(&self, data: &[u8]) -> Vec<DanmakuItem> {
        let xml = match std::str::from_utf8(data) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("BilibiliXmlParser: invalid UTF-8: {}", e);
                return Vec::new();
            }
        };
        self.parse_xml_string(xml)
    }
}

/// JSON 弹幕解析器
///
/// 格式：JSON 数组，每个元素包含 time_ms/mode/font_size/color/text
/// ```json
/// [
///   {"time_ms": 1000, "mode": "ScrollRight", "font_size": 25, "color": 16777215, "text": "弹幕"}
/// ]
/// ```
pub struct JsonParser;

impl JsonParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for JsonParser {
    fn default() -> Self {
        Self::new()
    }
}

impl DanmakuParser for JsonParser {
    fn parse(&self, data: &[u8]) -> Vec<DanmakuItem> {
        match serde_json::from_slice::<Vec<DanmakuItem>>(data) {
            Ok(mut items) => {
                items.sort_by_key(|item| item.time_ms);
                items
            }
            Err(e) => {
                tracing::warn!("JsonParser: parse error: {}", e);
                Vec::new()
            }
        }
    }
}

/// 根据文件扩展名自动选择解析器
pub fn parser_for_extension(ext: &str) -> Box<dyn DanmakuParser> {
    match ext.to_lowercase().as_str() {
        "xml" => Box::new(BilibiliXmlParser::new()),
        "json" => Box::new(JsonParser::new()),
        _ => Box::new(JsonParser::new()), // 默认 JSON
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bilibili_xml_parser() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<i>
<d p="1.23000,1,25,16777215,1234567890,0,abc123,12345">hello world</d>
<d p="2.50000,5,25,65280,1234567891,0,def456,12346">green top</d>
<d p="3.00000,4,30,16711680,1234567892,0,ghi789,12347">red bottom</d>
</i>"#;
        let parser = BilibiliXmlParser::new();
        let items = parser.parse(xml.as_bytes());
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].time_ms, 1230);
        assert_eq!(items[0].mode, DanmakuMode::ScrollRight);
        assert_eq!(items[0].text, "hello world");
        assert_eq!(items[1].time_ms, 2500);
        assert_eq!(items[1].mode, DanmakuMode::TopFixed);
        assert_eq!(items[1].color, 65280);
        assert_eq!(items[2].time_ms, 3000);
        assert_eq!(items[2].mode, DanmakuMode::BottomFixed);
        assert_eq!(items[2].font_size, 30);
    }

    #[test]
    fn test_bilibili_xml_empty() {
        let parser = BilibiliXmlParser::new();
        let items = parser.parse(b"<i></i>");
        assert!(items.is_empty());
    }

    #[test]
    fn test_bilibili_xml_invalid_utf8() {
        let parser = BilibiliXmlParser::new();
        let items = parser.parse(&[0xFF, 0xFE]);
        assert!(items.is_empty());
    }

    #[test]
    fn test_json_parser() {
        let json = r#"[
            {"time_ms": 500, "mode": "ScrollRight", "font_size": 25, "color": 16777215, "text": "first"},
            {"time_ms": 200, "mode": "TopFixed", "font_size": 20, "color": 65280, "text": "second"}
        ]"#;
        let parser = JsonParser::new();
        let items = parser.parse(json.as_bytes());
        assert_eq!(items.len(), 2);
        // 应按 time_ms 升序排序
        assert_eq!(items[0].time_ms, 200);
        assert_eq!(items[1].time_ms, 500);
    }

    #[test]
    fn test_json_parser_invalid() {
        let parser = JsonParser::new();
        let items = parser.parse(b"not json");
        assert!(items.is_empty());
    }

    #[test]
    fn test_parser_for_extension() {
        let _xml_parser = parser_for_extension("xml");
        let _json_parser = parser_for_extension("json");
        let _default_parser = parser_for_extension("txt");
    }

    #[test]
    fn test_bilibili_xml_chinese_text() {
        let xml = r#"<i><d p="1.0,1,25,16777215,0,0,u,0">你好世界</d></i>"#;
        let parser = BilibiliXmlParser::new();
        let items = parser.parse(xml.as_bytes());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "你好世界");
    }
}
