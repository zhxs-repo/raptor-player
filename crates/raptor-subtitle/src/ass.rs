//! ASS 样式表与覆盖标签（{\...}）解析
//!
//! 覆盖常见特效子集：`\pos`、`\move`、`\fad`、`\an`、`\c`/`\1c`、`\fs`，
//! 以及 `[V4+ Styles]` 样式行与 `[Script Info]` PlayResX/PlayResY。
//! 不支持的标签（`\t`/`\fr`/`\blur`/`\bord`/`\k` 等）安全忽略（文本仍会显示）。

use std::collections::HashMap;

/// ASS 对齐编号（numpad 布局）：1=左下 … 9=右上
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alignment(pub u8);

impl Alignment {
    pub fn default_bottom_center() -> Self {
        Self(2)
    }

    /// 水平锚点：0=左 1=中 2=右
    pub fn h(self) -> u8 {
        (self.0 - 1) % 3
    }

    /// 垂直锚点：0=下 1=中 2=上
    pub fn v(self) -> u8 {
        (self.0 - 1) / 3
    }

    pub fn is_valid(self) -> bool {
        (1..=9).contains(&self.0)
    }
}

/// 解析 &HAABBGGRR（或 &HBBGGRR、十进制）ASS 颜色 → RGBA（0..1，直通 alpha）
///
/// ASS 的 alpha 字节是反义的：0x00 = 不透明，0xFF = 完全透明。
pub fn parse_ass_colour(s: &str) -> Option<[f32; 4]> {
    let s = s.trim().trim_end_matches('&').trim();
    let hex = if let Some(h) = s
        .strip_prefix("&H")
        .or_else(|| s.strip_prefix("&h"))
        .or_else(|| s.strip_prefix("H"))
    {
        h
    } else if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        h
    } else if s.chars().all(|c| c.is_ascii_digit()) {
        // 纯十进制：与 6 位十六进制同一编码（低位字节 = R）
        let v: u32 = s.parse().ok()?;
        return Some([
            (v & 0xFF) as f32 / 255.0,
            ((v >> 8) & 0xFF) as f32 / 255.0,
            ((v >> 16) & 0xFF) as f32 / 255.0,
            1.0,
        ]);
    } else {
        return None;
    };
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) || hex.len() > 8 {
        return None;
    }
    let v: u32 = u32::from_str_radix(hex, 16).ok()?;
    let (a, b, g, r) = if hex.len() >= 8 {
        (
            ((v >> 24) & 0xFF) as u8,
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
        )
    } else {
        (
            0u8,
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
        )
    };
    Some([
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        (255 - a) as f32 / 255.0,
    ])
}

/// `\move` 参数
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AssMove {
    pub from: (f32, f32),
    pub to: (f32, f32),
    /// 移动起止时间（相对事件开始，毫秒；默认 0..事件时长）
    pub t1_ms: f64,
    pub t2_ms: f64,
}

/// 一个覆盖块 `{...}` 中本引擎支持的标签
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AssTags {
    pub pos: Option<(f32, f32)>,
    pub move_: Option<AssMove>,
    /// (\fad 淡入时长 ms, 淡出时长 ms)
    pub fade: Option<(f64, f64)>,
    pub alignment: Option<Alignment>,
    /// 主颜色（RGBA 0..1）
    pub color: Option<[f32; 4]>,
    pub font_size: Option<f32>,
}

impl AssTags {
    /// 用后出现的块覆盖先前值（后写赢）
    pub fn apply_overlay(&mut self, other: &AssTags) {
        macro_rules! overwrite {
            ($($field:ident),* $(,)?) => {
                $(if other.$field.is_some() { self.$field = other.$field; })*
            };
        }
        overwrite!(pos, move_, fade, alignment, color, font_size);
    }

    pub fn is_empty(&self) -> bool {
        self == &AssTags::default()
    }
}

fn split_args(args: &str) -> Vec<&str> {
    args.split(',').map(|s| s.trim()).collect()
}

