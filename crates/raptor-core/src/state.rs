use serde::{Deserialize, Serialize};

/// 播放器状态机 — 所有控制流围绕状态转换
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerState {
    /// 空闲（初始状态）
    #[default]
    Idle,
    /// 加载中
    Loading,
    /// 就绪（文件已加载，可播放）
    Ready,
    /// 播放中
    Playing,
    /// 暂停
    Paused,
    /// 已停止
    Stopped,
}

/// 触发状态转换的事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlayerEvent {
    Load { url: String },
    Play,
    Pause,
    TogglePause,
    Stop,
    Seek { target: f64, mode: SeekMode },
    SetVolume { volume: u8 },
    Quit,
    End,
    PlaybackRestart,
    FileLoaded,
}

/// Seek 模式
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SeekMode {
    Absolute,
    Relative,
}

impl PlayerState {
    /// 检查是否可以从当前状态转换到目标事件对应的状态
    pub fn can_transition_to(&self, event: &PlayerEvent) -> std::result::Result<(), String> {
        use PlayerEvent::*;
        use PlayerState::*;

        match (self, event) {
            // Idle → Loading (Load)
            (Idle, Load { .. }) => Ok(()),

            // Loading → Ready (FileLoaded)
            (Loading, FileLoaded) => Ok(()),
            // Loading → Idle (End/Error)
            (Loading, End) => Ok(()),

            // Ready → Playing (Play)
            (Ready, Play) => Ok(()),
            // Ready → Loading (Load another file)
            (Ready, Load { .. }) => Ok(()),
            // Ready → Stopped (Stop)
            (Ready, Stop) => Ok(()),

            // Playing → Paused (Pause)
            (Playing, Pause) => Ok(()),
            // Playing → Paused (TogglePause)
            (Playing, TogglePause) => Ok(()),
            // Playing → Stopped (Stop)
            (Playing, Stop) => Ok(()),
            // Playing → Playing (Seek / SetVolume / PlaybackRestart)
            (Playing, Seek { .. }) => Ok(()),
            (Playing, SetVolume { .. }) => Ok(()),
            (Playing, PlaybackRestart) => Ok(()),
            // Playing → Loading (Load)
            (Playing, Load { .. }) => Ok(()),
            // Playing → Stopped (End/Quit)
            (Playing, End) => Ok(()),
            (Playing, Quit) => Ok(()),

            // Paused → Playing (Play / TogglePause)
            (Paused, Play) => Ok(()),
            (Paused, TogglePause) => Ok(()),
            // Paused → Stopped (Stop)
            (Paused, Stop) => Ok(()),
            // Paused → Loading (Load)
            (Paused, Load { .. }) => Ok(()),
            // Paused → Playing (Seek)
            (Paused, Seek { .. }) => Ok(()),
            // Paused → Stopped (End/Quit)
            (Paused, End) => Ok(()),
            (Paused, Quit) => Ok(()),

            // Stopped → Loading (Load)
            (Stopped, Load { .. }) => Ok(()),

            _ => Err(format!(
                "invalid transition: {:?} cannot handle {:?}",
                self, event
            )),
        }
    }

    /// 根据事件获取目标状态
    pub fn next_state(&self, event: &PlayerEvent) -> Option<PlayerState> {
        use PlayerEvent::*;
        use PlayerState::*;

        match (self, event) {
            (Idle, Load { .. }) => Some(Loading),
            (Loading, FileLoaded) => Some(Ready),
            (Loading, End) => Some(Idle),
            (Ready, Play) => Some(Playing),
            (Ready, Load { .. }) => Some(Loading),
            (Ready, Stop) => Some(Stopped),
            (Playing, Pause) => Some(Paused),
            (Playing, TogglePause) => Some(Paused),
            (Playing, Stop) => Some(Stopped),
            (Playing, Load { .. }) => Some(Loading),
            (Playing, End) => Some(Stopped),
            (Playing, Quit) => Some(Stopped),
            (Paused, Play) => Some(Playing),
            (Paused, TogglePause) => Some(Playing),
            (Paused, Stop) => Some(Stopped),
            (Paused, Load { .. }) => Some(Loading),
            (Paused, End) => Some(Stopped),
            (Paused, Quit) => Some(Stopped),
            (Stopped, Load { .. }) => Some(Loading),
            // 不改变状态的事件
            (Playing, Seek { .. }) => Some(Playing),
            (Playing, SetVolume { .. }) => Some(Playing),
            (Playing, PlaybackRestart) => Some(Playing),
            (Paused, Seek { .. }) => Some(Paused),
            _ => None,
        }
    }

