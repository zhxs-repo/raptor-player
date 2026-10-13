//! 弹幕布局引擎 — 基于 DFM+ 的轨道碰撞算法
//!
//! 核心策略（移植自 Erika DFM+）：
//! - 四种弹幕类型独立轨道系统 (ScrollRL, ScrollLR, FixTop, FixBottom)
//! - DFM 原版双时间点碰撞检测（替代单时间点边缘检测）
//! - overwriteInsert 策略（轨道满时选择右边缘最左的轨道清除并放入新弹幕）
//! - 自适应窗口尺寸（布局跟随窗口大小变化，viewport 变化时可 relayout）
//! - FilterSystem 过滤链：类型屏蔽 / 数量密度 / 关键词 / 去重合并
//! - max_lines 行数限制

use crate::dfm_core::filters::{FilterContext, FilterSystem};
use crate::dfm_core::model::{
    DanmakuItem as DfmItem, DanmakuType as DfmType, Duration, GlobalFlags,
};
use crate::dfm_core::retainer::DanmakuRetainer;
use crate::types::{DanmakuConfig, DanmakuInstance, DanmakuItem, DanmakuMode};

// ─── 外部类型 ↔ DFM 内部类型转换 ────────────────────────────

fn mode_to_dfm_type(mode: DanmakuMode) -> DfmType {
    match mode {
        DanmakuMode::ScrollRight => DfmType::ScrollRL,
        DanmakuMode::ScrollLeft => DfmType::ScrollLR,
        DanmakuMode::TopFixed => DfmType::FixTop,
        DanmakuMode::BottomFixed => DfmType::FixBottom,
        DanmakuMode::Advanced => DfmType::ScrollRL, // fallback, will be skipped
    }
}

/// 弹幕文件的字号是按 1080p 窗口录的绝对像素；按**窗口高度**换算，才能在任意
/// 分辨率 / 屏幕方向下铺满播放器窗口（B 站的观感：字号随播放器大小变化）。
///
/// 基准刻意用窗口而不是视频内容：弹幕铺满整个播放器（含黑边），若按视频原生
/// 分辨率算，竖屏手机播 16:9 视频时字号会小到几乎看不见。
pub const REFERENCE_VIEWPORT_HEIGHT: f32 = 1080.0;

/// 字号缩放系数下限（超小窗口 / 极低分辨率素材的兜底）
const FONT_SCALE_MIN: f32 = 0.4;
/// 字号缩放系数上限（8K 素材的兜底，避免轨道数爆炸）
const FONT_SCALE_MAX: f32 = 4.0;

/// 默认字号放大系数
///
/// XML/ASS 弹幕文件里的字号是按"满屏 1080p"录的，实际播放器窗口通常只有全屏的
/// 一半不到，按窗口高度换算后正文偏小、远距离可读性差。B 站客户端的默认字号也比
/// 素材原始值大一档，这里统一乘一个系数当作观感基线。
const FONT_SIZE_BOOST: f32 = 1.25;

/// 由**窗口高度**算出的字号缩放系数
pub fn font_scale_for(window_height: f32) -> f32 {
    if window_height <= 0.0 {
        return FONT_SIZE_BOOST;
    }
    (window_height / REFERENCE_VIEWPORT_HEIGHT * FONT_SIZE_BOOST)
        .clamp(FONT_SCALE_MIN, FONT_SCALE_MAX)
}

fn item_to_dfm(
    item: &DanmakuItem,
    index: u32,
    scroll_dur_ms: i64,
    fixed_dur_ms: i64,
    font_scale: f32,
) -> DfmItem {
    let dfm_type = mode_to_dfm_type(item.mode);
    let duration_ms = match item.mode {
        DanmakuMode::ScrollRight | DanmakuMode::ScrollLeft => scroll_dur_ms,
        DanmakuMode::TopFixed | DanmakuMode::BottomFixed => fixed_dur_ms,
        DanmakuMode::Advanced => scroll_dur_ms,
    };
    let mut dfm = DfmItem::new(
        item.time_ms as i64,
        item.text.clone(),
        item.color,
        item.font_size as f32 * font_scale,
        dfm_type,
        duration_ms,
    );
    dfm.index = index;
    dfm
}

// ─── 布局引擎 ───────────────────────────────────────────────

/// 布局引擎 — 将弹幕列表转换为带坐标的实例列表
pub struct LayoutEngine {
    config: DanmakuConfig,
}

impl LayoutEngine {
    pub fn new(config: DanmakuConfig) -> Self {
        Self { config }
    }

    /// 估算文本宽度（像素）
    ///
    /// 使用 DFM+ 的估算公式：CJK=1.0em, ASCII=0.55em, whitespace=0.35em, *1.15 系数
    pub fn estimate_text_width(text: &str, font_size: u32) -> f32 {
        crate::dfm_core::model::measure_text_width(text, font_size as f32) * 1.15
    }