/// 解析单个覆盖块内容（不含花括号）
pub fn parse_tag_block(inner: &str) -> AssTags {
    let mut tags = AssTags::default();
    for chunk in inner.split('\\') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        let name_end = chunk
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(chunk.len());
        let (name, rest) = chunk.split_at(name_end);
        // 参数取第一个 ')' 之前的内容：容忍 Danmaku2ASS 等生成器在 ')' 后附加的残缺标签
        let args = match rest.strip_prefix('(') {
            Some(inner) => inner.split(')').next().unwrap_or(inner),
            None => rest,
        };
        match name.to_ascii_lowercase().as_str() {
            "an" => {
                if let Ok(n) = args.trim().parse::<u8>() {
                    let a = Alignment(n);
                    if a.is_valid() {
                        tags.alignment = Some(a);
                    }
                }
            }
            "pos" => {
                let a = split_args(args);
                if a.len() >= 2 {
                    if let (Ok(x), Ok(y)) = (a[0].parse::<f32>(), a[1].parse::<f32>()) {
                        tags.pos = Some((x, y));
                    }
                }
            }
            "move" => {
                let a = split_args(args);
                if a.len() >= 4 {
                    let nums: Option<Vec<f32>> =
                        a[..4].iter().map(|s| s.parse::<f32>().ok()).collect();
                    if let Some(n) = nums {
                        let t1 = a.get(4).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                        let t2 = a
                            .get(5)
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(f64::NAN);
                        tags.move_ = Some(AssMove {
                            from: (n[0], n[1]),
                            to: (n[2], n[3]),
                            t1_ms: t1,
                            t2_ms: t2,
                        });
                    }
                }
            }
            "fad" => {
                let a = split_args(args);
                if a.len() >= 2 {
                    if let (Ok(t1), Ok(t2)) = (a[0].parse::<f64>(), a[1].parse::<f64>()) {
                        tags.fade = Some((t1.max(0.0), t2.max(0.0)));
                    }
                }
            }
            "c" | "1c" => {
                if let Some(c) = parse_ass_colour(args) {
                    tags.color = Some(c);
                }
            }
            "fs" => {
                if let Ok(v) = args.trim().parse::<f32>() {
                    if v > 0.0 {
                        tags.font_size = Some(v);
                    }
                }
            }
            _ => {
                // 其余标签（\b \i \fn \fr \t \blur \bord \shad \k …）忽略，文本照常渲染
            }
        }
    }
    tags
}

/// 把带覆盖块的原始文本切成 (标签, 文本) 段序列
///
/// 块只对其后文本生效；相邻多个块按“后写赢”合并进同一段。
pub fn split_override_segments(raw: &str) -> Vec<(AssTags, String)> {
    let mut segments = Vec::new();
    let mut pending = AssTags::default();
    let mut text = String::new();
    let mut chars = raw.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
        match ch {
            '{' if idx == 0 || !raw[..idx].ends_with('\\') => {
                if !text.is_empty() {
                    segments.push((pending, std::mem::take(&mut text)));
                }
                // 找到匹配的 '}'（缺失则把剩余当文本）
                let rest = &raw[idx + 1..];
                match rest.find('}') {
                    Some(end) => {
                        pending.apply_overlay(&parse_tag_block(&rest[..end]));
                        // 跳过块内容
                        let skip = idx + 1 + end + 1;
                        while chars.peek().is_some_and(|&(next_idx, _)| next_idx < skip) {
                            chars.next();
                        }
                    }
                    None => text.push(ch),
                }
            }
            '}' => {
                if !text.is_empty() {
                    segments.push((pending, std::mem::take(&mut text)));
                }
            }
            _ => text.push(ch),
        }
    }
    if !text.is_empty() || segments.is_empty() {
        segments.push((pending, text));
    }
    segments
}

/// `[V4+ Styles]` 样式行
#[derive(Debug, Clone)]
pub struct AssStyle {
    pub name: String,
    pub font_name: String,
    pub font_size: f32,
    pub primary_colour: [f32; 4],
    pub alignment: Alignment,
    pub margin_l: f32,
    pub margin_r: f32,
    pub margin_v: f32,
}

impl Default for AssStyle {
    fn default() -> Self {
        Self {
            name: "Default".to_string(),
            font_name: String::new(),
            font_size: 0.0, // 0 = 使用 config.default_font_size
            primary_colour: [1.0, 1.0, 1.0, 1.0],
            alignment: Alignment::default_bottom_center(),
            margin_l: 0.0,
            margin_r: 0.0,
            margin_v: 0.0, // 0 = 使用 config.bottom_margin
        }
    }
}

