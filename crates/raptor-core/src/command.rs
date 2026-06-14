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
