//! 弹幕布局引擎 — 参考 DFM 的轨道碰撞算法
//!
//! 核心策略：
//! - 将屏幕划分为水平轨道（track），每条弹幕占据一个轨道
//! - 滚动弹幕：根据速度和文字宽度计算碰撞时间，分配到不冲突的轨道
//! - 固定弹幕：顶部/底部堆叠，超时自动消失
//! - 高级弹幕：指定 (x, y) 坐标（暂不支持轨迹动画）

use crate::types::{DanmakuConfig, DanmakuInstance, DanmakuItem, DanmakuMode};

/// 轨道分配器 — 管理一组轨道的占用状态
struct TrackAllocator {
    /// 每个轨道的"释放时间"（该轨道上的弹幕何时完全离开屏幕）
    release_times: Vec<f64>,
    /// 轨道高度
    track_height: f32,
    /// 最大轨道数
    max_tracks: usize,
}

impl TrackAllocator {
    fn new(track_height: f32, canvas_height: f32) -> Self {
        let max_tracks = (canvas_height / track_height).floor().max(1.0) as usize;
        Self {
            release_times: vec![0.0; max_tracks],
            track_height,
            max_tracks,
        }
    }

    /// 为滚动弹幕分配轨道 — 找到最早释放的轨道
    ///
    /// 碰撞检测：新弹幕进入屏幕时，前一条弹幕必须已经完全离开
    /// 对于右→左滚动弹幕：
    ///   - 弹幕从 x=canvas_width 进入，到 x=-text_width 离开
    ///   - 运动速度 = (canvas_width + text_width) / scroll_duration
    ///   - 前一条弹幕的右边缘在新弹幕到达同一位置时，必须已经离开
    fn allocate_scroll(
        &mut self,
        start_time: f64,
        text_width: f32,
        canvas_width: f32,
        scroll_duration: f64,
    ) -> i32 {
        let speed = (canvas_width + text_width) / scroll_duration as f32;
        // 新弹幕左边缘到达屏幕左边缘（x=0）的时间
        let time_to_left_edge = (canvas_width / speed) as f64;

        for (i, release_time) in self.release_times.iter_mut().enumerate() {
            if *release_time <= start_time + time_to_left_edge {
                // 这条轨道可用
                // 计算这条弹幕完全离开屏幕的时间
                let total_travel = canvas_width + text_width;
                let end_time = start_time + (total_travel / speed) as f64;
                *release_time = end_time;
                return i as i32;
            }
        }

        // 所有轨道都满了，使用最早释放的轨道（强制覆盖）
        let min_idx = self
            .release_times
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        let total_travel = canvas_width + text_width;
        let end_time = start_time + (total_travel / speed) as f64;
        tracing::debug!(
            "layout: track overflow at {:.2}s (track={})",
            start_time,
            min_idx,
        );
        self.release_times[min_idx] = end_time;
        min_idx as i32
    }

    /// 为固定弹幕分配轨道（顶部或底部堆叠）
    fn allocate_fixed(
        &mut self,
        start_time: f64,
        end_time: f64,
        from_bottom: bool,
    ) -> i32 {
        if from_bottom {
            // 从底部往上堆叠
            for i in (0..self.max_tracks).rev() {
                if self.release_times[i] <= start_time {
                    self.release_times[i] = end_time;
                    return i as i32;
                }
            }
            // 强制使用最底部
            let idx = self.max_tracks - 1;
            self.release_times[idx] = end_time;
            idx as i32
        } else {
            // 从顶部往下堆叠
            for (i, release_time) in self.release_times.iter_mut().enumerate() {
                if *release_time <= start_time {
                    *release_time = end_time;
                    return i as i32;
                }
            }
            self.release_times[0] = end_time;
            0
        }
    }