    /// 对弹幕列表进行布局，生成实例列表
    ///
    /// 使用 DFM+ DanmakuRetainer 进行轨道碰撞检测，支持：
    /// - 双时间点碰撞检测
    /// - overwriteInsert 策略
    /// - 四种弹幕类型独立轨道
    /// - FilterSystem 过滤链
    /// - max_lines 行数限制
    pub fn layout(&self, items: &[DanmakuItem]) -> Vec<DanmakuInstance> {
        let view_w = self.config.viewport_width;
        let view_h = self.config.viewport_height;
        let scroll_dur_ms = (self.config.scroll_duration * 1000.0) as i64;
        let fixed_dur_ms = (self.config.fixed_duration * 1000.0) as i64;
        let display_area = self.config.display_area.clamp(0.1, 1.0);
        let track_gap_ratio = self.config.track_gap_ratio.clamp(0.0, 2.0);
        let outline_multiplier = self.config.outline_multiplier.clamp(0.0, 4.0);

        let flags = GlobalFlags::default();
        // 布局和光栅化共用这一个系数，否则占位宽度与实绘宽度不一致，
        // 轨道碰撞检测会算错（弹幕重叠 / 留异常空隙）
        let font_scale = self.config.font_scale;

        // ── 1. 构建 DFM 内部 item 列表 ──
        let mut dfm_items: Vec<(u32, DfmItem)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                if matches!(item.mode, DanmakuMode::Advanced) {
                    return None;
                }
                let mut dfm = item_to_dfm(item, i as u32, scroll_dur_ms, fixed_dur_ms, font_scale);
                let outline_px =
                    resolve_outline_px(item.font_size as f32 * font_scale, outline_multiplier);
                dfm.measure_with_outline(view_w, view_h, &flags, outline_px);
                Some((i as u32, dfm))
            })
            .collect();

        // ── 2. 设置 FilterSystem ──
        let mut filter_sys = FilterSystem::default();
        if let Some(q) = self.config.max_quantity {
            filter_sys.max_quantity = Some(q);
        }
        if let Some(max) = self.config.max_lines {
            for ty in [
                DfmType::ScrollRL,
                DfmType::ScrollLR,
                DfmType::FixTop,
                DfmType::FixBottom,
            ] {
                filter_sys.max_lines.insert(ty, max);
            }
        }
        for mode in &self.config.blocked_types {
            filter_sys.blocked_types.insert(mode_to_dfm_type(*mode));
        }
        filter_sys.duplicate_merge = self.config.merge_duplicates;
        filter_sys.set_block_words(&self.config.block_words);

        // ── 3. 运行主过滤器 ──
        let scroll_duration = Duration::new(scroll_dur_ms);
        let mut ctx = FilterContext {
            timer_ms: 0,
            index_in_screen: 0,
            screen_size: dfm_items.len(),
            frame_elapsed_ms: 0,
            global_flags: flags,
            scroll_duration,
        };
        for (i, (_, item)) in dfm_items.iter_mut().enumerate() {
            if item.is_filtered {
                continue;
            }
            ctx.index_in_screen = i;
            filter_sys.filter_primary(item, &ctx);
        }

        // ── 4. 轨道碰撞避让 ──
        let mut retainer = DanmakuRetainer::new(2.0, track_gap_ratio);

        let mut instances: Vec<(usize, DanmakuInstance)> = Vec::with_capacity(dfm_items.len());
        // overwriteInsert 挤掉的旧弹幕索引（原始 item 索引），最终从实例列表中移除
        let mut displaced_set: std::collections::HashSet<usize> = std::collections::HashSet::new();

        for (orig_idx, dfm) in &mut dfm_items {
            if dfm.is_filtered {
                continue;
            }

            let (placed, displaced) = retainer.fix_with_options(
                dfm,
                view_w,
                view_h,
                &flags,
                display_area,
                false,
                self.config.allow_stacking,
                self.config.allow_scroll_overwrite,
            );

            if !placed {
                tracing::debug!(
                    "layout: item {} dropped (all tracks full), type={:?}",
                    orig_idx,
                    dfm.danmaku_type
                );
                continue;
            }

            // max_lines 检查
            if let Some(max) = self.config.max_lines {
                if max > 0 {
                    let track_height =
                        (dfm.paint_height + dfm.paint_height * track_gap_ratio).max(1.0);
                    let row = if dfm.danmaku_type == DfmType::FixBottom {
                        let effective_height = view_h * display_area;
                        ((effective_height - dfm.y) / track_height).ceil().max(1.0) as u32 - 1
                    } else {
                        ((dfm.y - 2.0) / track_height).floor().max(0.0) as u32
                    };
                    if row >= max {
                        continue;
                    }
                }
            }

            // 收集被 overwriteInsert 挤掉的弹幕索引，循环后统一回写移除
            displaced_set.extend(displaced);

            // 从原始索引取 DanmakuItem
            let orig_item = &items[*orig_idx as usize];
            let start_time = orig_item.time_ms as f64 / 1000.0;
            let text_width = dfm.paint_width;

            let instance = match orig_item.mode {
                DanmakuMode::ScrollRight => {
                    let end_time = start_time + self.config.scroll_duration;
                    DanmakuInstance {
                        item: orig_item.clone(),
                        track: (dfm.y / dfm.paint_height).floor() as i32,
                        start_time,
                        end_time,
                        start_x: view_w,
                        end_x: -text_width,
                        y: dfm.y,
                        text_width,
                    }
                }
                DanmakuMode::ScrollLeft => {
                    let end_time = start_time + self.config.scroll_duration;
                    DanmakuInstance {
                        item: orig_item.clone(),
                        track: (dfm.y / dfm.paint_height).floor() as i32,
                        start_time,
                        end_time,
                        start_x: -text_width,
                        end_x: view_w,
                        y: dfm.y,
                        text_width,
                    }
                }
                DanmakuMode::TopFixed => {
                    let end_time = start_time + self.config.fixed_duration;
                    let center_x = (view_w - text_width) / 2.0;
                    DanmakuInstance {
                        item: orig_item.clone(),
                        track: (dfm.y / dfm.paint_height).floor() as i32,
                        start_time,
                        end_time,
                        start_x: center_x,
                        end_x: center_x,
                        y: dfm.y,
                        text_width,
                    }
                }
                DanmakuMode::BottomFixed => {
                    let end_time = start_time + self.config.fixed_duration;
                    let center_x = (view_w - text_width) / 2.0;
                    DanmakuInstance {
                        item: orig_item.clone(),
                        track: (dfm.y / dfm.paint_height).floor() as i32,
                        start_time,
                        end_time,
                        start_x: center_x,
                        end_x: center_x,
                        y: dfm.y,
                        text_width,
                    }
                }
                DanmakuMode::Advanced => unreachable!(),
            };

            instances.push((*orig_idx as usize, instance));
        }

        // displaced 回写：被后来弹幕 overwriteInsert 挤掉的旧弹幕不再上屏，
        // 否则会出现同轨两条弹幕互相重叠
        let placed_total = instances.len();
        let instances: Vec<DanmakuInstance> = instances
            .into_iter()
            .filter(|(orig_idx, _)| !displaced_set.contains(orig_idx))
            .map(|(_, inst)| inst)
            .collect();
        let displaced_removed = placed_total - instances.len();
        if displaced_removed > 0 {
            tracing::debug!(
                "layout: {} items displaced (removed) by overwriteInsert",
                displaced_removed
            );
        }

        tracing::info!(
            "layout: {} items -> {} instances (displaced={}, viewport={}x{}, display_area={:.1}, filters={})",
            items.len(),
            instances.len(),
            displaced_removed,
            view_w,
            view_h,
            display_area,
            !self.config.block_words.is_empty() || self.config.max_quantity.is_some()
        );

        instances
    }

    /// 获取给定时间点的活跃弹幕
    pub fn active_at(instances: &[DanmakuInstance], time_secs: f64) -> Vec<&DanmakuInstance> {
        instances
            .iter()
            .filter(|inst| time_secs >= inst.start_time && time_secs < inst.end_time)
            .collect()
    }

    /// 计算滚动弹幕在当前时间的 x 坐标
    pub fn current_x(instance: &DanmakuInstance, time_secs: f64) -> f32 {
        let elapsed = time_secs - instance.start_time;
        let duration = instance.end_time - instance.start_time;
        if duration <= 0.0 {
            return instance.start_x;
        }
        let progress = (elapsed / duration) as f32;
        instance.start_x + (instance.end_x - instance.start_x) * progress
    }

    /// 从已布局的实例列表构建预处理布局（两阶段架构）
    pub fn prepare(&self, instances: &[DanmakuInstance]) -> PreparedLayout {
        PreparedLayout::from_instances(instances, &self.config)
    }

    /// 使用新 viewport 尺寸重新布局（自适应窗口）
    ///
    /// 当窗口大小变化时调用，使用原始弹幕数据重新执行 layout + prepare。
    pub fn relayout(
        items: &[DanmakuItem],
        new_width: f32,
        new_height: f32,
        config: &DanmakuConfig,
    ) -> (Vec<DanmakuInstance>, PreparedLayout) {
        let new_config = DanmakuConfig {
            viewport_width: new_width,
            viewport_height: new_height,
            // font_scale 由调用方按**视频原生分辨率**给出（见 font_scale_for）：
            // 字号已经与视口解耦，这里再按 new_height 重算等于把字号绑回窗口尺寸。
            font_scale: config.font_scale,
            ..config.clone()
        };
        let engine = LayoutEngine::new(new_config);
        let instances = engine.layout(items);
        let prepared = engine.prepare(&instances);
        (instances, prepared)
    }
}