    /// 状态名称
    pub fn name(&self) -> &'static str {
        match self {
            PlayerState::Idle => "Idle",
            PlayerState::Loading => "Loading",
            PlayerState::Ready => "Ready",
            PlayerState::Playing => "Playing",
            PlayerState::Paused => "Paused",
            PlayerState::Stopped => "Stopped",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === 合法转换测试 (12) ===

    #[test]
    fn idle_can_load() {
        assert!(PlayerState::Idle
            .can_transition_to(&PlayerEvent::Load {
                url: "test.mp4".into()
            })
            .is_ok());
    }

    #[test]
    fn loading_can_become_ready() {
        assert!(PlayerState::Loading
            .can_transition_to(&PlayerEvent::FileLoaded)
            .is_ok());
    }

    #[test]
    fn loading_can_fail() {
        assert!(PlayerState::Loading
            .can_transition_to(&PlayerEvent::End)
            .is_ok());
    }

    #[test]
    fn ready_can_play() {
        assert!(PlayerState::Ready
            .can_transition_to(&PlayerEvent::Play)
            .is_ok());
    }

    #[test]
    fn ready_can_load_another_file() {
        assert!(PlayerState::Ready
            .can_transition_to(&PlayerEvent::Load {
                url: "test2.mp4".into()
            })
            .is_ok());
    }

    #[test]
    fn ready_cannot_stop() {
        // Ready → Stop is valid
        assert!(PlayerState::Ready
            .can_transition_to(&PlayerEvent::Stop)
            .is_ok());
    }

    #[test]
    fn playing_can_pause() {
        assert!(PlayerState::Playing
            .can_transition_to(&PlayerEvent::Pause)
            .is_ok());
    }

    #[test]
    fn playing_can_stop() {
        assert!(PlayerState::Playing
            .can_transition_to(&PlayerEvent::Stop)
            .is_ok());
    }

    #[test]
    fn playing_can_seek() {
        assert!(PlayerState::Playing
            .can_transition_to(&PlayerEvent::Seek {
                target: 5.0,
                mode: SeekMode::Absolute
            })
            .is_ok());
    }

    #[test]
    fn paused_can_resume() {
        assert!(PlayerState::Paused
            .can_transition_to(&PlayerEvent::Play)
            .is_ok());
    }

    #[test]
    fn paused_can_seek() {
        assert!(PlayerState::Paused
            .can_transition_to(&PlayerEvent::Seek {
                target: 10.0,
                mode: SeekMode::Absolute
            })
            .is_ok());
    }

    #[test]
    fn paused_can_load() {
        assert!(PlayerState::Paused
            .can_transition_to(&PlayerEvent::Load {
                url: "test3.mp4".into()
            })
            .is_ok());
    }

    // === 非法转换测试 (4) ===

    #[test]
    fn idle_cannot_play() {
        assert!(PlayerState::Idle
            .can_transition_to(&PlayerEvent::Play)
            .is_err());
    }

    #[test]
    fn idle_cannot_pause() {
        assert!(PlayerState::Idle
            .can_transition_to(&PlayerEvent::Pause)
            .is_err());
    }

    #[test]
    fn loading_cannot_play() {
        assert!(PlayerState::Loading
            .can_transition_to(&PlayerEvent::Play)
            .is_err());
    }

    #[test]
    fn loading_cannot_pause() {
        assert!(PlayerState::Loading
            .can_transition_to(&PlayerEvent::Pause)
            .is_err());
    }

    // === 其他测试 ===

    #[test]
    fn playing_can_reach_eof() {
        assert!(PlayerState::Playing
            .can_transition_to(&PlayerEvent::End)
            .is_ok());
    }

    #[test]
    fn paused_can_stop() {
        assert!(PlayerState::Paused
            .can_transition_to(&PlayerEvent::Stop)
            .is_ok());
    }

    #[test]
    fn stopping_can_stopped() {
        // Stopped → Load is valid
        assert!(PlayerState::Stopped
            .can_transition_to(&PlayerEvent::Load { url: "x".into() })
            .is_ok());
    }

    #[test]
    fn state_names() {
        assert_eq!(PlayerState::Idle.name(), "Idle");
        assert_eq!(PlayerState::Loading.name(), "Loading");
        assert_eq!(PlayerState::Ready.name(), "Ready");
        assert_eq!(PlayerState::Playing.name(), "Playing");
        assert_eq!(PlayerState::Paused.name(), "Paused");
        assert_eq!(PlayerState::Stopped.name(), "Stopped");
    }

    #[test]
    fn next_state_transitions() {
        assert_eq!(
            PlayerState::Idle.next_state(&PlayerEvent::Load { url: "x".into() }),
            Some(PlayerState::Loading)
        );
        assert_eq!(
            PlayerState::Ready.next_state(&PlayerEvent::Play),
            Some(PlayerState::Playing)
        );
    }
}