/// 一条 Dialogue（保留覆盖标签信息；base.text 为去标签纯文本，向后兼容）
#[derive(Debug, Clone)]
pub struct AssEvent {
    pub base: crate::types::SubtitleEvent,
    pub segments: Vec<(AssTags, String)>,
    pub layer: i32,
    pub margin_l: f32,
    pub margin_r: f32,
    pub margin_v: f32,
}

impl AssEvent {
    /// 无特效的纯文本事件（SRT、内嵌字幕走这条路）
    pub fn plain(base: crate::types::SubtitleEvent) -> Self {
        Self {
            base,
            segments: Vec::new(),
            layer: 0,
            margin_l: 0.0,
            margin_r: 0.0,
            margin_v: 0.0,
        }
    }

    pub fn has_effects(&self) -> bool {
        self.segments.iter().any(|(t, _)| !t.is_empty())
    }
}

/// 完整 ASS 文档（样式表 + 画布分辨率 + 事件）
#[derive(Debug, Clone, Default)]
pub struct AssDocument {
    pub play_res_x: f32,
    pub play_res_y: f32,
    pub styles: HashMap<String, AssStyle>,
    pub events: Vec<AssEvent>,
}

/// `\move` 在 pts 时刻的插值位置（画布坐标）
pub fn eval_move(mv: &AssMove, pts: f64, event_start: f64, event_end: f64) -> (f32, f32) {
    let duration_ms = ((event_end - event_start) * 1000.0).max(0.0);
    let t1 = mv.t1_ms;
    let t2 = if mv.t2_ms.is_nan() {
        duration_ms
    } else {
        mv.t2_ms
    };
    let rel = (pts - event_start) * 1000.0;
    let u: f32 = if t2 <= t1 {
        1.0
    } else {
        ((rel - t1) / (t2 - t1)).clamp(0.0, 1.0) as f32
    };
    (
        mv.from.0 + (mv.to.0 - mv.from.0) * u,
        mv.from.1 + (mv.to.1 - mv.from.1) * u,
    )
}

