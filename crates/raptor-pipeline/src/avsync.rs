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

/// 时钟内部状态 — 单一 Mutex 保护，避免多锁死锁
struct ClockState {
    start_instant: Option<Instant>,
    base_pts: f64,
    video_clock: f64,
    consecutive_drops: u32,
}

/// AV 同步器 — 以挂钟（wall-clock）为主时钟
///
/// 主时钟 = `Instant::now() - start_instant + base_pts`，
/// 保证以真实时间推进，不受解码/缓冲速度影响。
/// 视频帧 PTS 与主时钟对比，决定显示/等待/丢弃。
///
/// **防丢帧死亡螺旋**：当连续丢帧超过阈值时，自动重新同步主时钟到
/// 当前视频 PTS，避免挂钟与视频进度永久脱节导致画面冻结。
pub struct AVSync {
    state: Mutex<ClockState>,
    /// 最大同步阈值（秒），视频落后超过此值则丢帧
    max_sync_threshold: f64,
    /// 连续丢帧上限，超过后强制重新同步
    max_consecutive_drops: u32,
    /// 主时钟与视频 PTS 最大允许漂移（秒），超过则重新同步
    max_drift_secs: f64,
}

impl AVSync {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(ClockState {
                start_instant: None,
                base_pts: 0.0,
                video_clock: 0.0,
                consecutive_drops: 0,
            }),
            max_sync_threshold: 0.1,
            max_consecutive_drops: 5,
            max_drift_secs: 2.0,
        }
    }

    /// 设置首帧时间基准 — 在收到第一个视频帧时调用
    pub fn set_first_frame_time(&self, pts: f64) {
        let mut state = self.state.lock();
        if state.start_instant.is_none() {
            state.start_instant = Some(Instant::now());
            state.base_pts = pts;
            tracing::info!("AVSync: first frame pts={:.3}s", pts);
        }
    }

    /// 获取当前主时钟值（秒）
    pub fn master_clock(&self) -> f64 {
        let state = self.state.lock();
        if let Some(instant) = state.start_instant {
            instant.elapsed().as_secs_f64() + state.base_pts
        } else {
            0.0
        }
    }

    /// 视频帧同步决策
    ///
    /// 当视频落后于主时钟超过 `max_sync_threshold` 时返回 Drop。
    /// 但如果连续丢帧超过 `max_consecutive_drops`，自动重新同步主时钟
    /// 到当前帧 PTS，返回 Display 以打破死亡螺旋。
    pub fn video_sync_decision(&self, frame_pts: f64) -> VideoSyncDecision {
        let mut state = self.state.lock();

        if state.start_instant.is_none() {
            return VideoSyncDecision::Display;
        }

        let elapsed = state.start_instant.unwrap().elapsed().as_secs_f64();
        let clock = elapsed + state.base_pts;
        let diff = frame_pts - clock;

        if diff > 0.01 {
            // 帧比时钟超前 > 10ms → 等待
            state.consecutive_drops = 0;
            VideoSyncDecision::Wait(diff.min(0.05))
        } else if diff < -self.max_sync_threshold {
            // 帧落后超过阈值
            state.consecutive_drops += 1;

            // 防死亡螺旋：连续丢帧过多 或 漂移过大 → 重新同步
            if state.consecutive_drops >= self.max_consecutive_drops
                || (-diff) > self.max_drift_secs
            {
                tracing::warn!(
                    "AVSync: re-sync after {} consecutive drops, drift={:.3}s, \
                     frame_pts={:.3}s, clock={:.3}s",
                    state.consecutive_drops,
                    -diff,
                    frame_pts,
                    clock
                );
                // 重新设置时间基准，让 master_clock ≈ frame_pts
                state.start_instant = Some(Instant::now());
                state.base_pts = frame_pts;
                state.consecutive_drops = 0;
                return VideoSyncDecision::Display;
            }

            VideoSyncDecision::Drop
        } else {
            // 正常范围
            state.consecutive_drops = 0;
            VideoSyncDecision::Display
        }
    }

    /// 更新视频时钟（诊断用）
    pub fn update_video_clock(&self, pts: f64) {
        self.state.lock().video_clock = pts;
    }

    /// 重置（seek 后调用）
    pub fn reset(&self, seek_target: f64) {
        let mut state = self.state.lock();
        state.start_instant = None;
        state.base_pts = seek_target;
        state.video_clock = 0.0;
        state.consecutive_drops = 0;
        tracing::info!("AVSync: reset to seek_target={:.3}s", seek_target);
    }

    /// 获取视频时钟（诊断）
    pub fn video_clock(&self) -> f64 {
        self.state.lock().video_clock
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

    #[test]
    fn consecutive_drops_trigger_resync() {
        let sync = AVSync::new();
        sync.set_first_frame_time(0.0);

        // 等待足够久让帧"落后"超过阈值
        std::thread::sleep(std::time::Duration::from_millis(300));

        // 连续发送落后帧，前 4 次应该 Drop
        for _ in 0..4 {
            let decision = sync.video_sync_decision(0.0);
            assert!(matches!(decision, VideoSyncDecision::Drop));
        }

        // 第 5 次触发 re-sync，应返回 Display（打破死亡螺旋）
        let decision = sync.video_sync_decision(0.0);
        assert!(
            matches!(decision, VideoSyncDecision::Display),
            "Expected Display after 5 consecutive drops, got {:?}",
            decision
        );
    }
}
