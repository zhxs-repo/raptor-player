use serde::{Deserialize, Serialize};

/// 播放器状态机 — 所有控制流围绕状态转换
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// 出错（加载失败或管线线程崩溃）
    ///
    /// 必须有独立的终态：失败只写日志的话，前端会一直停在 `Loading` 或
    /// `Playing`，界面显示"正在加载/正在播放"而画面其实早已静止。
    Error,
}

/// 非法状态转换
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidStateTransition {
    pub from: PlayerState,
    pub to: PlayerState,
}

impl std::fmt::Display for InvalidStateTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid state transition: {} -> {}", self.from, self.to)
    }
}

impl std::error::Error for InvalidStateTransition {}

/// 触发状态转换的事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlayerEvent {
    Load {
        url: String,
    },
    Play,
    Pause,
    TogglePause,
    Stop,
    Seek {
        target: f64,
        mode: SeekMode,
    },
    SetVolume {
        volume: u8,
    },
    Quit,
    End,
    PlaybackRestart,
    FileLoaded,
    /// 当前操作失败（进入 `Error` 态）
    Fail,
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

            // 任意状态都可能因失败进入 Error（Loading 失败、线程崩溃）
            (_, Fail) => Ok(()),

            // Error → 重新加载 / 停止
            (Error, Load { .. }) => Ok(()),
            (Error, Stop) => Ok(()),
            (Error, End) => Ok(()),
            (Error, Quit) => Ok(()),

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
            // 失败进入 Error；Error 只能重新加载或停止
            (_, Fail) => Some(Error),
            (Error, Load { .. }) => Some(Loading),
            (Error, Stop) => Some(Stopped),
            (Error, End) => Some(Stopped),
            (Error, Quit) => Some(Stopped),
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
            PlayerState::Error => "Error",
        }
    }

    /// 状态到状态的转换是否合法
    ///
    /// `can_transition_to` 描述"这个状态能不能执行该命令"，这里描述"状态机
    /// 能不能落到某个状态"。集中校验让状态写入不可能把机器带进非法组合，
    /// 非法时给出 from/to 而不是静默改写。
    pub fn transition_to(&self, to: PlayerState) -> Result<(), InvalidStateTransition> {
        use PlayerState::*;
        if *self == to {
            return Ok(());
        }
        let allowed = matches!(
            (*self, to),
            (Idle, Loading)
                | (Loading, Ready)
                | (Loading, Stopped)
                | (Loading, Idle)
                | (Ready, Playing)
                | (Ready, Stopped)
                | (Ready, Loading)
                | (Playing, Paused)
                | (Playing, Stopped)
                | (Playing, Loading)
                | (Paused, Playing)
                | (Paused, Stopped)
                | (Paused, Loading)
                | (Stopped, Loading)
                | (Stopped, Idle)
                // Error 是任意状态的终点，只能靠重新加载或停止离开
                | (_, PlayerState::Error)
                | (PlayerState::Error, Loading)
                | (PlayerState::Error, Stopped)
                | (PlayerState::Error, Idle)
        );
        if allowed {
            Ok(())
        } else {
            Err(InvalidStateTransition { from: *self, to })
        }
    }
}

impl std::fmt::Display for PlayerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
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
        assert_eq!(PlayerState::Error.name(), "Error");
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

    // === 额外非法转换测试 ===

    #[test]
    fn idle_cannot_seek() {
        assert!(PlayerState::Idle
            .can_transition_to(&PlayerEvent::Seek {
                target: 5.0,
                mode: SeekMode::Absolute
            })
            .is_err());
    }

    #[test]
    fn stopped_cannot_play() {
        assert!(PlayerState::Stopped
            .can_transition_to(&PlayerEvent::Play)
            .is_err());
    }

    #[test]
    fn stopped_cannot_seek() {
        assert!(PlayerState::Stopped
            .can_transition_to(&PlayerEvent::Seek {
                target: 5.0,
                mode: SeekMode::Absolute
            })
            .is_err());
    }

    #[test]
    fn ready_cannot_pause() {
        assert!(PlayerState::Ready
            .can_transition_to(&PlayerEvent::Pause)
            .is_err());
    }

    #[test]
    fn ready_cannot_seek() {
        assert!(PlayerState::Ready
            .can_transition_to(&PlayerEvent::Seek {
                target: 5.0,
                mode: SeekMode::Absolute
            })
            .is_err());
    }

    #[test]
    fn loading_cannot_seek() {
        assert!(PlayerState::Loading
            .can_transition_to(&PlayerEvent::Seek {
                target: 5.0,
                mode: SeekMode::Absolute
            })
            .is_err());
    }

    // === Error 态 ===

    #[test]
    fn any_state_can_fail_into_error() {
        for state in [
            PlayerState::Idle,
            PlayerState::Loading,
            PlayerState::Ready,
            PlayerState::Playing,
            PlayerState::Paused,
            PlayerState::Stopped,
            PlayerState::Error,
        ] {
            assert!(
                state.can_transition_to(&PlayerEvent::Fail).is_ok(),
                "{state} 应能进入 Error"
            );
            assert_eq!(
                state.next_state(&PlayerEvent::Fail),
                Some(PlayerState::Error)
            );
        }
    }

    #[test]
    fn error_state_can_retry_or_stop() {
        assert!(PlayerState::Error
            .can_transition_to(&PlayerEvent::Load { url: "x".into() })
            .is_ok());
        assert!(PlayerState::Error
            .can_transition_to(&PlayerEvent::Stop)
            .is_ok());
        // 但不能再直接播放：出错后必须先重新加载
        assert!(PlayerState::Error
            .can_transition_to(&PlayerEvent::Play)
            .is_err());
    }

    // === transition_to（状态到状态） ===

    #[test]
    fn transition_to_accepts_normal_flow() {
        let flow = [
            (PlayerState::Idle, PlayerState::Loading),
            (PlayerState::Loading, PlayerState::Ready),
            (PlayerState::Ready, PlayerState::Playing),
            (PlayerState::Playing, PlayerState::Paused),
            (PlayerState::Paused, PlayerState::Playing),
            (PlayerState::Playing, PlayerState::Stopped),
            (PlayerState::Stopped, PlayerState::Loading),
            (PlayerState::Playing, PlayerState::Loading),
        ];
        for (from, to) in flow {
            assert!(from.transition_to(to).is_ok(), "{from} -> {to} 应合法");
        }
        // 同状态写入恒等
        assert!(PlayerState::Playing
            .transition_to(PlayerState::Playing)
            .is_ok());
    }

    #[test]
    fn transition_to_rejects_with_from_and_to() {
        let err = PlayerState::Idle
            .transition_to(PlayerState::Playing)
            .unwrap_err();
        assert_eq!(err.from, PlayerState::Idle);
        assert_eq!(err.to, PlayerState::Playing);
        assert_eq!(err.to_string(), "invalid state transition: Idle -> Playing");
        // Error 只能靠重新加载或停止离开
        assert!(PlayerState::Error
            .transition_to(PlayerState::Playing)
            .is_err());
        assert!(PlayerState::Error
            .transition_to(PlayerState::Loading)
            .is_ok());
        // 任意状态都能落进 Error
        assert!(PlayerState::Paused
            .transition_to(PlayerState::Error)
            .is_ok());
    }

    #[test]
    fn state_display_uses_name() {
        assert_eq!(PlayerState::Error.to_string(), "Error");
        assert_eq!(PlayerState::Error.name(), "Error");
    }
}