// ─── Prepare / Frame-Query 两阶段架构 ───────────────────────────────

/// 预处理后的布局项 — 从 DanmakuInstance 提取渲染所需信息
#[derive(Debug, Clone)]
pub struct PreparedItem {
    /// 原始 DanmakuInstance 在列表中的索引
    pub index: usize,
    /// 出现时间（秒）
    pub start_time: f64,
    /// 消失时间（秒）
    pub end_time: f64,
    /// 起始 x 坐标
    pub start_x: f32,
    /// 终止 x 坐标（滚动弹幕有效）
    pub end_x: f32,
    /// y 坐标
    pub y: f32,
    /// 估算文本宽度（像素）
    pub text_width: f32,
    /// 字体大小
    pub font_size: u32,
    /// 颜色 (0xRRGGBB)
    pub color: u32,
    /// 是否为滚动弹幕
    pub is_scroll: bool,
    /// 弹幕文本
    pub text: String,
}

/// 预处理后的布局 — 只构建一次，支持多次 frame query
#[derive(Debug, Clone)]
pub struct PreparedLayout {
    /// 按时间排序的布局项
    pub items: Vec<PreparedItem>,
    /// start_time 数组（供二分查找）
    pub item_times: Vec<f64>,
    /// 滚动弹幕持续时间（秒）
    pub scroll_duration: f64,
    /// 固定弹幕持续时间（秒）
    pub static_duration: f64,
    /// 布局时使用的 viewport 尺寸
    pub viewport_width: f32,
    pub viewport_height: f32,
}

