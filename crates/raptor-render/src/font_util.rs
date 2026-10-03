//! 字体数据工具 — 字幕与弹幕引擎共用的纯字节操作

/// 从 TTC (TrueType Collection) 文件中提取单个字体为独立的 TTF 数据
///
/// TTC 文件中每个字体的 offset table 使用绝对文件偏移，
/// 直接切片后 `FontRef::try_from_slice` 无法正确解析。
/// 此函数重写 offset table，将所有表偏移调整为相对于字体数据起始位置。
pub fn extract_font_from_ttc(data: &[u8], font_offset: usize) -> Option<Vec<u8>> {
    if data.len() < font_offset + 12 {
        return None;
    }

    // 读取 offset table: sfVersion(4) + numTables(2) + searchRange(2) + entrySelector(2) + rangeShift(2)
    let num_tables =
        u16::from_be_bytes(data[font_offset + 4..font_offset + 6].try_into().ok()?) as usize;
    let header_size = 12 + num_tables * 16; // offset table header + table records
    if data.len() < font_offset + header_size {
        return None;
    }

    // 计算所有表的绝对偏移范围，确定字体数据边界
    let mut abs_min = font_offset;
    let mut abs_max = font_offset + header_size;

    for i in 0..num_tables {
        let rec = font_offset + 12 + i * 16;
        let offset = u32::from_be_bytes(data[rec + 8..rec + 12].try_into().ok()?) as usize;
        let length = u32::from_be_bytes(data[rec + 12..rec + 16].try_into().ok()?) as usize;
        abs_min = abs_min.min(offset);
        abs_max = abs_max.max(offset + length);
    }

    // 提取完整字体数据
    let base = abs_min;
    let end = abs_max.min(data.len());
    let mut result = data[base..end].to_vec();

    // 验证 result 包含 offset table
    if font_offset < base || font_offset + header_size > end {
        return None;
    }
    let local_header = font_offset - base;

    // 重写每个表的 offset：绝对偏移 - base
    for i in 0..num_tables {
        let rec = local_header + 12 + i * 16;
        if rec + 16 > result.len() {
            return None;
        }
        let abs_offset = u32::from_be_bytes(result[rec + 8..rec + 12].try_into().ok()?);
        let new_offset = abs_offset.wrapping_sub(base as u32);
        result[rec + 8..rec + 12].copy_from_slice(&new_offset.to_be_bytes());
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_font_from_ttc_synthetic() {
        // 构建最小合成 TTC 文件：
        // - TTC header: "ttcf" + version + numFonts=1 + offset to font 0
        // - Font offset table: sfVersion + numTables=1 + header fields
        // - 1 个 table record: tag + checksum + offset + length
        // - table 数据

        let mut ttc = Vec::new();

        // TTC header (12 + 4 = 16 bytes)
        ttc.extend_from_slice(b"ttcf"); // tag
        ttc.extend_from_slice(&[0, 1, 0, 0]); // version 1.0
        ttc.extend_from_slice(&[0, 0, 0, 1]); // numFonts = 1
        ttc.extend_from_slice(&[0, 0, 0, 16]); // offset[0] = 16 (font starts right after TTC header)

        // Font offset table at byte 16 (12 bytes header + 16 bytes per table)
        let font_start = 16usize;
        let num_tables = 1u16;
        ttc.extend_from_slice(&[0, 1, 0, 0]); // sfVersion (TrueType)
        ttc.extend_from_slice(&num_tables.to_be_bytes()); // numTables
        ttc.extend_from_slice(&[0, 0]); // searchRange
        ttc.extend_from_slice(&[0, 0]); // entrySelector
        ttc.extend_from_slice(&[0, 0]); // rangeShift

        // Table record: "head" + checksum + offset + length
        let table_data_offset = font_start + 12 + 16; // 12 (offset table) + 16 (1 record)
        let table_data_len = 8u32;
        ttc.extend_from_slice(b"head"); // tag
        ttc.extend_from_slice(&[0, 0, 0, 0]); // checksum
        ttc.extend_from_slice(&(table_data_offset as u32).to_be_bytes()); // absolute offset
        ttc.extend_from_slice(&table_data_len.to_be_bytes()); // length

        // Table data
        ttc.extend_from_slice(&[0u8; 8]);

        // 执行提取
        let extracted = extract_font_from_ttc(&ttc, font_start).expect("extraction should succeed");

        // 验证：提取后的数据应从 table_data_offset 开始
        // offset table 中的表偏移应被重写为相对于 base 的值
        // base = font_start = 16
        // 原始绝对偏移 = table_data_offset = 44
        // 重写后 = 44 - 16 = 28
        let rec_start = 12; // offset table 内 table record 起始
        let rewritten_offset =
            u32::from_be_bytes(extracted[rec_start + 8..rec_start + 12].try_into().unwrap());
        assert_eq!(rewritten_offset, (table_data_offset - font_start) as u32);

        assert!(!extracted.is_empty());
    }

    #[test]
    fn test_extract_font_from_ttc_invalid() {
        // 太短的数据
        assert!(extract_font_from_ttc(&[0u8; 5], 0).is_none());
        // 偏移超出范围
        assert!(extract_font_from_ttc(&[0u8; 20], 100).is_none());
    }
}
