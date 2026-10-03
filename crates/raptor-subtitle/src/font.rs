//! 系统字体加载工具
//!
//! 从操作系统加载默认字体文件（TTF/OTF），供字幕和弹幕引擎光栅化文本使用。

/// 从系统加载默认字体的字节数据
///
/// 搜索顺序：
/// - Windows: Microsoft YaHei → Segoe UI → Arial
/// - Linux: Noto Sans CJK → DejaVu Sans → Liberation Sans
/// - macOS: PingFang SC → Hiragino Sans GB → Helvetica Neue
///
/// 返回 `None` 表示找不到任何可用字体。
pub fn load_system_font() -> Option<Vec<u8>> {
    let candidates = font_candidates();
    for path in &candidates {
        if let Ok(data) = std::fs::read(path) {
            tracing::info!("font: loaded system font from {}", path);
            return Some(data);
        }
    }
    tracing::warn!(
        "font: no system font found among {} candidates",
        candidates.len()
    );
    None
}

/// 返回候选字体文件路径列表（按优先级排序）
fn font_candidates() -> Vec<String> {
    #[cfg(target_os = "windows")]
    {
        let fonts_dir = std::env::var("WINDIR")
            .map(|w| format!("{w}\\Fonts"))
            .unwrap_or_else(|_| "C:\\Windows\\Fonts".into());
        vec![
            format!("{fonts_dir}\\msyh.ttc"),    // 微软雅黑
            format!("{fonts_dir}\\msyhbd.ttc"),  // 微软雅黑 Bold
            format!("{fonts_dir}\\segoeui.ttf"), // Segoe UI
            format!("{fonts_dir}\\arial.ttf"),   // Arial
            format!("{fonts_dir}\\simsun.ttc"),  // 宋体
            format!("{fonts_dir}\\simhei.ttf"),  // 黑体
        ]
    }

    #[cfg(target_os = "linux")]
    {
        vec![
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into(),
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc".into(),
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc".into(),
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".into(),
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf".into(),
            "/usr/share/fonts/TTF/DejaVuSans.ttf".into(),
        ]
    }

    #[cfg(target_os = "macos")]
    {
        vec![
            "/System/Library/Fonts/PingFang.ttc".into(),
            "/System/Library/Fonts/Hiragino Sans GB.ttc".into(),
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf".into(),
            "/Library/Fonts/Arial.ttf".into(),
            "/System/Library/Fonts/HelveticaNeue.ttc".into(),
        ]
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_system_font() {
        // 在 CI 环境可能没有字体，所以只验证不 panic
        let result = load_system_font();
        if let Some(data) = result {
            assert!(!data.is_empty(), "font data should not be empty");
        }
    }

    #[test]
    fn test_font_candidates_not_empty() {
        let candidates = font_candidates();
        assert!(!candidates.is_empty(), "should have at least one candidate");
    }
}