/// 单帧查询结果
#[derive(Debug, Clone, Default)]
pub struct FrameLayout {
    /// 当前帧可见的弹幕
    pub items: Vec<FrameItem>,
}

/// 单帧中一条弹幕的屏幕位置
#[derive(Debug, Clone, Copy)]
pub struct FrameItem {
    /// 对应 PreparedLayout.items 中的索引
    pub item_index: usize,
    /// 当前帧的 x 坐标（像素）
    pub x: f32,
    /// y 坐标（像素）
    pub y: f32,
}

impl PreparedLayout {
    /// 创建空的预处理布局
    pub fn empty(scroll_duration: f64, static_duration: f64) -> Self {
        Self {
            items: Vec::new(),
            item_times: Vec::new(),
            scroll_duration,
            static_duration,
            viewport_width: 0.0,
            viewport_height: 0.0,
        }
    }

    /// 从 DanmakuInstance 列表构建预处理布局
    pub fn from_instances(instances: &[DanmakuInstance], config: &DanmakuConfig) -> Self {
        let mut items: Vec<PreparedItem> = instances
            .iter()
            .enumerate()
            .map(|(i, inst)| {
                let is_scroll = matches!(
                    inst.item.mode,
                    DanmakuMode::ScrollRight | DanmakuMode::ScrollLeft
                );
                PreparedItem {
                    index: i,
                    start_time: inst.start_time,
                    end_time: inst.end_time,
                    start_x: inst.start_x,
                    end_x: inst.end_x,
                    y: inst.y,
                    text_width: inst.text_width,
                    // 必须存**缩放后**的字号：光栅化直接拿这个值来选字形尺寸。
                    // 存原始值会让"占位宽度按缩放算、实际字形按原始像素画"，
                    // 两者不一致 —— 这正是文字看起来被压窄的直接原因。
                    font_size: (inst.item.font_size as f32 * config.font_scale).round() as u32,
                    color: inst.item.color,
                    is_scroll,
                    text: inst.item.text.clone(),
                }
            })
            .collect();

        // 按 start_time 排序
        items.sort_by(|a, b| {
            a.start_time
                .partial_cmp(&b.start_time)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let item_times: Vec<f64> = items.iter().map(|item| item.start_time).collect();

        tracing::info!(
            "PreparedLayout: {} items prepared, scroll_dur={:.1}s static_dur={:.1}s viewport={}x{}",
            items.len(),
            config.scroll_duration,
            config.fixed_duration,
            config.viewport_width,
            config.viewport_height,
        );

        Self {
            items,
            item_times,
            scroll_duration: config.scroll_duration,
            static_duration: config.fixed_duration,
            viewport_width: config.viewport_width,
            viewport_height: config.viewport_height,
        }
    }

    /// 二分查找：查询当前时间可见的弹幕 — O(log n + k)
    pub fn query_frame(&self, current_time: f64) -> FrameLayout {
        if self.items.is_empty() {
            return FrameLayout::default();
        }

        let max_dur = self.scroll_duration.max(self.static_duration);
        let window_start = current_time - max_dur;

        // 二分查找：第一个 start_time >= window_start 的索引
        let start_idx = lower_bound(&self.item_times, window_start);
        // 二分查找：第一个 start_time > current_time 的索引
        let end_idx = upper_bound(&self.item_times, current_time);

        let mut frame_items = Vec::with_capacity(end_idx.saturating_sub(start_idx));

        for i in start_idx..end_idx {
            let item = &self.items[i];
            let elapsed = current_time - item.start_time;

            // 尚未开始
            if elapsed < 0.0 {
                continue;
            }

            // 固定弹幕已过期
            if !item.is_scroll && elapsed > (item.end_time - item.start_time) {
                continue;
            }

            // 滚动弹幕已离开屏幕
            if item.is_scroll && item.x_at(current_time) < -item.text_width {
                continue;
            }

            frame_items.push(FrameItem {
                item_index: i,
                x: item.x_at(current_time),
                y: item.y,
            });
        }

        FrameLayout { items: frame_items }
    }
}

impl PreparedItem {
    /// 计算当前时间的 x 坐标
    ///
    /// 线性插值等价于 DFM 的 get_r2l_x / get_l2r_x 公式：
    /// - ScrollRL: x = viewport_width - elapsed * step_x
    /// - ScrollLR: x = elapsed * step_x - paint_width
    /// - Fixed: x = center (恒定)
    pub fn x_at(&self, current_time: f64) -> f32 {
        let elapsed = current_time - self.start_time;
        let duration = self.end_time - self.start_time;
        if duration <= 0.0 {
            return self.start_x;
        }
        let progress = (elapsed / duration) as f32;
        self.start_x + (self.end_x - self.start_x) * progress
    }
}

/// Compute outline width in pixels based on font size and multiplier.
fn resolve_outline_px(font_size: f32, outline_multiplier: f32) -> f32 {
    if outline_multiplier <= 0.0 || !outline_multiplier.is_finite() {
        return 0.0;
    }
    (font_size * 0.06).clamp(1.0, 2.6) * outline_multiplier
}

/// 二分查找：第一个 >= target 的索引
fn lower_bound(times: &[f64], target: f64) -> usize {
    times.partition_point(|&t| t < target)
}

/// 二分查找：第一个 > target 的索引
fn upper_bound(times: &[f64], target: f64) -> usize {
    times.partition_point(|&t| t <= target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DanmakuMode;

    fn make_item(time_ms: u64, mode: DanmakuMode, text: &str) -> DanmakuItem {
        DanmakuItem {
            time_ms,
            mode,
            font_size: 25,
            color: 0xFFFFFF,
            text: text.to_string(),
        }
    }

    #[test]
    fn test_estimate_text_width_ascii() {
        let w = LayoutEngine::estimate_text_width("hello", 25);
        // DFM+ formula: 5 ASCII * 0.55 * 25 * 1.15 ≈ 79.1
        assert!(w > 60.0 && w < 100.0, "w={}", w);
    }

    #[test]
    fn test_estimate_text_width_cjk() {
        let w = LayoutEngine::estimate_text_width("你好", 25);
        // DFM+ formula: 2 CJK * 1.0 * 25 * 1.15 = 57.5
        assert!(w > 45.0 && w < 70.0, "w={}", w);
    }

    #[test]
    fn test_estimate_text_width_mixed() {
        let w = LayoutEngine::estimate_text_width("hi你", 25);
        // DFM+: (2*0.55 + 1*1.0) * 25 * 1.15 = 2.1 * 25 * 1.15 = 60.4
        assert!(w > 50.0 && w < 75.0, "w={}", w);
    }

    #[test]
    fn test_layout_scroll_right() {
        let config = DanmakuConfig::default();
        let engine = LayoutEngine::new(config);
        let items = vec![make_item(0, DanmakuMode::ScrollRight, "test")];
        let instances = engine.layout(&items);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].start_x, 1920.0);
        assert!(instances[0].end_x < 0.0);
        assert_eq!(instances[0].start_time, 0.0);
        assert_eq!(instances[0].end_time, 8.0);
    }

    #[test]
    fn test_layout_top_fixed() {
        let config = DanmakuConfig::default();
        let engine = LayoutEngine::new(config);
        let items = vec![make_item(1000, DanmakuMode::TopFixed, "fixed")];
        let instances = engine.layout(&items);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].start_time, 1.0);
        assert_eq!(instances[0].end_time, 5.0); // 1.0 + 4.0
        assert_eq!(instances[0].start_x, instances[0].end_x); // 固定弹幕不移动
    }

    #[test]
    fn test_layout_track_allocation() {
        let config = DanmakuConfig {
            viewport_width: 100.0,
            viewport_height: 200.0,
            track_height: 25.0,
            scroll_duration: 2.0,
            ..Default::default()
        };
        let engine = LayoutEngine::new(config);
        // 同时发出多条弹幕，应分配到不同轨道
        let items = vec![
            make_item(0, DanmakuMode::ScrollRight, "a"),
            make_item(100, DanmakuMode::ScrollRight, "b"),
            make_item(200, DanmakuMode::ScrollRight, "c"),
        ];
        let instances = engine.layout(&items);
        assert_eq!(instances.len(), 3);
        // DFM+ retainer 应为前几条同时弹幕分配不同 y 坐标
        assert_ne!(instances[0].y, instances[1].y);
    }

    #[test]
    fn test_active_at() {
        let config = DanmakuConfig::default();
        let engine = LayoutEngine::new(config);
        let items = vec![
            make_item(0, DanmakuMode::ScrollRight, "early"),
            make_item(5000, DanmakuMode::ScrollRight, "mid"),
            make_item(10000, DanmakuMode::ScrollRight, "late"),
        ];
        let instances = engine.layout(&items);

        // scroll_duration = 8.0s, so:
        // "early": [0.0, 8.0), "mid": [5.0, 13.0), "late": [10.0, 18.0)

        // 只有 "early" 在时间 2.0 活跃
        let active = LayoutEngine::active_at(&instances, 2.0);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].item.text, "early");

        // 时间 6.0: "early" (0~8) 和 "mid" (5~13) 都活跃
        let active = LayoutEngine::active_at(&instances, 6.0);
        assert_eq!(active.len(), 2);

        // 时间 9.0: 只有 "mid" (5~13) 活跃
        let active = LayoutEngine::active_at(&instances, 9.0);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].item.text, "mid");

        // 时间 20.0: 无活跃弹幕
        let active = LayoutEngine::active_at(&instances, 20.0);
        assert!(active.is_empty());
    }

    #[test]
    fn test_current_x_scroll() {
        let instance = DanmakuInstance {
            item: make_item(0, DanmakuMode::ScrollRight, "test"),
            track: 0,
            start_time: 0.0,
            end_time: 8.0,
            start_x: 1920.0,
            end_x: -100.0,
            y: 0.0,
            text_width: 100.0,
        };
        // 在起始时间，x 应该等于 start_x
        let x = LayoutEngine::current_x(&instance, 0.0);
        assert!((x - 1920.0).abs() < 1.0);
        // 在中间时间，x 应该在中间
        let x = LayoutEngine::current_x(&instance, 4.0);
        assert!(x > -100.0 && x < 1920.0);
    }

    // ─── Prepare / Frame-Query 测试 ───────────────────────────────

    #[test]
    fn test_prepared_layout_from_instances() {
        let config = DanmakuConfig::default();
        let engine = LayoutEngine::new(config.clone());
        let items = vec![
            make_item(0, DanmakuMode::ScrollRight, "first"),
            make_item(5000, DanmakuMode::ScrollRight, "second"),
            make_item(10000, DanmakuMode::TopFixed, "third"),
        ];
        let instances = engine.layout(&items);
        let prepared = engine.prepare(&instances);

        assert_eq!(prepared.items.len(), 3);
        // 应该按时间排序
        assert!(prepared.item_times.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(prepared.scroll_duration, config.scroll_duration);
        assert_eq!(prepared.static_duration, config.fixed_duration);
    }

    #[test]
    fn test_prepared_layout_query_frame() {
        let config = DanmakuConfig::default();
        let engine = LayoutEngine::new(config);
        let items = vec![
            make_item(0, DanmakuMode::ScrollRight, "early"),
            make_item(5000, DanmakuMode::ScrollRight, "mid"),
            make_item(10000, DanmakuMode::ScrollRight, "late"),
        ];
        let instances = engine.layout(&items);
        let prepared = engine.prepare(&instances);

        // 时间 2.0: 只有 "early" (0~8s)
        let frame = prepared.query_frame(2.0);
        assert_eq!(frame.items.len(), 1);

        // 时间 6.0: "early" (0~8) 和 "mid" (5~13)
        let frame = prepared.query_frame(6.0);
        assert_eq!(frame.items.len(), 2);

        // 时间 9.0: 只有 "mid" (5~13)
        let frame = prepared.query_frame(9.0);
        assert_eq!(frame.items.len(), 1);

        // 时间 20.0: 无活跃弹幕
        let frame = prepared.query_frame(20.0);
        assert!(frame.items.is_empty());
    }

    #[test]
    fn test_prepared_layout_empty() {
        let prepared = PreparedLayout::empty(8.0, 4.0);
        let frame = prepared.query_frame(5.0);
        assert!(frame.items.is_empty());
    }

    /// 弹幕字号随**窗口高度**缩放，且布局与光栅化用同一个系数
    ///
    /// 弹幕铺满整个播放器窗口（含黑边），所以字号按窗口高度走 —— 与字幕不同，
    /// 字幕是按视频原生分辨率（内容基准）。这条曾经被改成内容基准，真机反馈是
    /// 「字偏小 + 竖屏下轨道数骤减导致弹幕中途消失」。
    ///
    /// 另一条独立约束：`PreparedItem.font_size` 必须存**缩放后**的值。存原始值会让
    /// 「占位宽度按缩放算、实际字形按原始像素画」不一致，轨道碰撞检测因此算错，
    /// 表现为弹幕被压窄。
    #[test]
    fn font_size_scales_with_window_height_and_is_shared_by_layout_and_raster() {
        let items = vec![make_item(0, DanmakuMode::ScrollRight, "测试弹幕")];

        // 1080p 窗口：scale = 1.25（默认放大系数），字号 = 文件的 25 → 31
        let cfg_hd = DanmakuConfig {
            font_scale: font_scale_for(1080.0),
            ..Default::default()
        };
        let engine_hd = LayoutEngine::new(cfg_hd);
        let prepared_hd = engine_hd.prepare(&engine_hd.layout(&items));
        assert_eq!(
            prepared_hd.items[0].font_size, 31,
            "1080p 应得到放大一档的字号"
        );

        // 半高窗口（手机横屏 / 小窗播放）：scale = 0.625，字号 25 → 16
        let cfg_small = DanmakuConfig {
            viewport_width: 960.0,
            viewport_height: 540.0,
            font_scale: font_scale_for(540.0),
            ..Default::default()
        };
        let engine_small = LayoutEngine::new(cfg_small);
        let prepared_small = engine_small.prepare(&engine_small.layout(&items));
        assert_eq!(
            prepared_small.items[0].font_size, 16,
            "540p window should halve the font size (got {})",
            prepared_small.items[0].font_size
        );

        // 手机竖屏（2340 高）：scale ≈ 2.71，字号显著放大 —— 这是弹幕"铺满窗口"
        // 的预期观感，也是曾经被误改成内容基准后显得偏小的原因
        let cfg_portrait = DanmakuConfig {
            viewport_width: 1080.0,
            viewport_height: 2340.0,
            font_scale: font_scale_for(2340.0),
            ..Default::default()
        };
        let engine_portrait = LayoutEngine::new(cfg_portrait);
        let prepared_portrait = engine_portrait.prepare(&engine_portrait.layout(&items));
        assert!(
            prepared_portrait.items[0].font_size > 31,
            "tall window should enlarge font, got {}",
            prepared_portrait.items[0].font_size
        );
    }

    /// 轨道数必须跟着**窗口**高度走，不能被画面矩形压扁
    ///
    /// 防回归：曾把布局视口从整个窗口换成 letterbox 后的画面矩形。竖屏手机播
    /// 16:9 视频时画面矩形只有 ~608px 高，轨道数从 ~60 掉到 ~16，
    /// overwriteInsert 于是频繁挤掉**正在屏幕上**的老弹幕 —— 表现为「刚出现就
    /// 突然消失」。
    #[test]
    fn track_count_follows_window_height_not_video_rect() {
        // 竖屏 surface 1080x2340，播 16:9 视频 → 画面矩形只有 1080x608
        let area = raptor_render::OverlayArea::new(1080, 2340, 1920, 1080);
        let window_tracks = (area.surface_height as f32 / 37.5).floor() as u32;
        let rect_tracks = (area.rect_height() / 37.5).floor() as u32;

        assert!(
            rect_tracks * 2 < window_tracks,
            "画面矩形轨道数 {} 应远少于窗口轨道数 {}",
            rect_tracks,
            window_tracks
        );

        // 用窗口高度布局时轨道数必须充裕
        let cfg = DanmakuConfig {
            viewport_width: area.surface_width as f32,
            viewport_height: area.surface_height as f32,
            font_scale: font_scale_for(area.surface_height as f32),
            ..Default::default()
        };
        let engine = LayoutEngine::new(cfg);
        let items: Vec<DanmakuItem> = (0..12)
            .map(|i| make_item(0, DanmakuMode::ScrollRight, &format!("弹幕{}", i)))
            .collect();
        let instances = engine.layout(&items);

        // 12 条同时刻弹幕在窗口基准下应全部排得下，且互不同轨
        assert_eq!(
            instances.len(),
            12,
            "窗口轨道充足时不应有弹幕被挤掉，实得 {}",
            instances.len()
        );
        let mut ys: Vec<f32> = instances.iter().map(|i| i.y).collect();
        ys.sort_by(f32::total_cmp);
        assert_eq!(
            ys.windows(2).filter(|w| w[0] == w[1]).count(),
            0,
            "同轨不应重叠：{ys:?}"
        );
    }

    /// relayout 只改位置，字号由调用方给的系数决定
    #[test]
    fn relayout_changes_positions_and_uses_caller_font_scale() {
        let items = vec![make_item(0, DanmakuMode::ScrollRight, "test")];

        let cfg = DanmakuConfig {
            font_scale: font_scale_for(1080.0),
            ..Default::default()
        };

        let engine = LayoutEngine::new(cfg.clone());
        let before = engine.prepare(&engine.layout(&items));

        // 把窗口缩小一半
        let (instances, after) = LayoutEngine::relayout(&items, 960.0, 540.0, &cfg);

        // 调用方给的系数被原样保留（窗口变了不等于字号要变）
        assert_eq!(
            after.items[0].font_size, before.items[0].font_size,
            "relayout 不得自行改写 font_scale"
        );
        // 位置必须跟着新窗口尺寸走
        assert_eq!(instances[0].start_x, 960.0, "滚动起点应等于新的窗口宽度");
    }

    #[test]
    fn font_scale_clamps_and_guards_zero() {
        assert_eq!(
            font_scale_for(0.0),
            FONT_SIZE_BOOST,
            "zero height must not divide by zero"
        );
        assert_eq!(font_scale_for(1080.0), FONT_SIZE_BOOST);
        // 极端值必须被夹住，否则小字变大字、轨道数爆炸
        assert!(font_scale_for(1.0) >= 0.4);
        assert!(font_scale_for(100000.0) <= 4.0);
    }

    #[test]
    fn test_frame_item_x_coordinate() {
        let config = DanmakuConfig::default();
        let engine = LayoutEngine::new(config);
        let items = vec![make_item(0, DanmakuMode::ScrollRight, "test")];
        let instances = engine.layout(&items);
        let prepared = engine.prepare(&instances);

        // 起始时刻 x 应该接近 start_x (1920)
        let frame = prepared.query_frame(0.0);
        assert_eq!(frame.items.len(), 1);
        assert!((frame.items[0].x - 1920.0).abs() < 1.0);

        // 中间时刻 x 应该在中间
        let frame = prepared.query_frame(4.0);
        assert_eq!(frame.items.len(), 1);
        assert!(frame.items[0].x > -200.0 && frame.items[0].x < 1920.0);
    }

    #[test]
    fn test_relayout_viewport_change() {
        let config = DanmakuConfig::default();
        let items = vec![make_item(0, DanmakuMode::ScrollRight, "test")];

        // 初始 layout
        let engine = LayoutEngine::new(config.clone());
        let instances = engine.layout(&items);
        assert_eq!(instances[0].start_x, 1920.0);

        // relayout with new viewport
        let (new_instances, _prepared) = LayoutEngine::relayout(&items, 1280.0, 720.0, &config);
        assert_eq!(new_instances.len(), 1);
        assert_eq!(new_instances[0].start_x, 1280.0);
    }

    #[test]
    fn test_overwrite_insert_on_overflow() {
        let config = DanmakuConfig {
            viewport_width: 1920.0,
            viewport_height: 150.0, // 3 条轨道（track_height = 30 * 1.5 = 45）
            track_height: 29.0,
            scroll_duration: 8.0,
            ..Default::default()
        };
        let engine = LayoutEngine::new(config);
        // 同时发出很多弹幕，超过轨道数
        let items: Vec<DanmakuItem> = (0..10)
            .map(|i| make_item(i * 10, DanmakuMode::ScrollRight, &format!("item{}", i)))
            .collect();
        let instances = engine.layout(&items);
        // DFM+ overwriteInsert 策略：轨道满时后来者挤掉旧弹幕，
        // 幸存数 = 轨道数（每条轨道最后的占位者），而非全部丢弃
        assert!(
            instances.len() >= 2,
            "overwriteInsert should keep latest items, got {}",
            instances.len()
        );
        // displaced 回写后不应有同轨重叠：所有弹幕时间窗几乎相同，y 必须互异
        let mut ys: Vec<f32> = instances.iter().map(|i| i.y).collect();
        ys.sort_by(f32::total_cmp);
        let dup = ys.windows(2).filter(|w| w[0] == w[1]).count();
        assert_eq!(dup, 0, "displaced items must be removed, ys={:?}", ys);
    }

    #[test]
    fn test_displaced_item_removed_from_instances() {
        let config = DanmakuConfig {
            viewport_width: 1920.0,
            viewport_height: 130.0, // 2 条轨道（track_height = 30 * 1.5 = 45）
            track_height: 29.0,
            scroll_duration: 8.0,
            ..Default::default()
        };
        let engine = LayoutEngine::new(config);
        // 3 条同时刻滚动弹幕 > 2 条轨道：第 3 条必然挤掉 1 条已放置的
        let items = vec![
            make_item(0, DanmakuMode::ScrollRight, "aaa"),
            make_item(0, DanmakuMode::ScrollRight, "bbb"),
            make_item(0, DanmakuMode::ScrollRight, "ccc"),
        ];
        let instances = engine.layout(&items);
        assert_eq!(instances.len(), 2, "one item must be displaced out");
        assert_ne!(instances[0].y, instances[1].y);
        // 被挤掉者不得残留：三条文本最多出现两条
        let texts: Vec<&str> = instances.iter().map(|i| i.item.text.as_str()).collect();
        assert_eq!(
            texts.iter().collect::<std::collections::HashSet<_>>().len(),
            2
        );
    }
}
