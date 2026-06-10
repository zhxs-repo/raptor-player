use parking_lot::Mutex;
use std::time::Instant;

/// 视频同步决策
#[derive(Debug, Clone, Copy)]
pub enum VideoSyncDecision {
    /// 正常显示
    Display,
    /// 等待指定秒数后显示
    Wait(f64),
    /// 丢弃该帧（太落后）
    Drop,
}

/// AV 同步器 — 以挂钟（wall-clock）为主时钟
///
/// 主时钟 = `Instant::now() - start_instant + base_pts`，
/// 保证以真实时间推进，不受解码/缓冲速度影响。
/// 视频帧 PTS 与主时钟对比，决定显示/等待/丢弃。
pub struct AVSync {
    /// 播放开始的挂钟时刻（首帧显示时设置）
    start_instant: Mutex<Option<Instant>>,
    /// 播放起始 PTS（首帧 PTS 或 seek 目标时间）
    base_pts: Mutex<f64>,
    /// 最近一次更新的视频 PTS（用于诊断）
    video_clock: Mutex<f64>,
    /// 最大同步阈值（秒），视频落后超过此值则丢帧
    max_sync_threshold: f64,
}

impl AVSync {
    pub fn new() -> Self {
        Self {
            start_instant: Mutex::new(None),
            base_pts: Mutex::new(0.0),
            video_clock: Mutex::new(0.0),
            max_sync_threshold: 0.1,
        }
    }

    /// 设置首帧时间基准 — 在收到第一个视频帧时调用
    pub fn set_first_frame_time(&self, pts: f64) {
        let mut start = self.start_instant.lock();
        if start.is_none() {
            *start = Some(Instant::now());
            *self.base_pts.lock() = pts;
            tracing::info!("AVSync: first frame pts={:.3}s", pts);
        }
    }

    /// 获取当前主时钟值（秒）
    pub fn master_clock(&self) -> f64 {
        let start = self.start_instant.lock();
        if let Some(instant) = *start {
            let elapsed = instant.elapsed().as_secs_f64();
            elapsed + *self.base_pts.lock()
        } else {
            0.0
        }
    }

    /// 视频帧同步决策
    pub fn video_sync_decision(&self, frame_pts: f64) -> VideoSyncDecision {
        let clock = self.master_clock();
        if clock == 0.0 {
            return VideoSyncDecision::Display;
        }

        let diff = frame_pts - clock;

        if diff > 0.01 {
            // 帧比时钟超前 > 10ms → 等待
            VideoSyncDecision::Wait(diff.min(0.05))
        } else if diff < -self.max_sync_threshold {
            // 帧落后超过阈值 → 丢弃
            VideoSyncDecision::Drop
        } else {
            // 正常范围
            VideoSyncDecision::Display
        }
    }

    /// 更新视频时钟（诊断用）
    pub fn update_video_clock(&self, pts: f64) {
        *self.video_clock.lock() = pts;
    }

    /// 重置（seek 后调用）
    pub fn reset(&self, seek_target: f64) {
        *self.start_instant.lock() = None;
        *self.base_pts.lock() = seek_target;
        *self.video_clock.lock() = 0.0;
        tracing::info!("AVSync: reset to seek_target={:.3}s", seek_target);
    }

    /// 获取视频时钟（诊断）
    pub fn video_clock(&self) -> f64 {
        *self.video_clock.lock()
    }
}

impl Default for AVSync {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_avsync_new() {
        let sync = AVSync::new();
        assert_eq!(sync.master_clock(), 0.0);
        assert_eq!(sync.video_clock(), 0.0);
    }

    #[test]
    fn test_set_first_frame_time() {
        let sync = AVSync::new();
        sync.set_first_frame_time(1.5);
        let clock = sync.master_clock();
        // 刚设置，时钟应接近 1.5
        assert!((clock - 1.5).abs() < 0.1);
    }

    #[test]
    fn test_video_sync_decision_display() {
        let sync = AVSync::new();
        sync.set_first_frame_time(0.0);
        // 帧 PTS 与当前时钟接近 → Display
        let decision = sync.video_sync_decision(0.0);
        assert!(matches!(decision, VideoSyncDecision::Display));
    }

    #[test]
    fn test_video_sync_decision_wait() {
        let sync = AVSync::new();
        sync.set_first_frame_time(0.0);
        // 帧 PTS 远在未来 → Wait
        let decision = sync.video_sync_decision(10.0);
        assert!(matches!(decision, VideoSyncDecision::Wait(_)));
    }

    #[test]
    fn test_video_sync_decision_drop() {
        let sync = AVSync::new();
        sync.set_first_frame_time(0.0);
        // 帧 PTS 远在过去 → Drop
        std::thread::sleep(std::time::Duration::from_millis(200));
        let decision = sync.video_sync_decision(0.0);
        assert!(matches!(decision, VideoSyncDecision::Drop));
    }

    #[test]
    fn test_reset() {
        let sync = AVSync::new();
        sync.set_first_frame_time(1.0);
        sync.update_video_clock(1.5);
        sync.reset(5.0);
        assert_eq!(sync.video_clock(), 0.0);
        // reset 后 start_instant 为 None，master_clock 应为 0
        assert_eq!(sync.master_clock(), 0.0);
    }

    #[test]
    fn default_matches_new() {
        let a = AVSync::new();
        let b = AVSync::default();
        assert_eq!(a.master_clock(), b.master_clock());
        assert_eq!(a.video_clock(), b.video_clock());
    }

    #[test]
    fn display_decision_before_start() {
        let sync = AVSync::new();
        // 未设置首帧，master_clock == 0.0 → 始终 Display
        let decision = sync.video_sync_decision(5.0);
        assert!(matches!(decision, VideoSyncDecision::Display));
    }

    #[test]
    fn first_frame_only_set_once() {
        let sync = AVSync::new();
        sync.set_first_frame_time(1.0);
        sync.set_first_frame_time(99.0); // 第二次调用应被忽略
        let clock = sync.master_clock();
        // 应接近 1.0 而非 99.0
        assert!((clock - 1.0).abs() < 0.5);
    }

    #[test]
    fn update_video_clock_records_pts() {
        let sync = AVSync::new();
        sync.update_video_clock(2.71);
        assert!((sync.video_clock() - 2.71).abs() < f64::EPSILON);
    }
}
