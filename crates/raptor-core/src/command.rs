use serde::{Deserialize, Serialize};

use crate::state::SeekMode;

/// 播放器命令
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    /// 加载文件
    LoadFile { url: String },
    /// 开始播放
    Play,
    /// 暂停
    Pause,
    /// 切换暂停/播放
    TogglePause,
    /// 停止
    Stop,
    /// 跳转到指定时间
    Seek { target: f64, mode: SeekMode },
    /// 设置音量 (0-100)
    SetVolume { volume: u8 },
    /// 加载字幕文件（SRT/ASS/SSA）
    LoadSubtitle { path: String },
    /// 切换字幕显示/隐藏
    ToggleSubtitle,
    /// 加载弹幕文件（B站XML/JSON）
    LoadDanmaku { path: String },
    /// 切换弹幕显示/隐藏
    ToggleDanmaku,
    /// 设置弹幕不透明度 (0-100)
    SetDanmakuOpacity { opacity: u8 },
    /// 退出
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_play() {
        let json = serde_json::to_string(&Command::Play).unwrap();
        assert_eq!(json, "\"Play\"");
    }

    #[test]
    fn deserialize_play() {
        let cmd: Command = serde_json::from_str("\"Play\"").unwrap();
        assert!(matches!(cmd, Command::Play));
    }

    #[test]
    fn roundtrip_loadfile() {
        let cmd = Command::LoadFile {
            url: "/path/to/test.mp4".into(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let cmd2: Command = serde_json::from_str(&json).unwrap();
        assert!(matches!(cmd2, Command::LoadFile { url } if url == "/path/to/test.mp4"));
    }

    #[test]
    fn roundtrip_seek() {
        let cmd = Command::Seek {
            target: 42.5,
            mode: SeekMode::Absolute,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let cmd2: Command = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(cmd2, Command::Seek { target, .. } if (target - 42.5).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn roundtrip_set_volume() {
        let cmd = Command::SetVolume { volume: 75 };
        let json = serde_json::to_string(&cmd).unwrap();
        let cmd2: Command = serde_json::from_str(&json).unwrap();
        assert!(matches!(cmd2, Command::SetVolume { volume } if volume == 75));
    }

    #[test]
    fn roundtrip_all_commands() {
        let commands = vec![
            Command::Play,
            Command::Pause,
            Command::TogglePause,
            Command::Stop,
            Command::Quit,
            Command::LoadFile {
                url: "test.mp4".into(),
            },
            Command::Seek {
                target: 10.0,
                mode: SeekMode::Absolute,
            },
            Command::SetVolume { volume: 50 },
            Command::LoadSubtitle {
                path: "sub.ass".into(),
            },
            Command::ToggleSubtitle,
            Command::LoadDanmaku {
                path: "dm.xml".into(),
            },
            Command::ToggleDanmaku,
            Command::SetDanmakuOpacity { opacity: 80 },
        ];
        for cmd in commands {
            let json = serde_json::to_string(&cmd).unwrap();
            let _: Command = serde_json::from_str(&json).unwrap();
        }
    }
}