/// `\fad` 在 pts 时刻的整体 alpha（0..1，1=完全不透明）
pub fn eval_fade(fade: (f64, f64), pts: f64, event_start: f64, event_end: f64) -> f32 {
    let (t_in, t_out) = fade;
    let mut alpha = 1.0f64;
    if t_in > 0.0 {
        alpha = alpha.min((pts - event_start) * 1000.0 / t_in);
    }
    if t_out > 0.0 {
        alpha = alpha.min((event_end - pts) * 1000.0 / t_out);
    }
    alpha.clamp(0.0, 1.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_colour() {
        // &HAABBGGRR：alpha=00 不透明
        assert_eq!(parse_ass_colour("&H00FFFFFF&"), Some([1.0, 1.0, 1.0, 1.0]));
        // R=FF → red
        assert_eq!(parse_ass_colour("&H000000FF"), Some([1.0, 0.0, 0.0, 1.0]));
        // B=FF → blue
        assert_eq!(parse_ass_colour("&H00FF0000"), Some([0.0, 0.0, 1.0, 1.0]));
        // alpha=7F 半透明
        let c = parse_ass_colour("&H7F0000FF").unwrap();
        assert!((c[3] - 0.506).abs() < 0.01);
        // 6 位无 alpha
        assert_eq!(parse_ass_colour("&H0000FF"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_ass_colour("16711680"), Some([0.0, 0.0, 1.0, 1.0]));
        assert_eq!(parse_ass_colour("garbage"), None);
        assert_eq!(parse_ass_colour(""), None);
    }

    #[test]
    fn test_alignment_mapping() {
        assert_eq!(Alignment(1).h(), 0); // 左
        assert_eq!(Alignment(2).h(), 1); // 中
        assert_eq!(Alignment(3).h(), 2); // 右
        assert_eq!(Alignment(2).v(), 0); // 下
        assert_eq!(Alignment(5).v(), 1); // 中
        assert_eq!(Alignment(9).v(), 2); // 右上
        assert!(Alignment(0).0 == 0 && !Alignment(0).is_valid());
        assert!(!Alignment(10).is_valid());
    }

    #[test]
    fn test_tag_block_supported_tags() {
        let t = parse_tag_block(r"an8\pos(100,200)\fs36");
        assert_eq!(t.alignment, Some(Alignment(8)));
        assert_eq!(t.pos, Some((100.0, 200.0)));
        assert_eq!(t.font_size, Some(36.0));

        let t = parse_tag_block(r"move(0,50,800,50)");
        let mv = t.move_.unwrap();
        assert_eq!(mv.from, (0.0, 50.0));
        assert_eq!(mv.to, (800.0, 50.0));
        assert!(mv.t2_ms.is_nan()); // 缺省 = 事件时长

        let t = parse_tag_block(r"move(0,0,10,10,500,1500)");
        let mv = t.move_.unwrap();
        assert_eq!((mv.t1_ms, mv.t2_ms), (500.0, 1500.0));

        let t = parse_tag_block(r"fad(500,800)");
        assert_eq!(t.fade, Some((500.0, 800.0)));

        let t = parse_tag_block(r"c&H0000FF&");
        assert_eq!(t.color, Some([1.0, 0.0, 0.0, 1.0]));

        // 未知标签安全忽略
        let t = parse_tag_block(r"frz45\t(0,100,\b1)\blur3");
        assert_eq!(t, AssTags::default());

        // Danmaku2ASS 残缺标签：')' 后带噪声也应解析出 move
        let t = parse_tag_block(r"move(2640,120,-720,120)&H1BFBFF");
        let mv = t.move_.unwrap();
        assert_eq!((mv.from, mv.to), ((2640.0, 120.0), (-720.0, 120.0)));
    }

    #[test]
    fn test_apply_overlay_last_wins() {
        let mut acc = AssTags::default();
        acc.apply_overlay(&parse_tag_block(r"an5\fs20\c&H00FF00&"));
        acc.apply_overlay(&parse_tag_block(r"an7"));
        assert_eq!(acc.alignment, Some(Alignment(7)));
        assert_eq!(acc.font_size, Some(20.0));
        assert_eq!(acc.color, Some([0.0, 1.0, 0.0, 1.0]));
    }

    #[test]
    fn test_split_override_segments() {
        let segs = split_override_segments(r"{\an8\pos(10,20)}hello{\c&HFF0000&} world");
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].1, "hello");
        assert_eq!(segs[0].0.alignment, Some(Alignment(8)));
        assert_eq!(segs[1].1, " world");
        // &HFF0000 = 6 位 BGR → blue
        assert_eq!(segs[1].0.color, Some([0.0, 0.0, 1.0, 1.0]));

        let segs = split_override_segments("no tags here");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].1, "no tags here");
        assert_eq!(segs[0].0, AssTags::default());

        // 未闭合大括号 → 不当作标签块
        let segs = split_override_segments(r"{\an8 broken");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].1.contains("an8"));
    }

    #[test]
    fn test_eval_move_interpolation() {
        let mv = AssMove {
            from: (0.0, 100.0),
            to: (400.0, 100.0),
            t1_ms: 0.0,
            t2_ms: f64::NAN,
        };
        // 事件 10s..20s：中点 → 插值一半
        let (x, y) = eval_move(&mv, 15.0, 10.0, 20.0);
        assert!((x - 200.0).abs() < 0.01);
        assert!((y - 100.0).abs() < 0.01);
        // 前段之前 → 起点
        assert_eq!(eval_move(&mv, 5.0, 10.0, 20.0), (0.0, 100.0));
        // 之后 → 终点
        assert_eq!(eval_move(&mv, 30.0, 10.0, 20.0), (400.0, 100.0));

        // 显式时间段 2000..4000ms
        let mv2 = AssMove {
            from: (0.0, 0.0),
            to: (100.0, 0.0),
            t1_ms: 2000.0,
            t2_ms: 4000.0,
        };
        assert_eq!(eval_move(&mv2, 11.0, 10.0, 20.0), (0.0, 0.0));
        let (x, _) = eval_move(&mv2, 13.0, 10.0, 20.0);
        assert!((x - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_eval_fade() {
        // 500ms 淡入 / 1000ms 淡出，事件 10..20s
        let f = (500.0, 1000.0);
        assert_eq!(eval_fade(f, 10.0, 10.0, 20.0), 0.0);
        let a = eval_fade(f, 10.25, 10.0, 20.0);
        assert!((a - 0.5).abs() < 0.01);
        assert_eq!(eval_fade(f, 11.0, 10.0, 20.0), 1.0);
        let a = eval_fade(f, 19.5, 10.0, 20.0);
        assert!((a - 0.5).abs() < 0.01);
        assert_eq!(eval_fade(f, 20.0, 10.0, 20.0), 0.0);
    }
}