    /// 重置所有轨道
    fn reset(&mut self) {
        for t in &mut self.release_times {
            *t = 0.0;
        }
    }
}

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
    /// 简单估算：CJK 字符约等于 font_size，ASCII 字符约为 font_size * 0.6
    pub fn estimate_text_width(text: &str, font_size: u32) -> f32 {
        let mut width = 0.0f32;
        for ch in text.chars() {
            if ch.is_ascii() {
                width += font_size as f32 * 0.6;
            } else {
                width += font_size as f32;
            }
        }
        width
    }

    /// 对弹幕列表进行布局，生成实例列表
    pub fn layout(&self, items: &[DanmakuItem]) -> Vec<DanmakuInstance> {
        let mut track_alloc = TrackAllocator::new(
            self.config.track_height,
            self.config.canvas_height,
        );

        let mut instances = Vec::with_capacity(items.len());

        for item in items {
            let start_time = item.time_ms as f64 / 1000.0;
            let text_width = Self::estimate_text_width(&item.text, item.font_size);

            let instance = match item.mode {
                DanmakuMode::ScrollRight => {
                    let track = track_alloc.allocate_scroll(
                        start_time,
                        text_width,
                        self.config.canvas_width,
                        self.config.scroll_duration,
                    );
                    let end_time = start_time + self.config.scroll_duration;
                    DanmakuInstance {
                        item: item.clone(),
                        track,
                        start_time,
                        end_time,
                        start_x: self.config.canvas_width,
                        end_x: -text_width,
                        y: track as f32 * self.config.track_height,
                        text_width,
                    }
                }
                DanmakuMode::ScrollLeft => {
                    let track = track_alloc.allocate_scroll(
                        start_time,
                        text_width,
                        self.config.canvas_width,
                        self.config.scroll_duration,
                    );
                    let end_time = start_time + self.config.scroll_duration;
                    DanmakuInstance {
                        item: item.clone(),
                        track,
                        start_time,
                        end_time,
                        start_x: -text_width,
                        end_x: self.config.canvas_width,
                        y: track as f32 * self.config.track_height,
                        text_width,
                    }
                }
                DanmakuMode::TopFixed => {
                    let end_time = start_time + self.config.fixed_duration;
                    let track = track_alloc.allocate_fixed(start_time, end_time, false);
                    let center_x = (self.config.canvas_width - text_width) / 2.0;
                    DanmakuInstance {
                        item: item.clone(),
                        track,
                        start_time,
                        end_time,
                        start_x: center_x,
                        end_x: center_x,
                        y: track as f32 * self.config.track_height,
                        text_width,
                    }
                }
                DanmakuMode::BottomFixed => {
                    let end_time = start_time + self.config.fixed_duration;
                    let track = track_alloc.allocate_fixed(start_time, end_time, true);
                    let center_x = (self.config.canvas_width - text_width) / 2.0;
                    DanmakuInstance {
                        item: item.clone(),
                        track,
                        start_time,
                        end_time,
                        start_x: center_x,
                        end_x: center_x,
                        y: track as f32 * self.config.track_height,
                        text_width,
                    }
                }
                DanmakuMode::Advanced => {
                    // 高级弹幕暂不支持，跳过
                    continue;
                }
            };

            instances.push(instance);
        }

        tracing::info!(
            "layout: {} items -> {} instances (tracks={})",
            items.len(),
            instances.len(),
            track_alloc.max_tracks,
        );

        instances
    }

    /// 获取给定时间点的活跃弹幕
    pub fn active_at<'a>(instances: &'a [DanmakuInstance], time_secs: f64) -> Vec<&'a DanmakuInstance> {
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
        // 5 ASCII chars * 25 * 0.6 = 75
        assert!((w - 75.0).abs() < 1.0);
    }

    #[test]
    fn test_estimate_text_width_cjk() {
        let w = LayoutEngine::estimate_text_width("你好", 25);
        // 2 CJK chars * 25 = 50
        assert!((w - 50.0).abs() < 1.0);
    }

    #[test]
    fn test_estimate_text_width_mixed() {
        let w = LayoutEngine::estimate_text_width("hi你", 25);
        // 2 ASCII * 15 + 1 CJK * 25 = 55
        assert!((w - 55.0).abs() < 1.0);
    }

    #[test]
    fn test_layout_scroll_right() {
        let config = DanmakuConfig::default();
        let engine = LayoutEngine::new(config);
        let items = vec![make_item(0, DanmakuMode::ScrollRight, "test")];
        let instances = engine.layout(&items);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].track, 0);
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
            canvas_width: 100.0,
            canvas_height: 100.0,
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
        // 轨道应该不同（前几条）
        assert_ne!(instances[0].track, instances[1].track);
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
}
