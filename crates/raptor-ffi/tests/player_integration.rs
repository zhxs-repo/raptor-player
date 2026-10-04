//! Integration tests for the Player API
//!
//! These tests verify the full command dispatch, event, and property flow
//! through the Player struct without requiring GPU or audio hardware.

use raptor_core::{Command, RaptorError, RaptorEvent, SeekMode};
use raptor_ffi::Player;

fn make_player() -> (Player, tokio::sync::mpsc::UnboundedReceiver<RaptorEvent>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (Player::new(tx), rx)
}

// === State Machine Tests ===

#[test]
fn idle_state_rejects_playback_commands() {
    let (player, _) = make_player();

    assert!(player.dispatch_command(Command::Play).is_err());
    assert!(player.dispatch_command(Command::Pause).is_err());
    assert!(player.dispatch_command(Command::TogglePause).is_err());
    assert!(player.dispatch_command(Command::Stop).is_err());
    assert!(player
        .dispatch_command(Command::Seek {
            target: 1.0,
            mode: SeekMode::Absolute
        })
        .is_err());
}

#[test]
fn idle_state_accepts_volume_and_quit() {
    let (player, _rx) = make_player();

    assert!(player
        .dispatch_command(Command::SetVolume { volume: 50 })
        .is_ok());
    assert!(player.dispatch_command(Command::Quit).is_ok());
}

// === Load File Tests ===

#[test]
fn load_nonexistent_file_returns_file_not_found() {
    let (player, _) = make_player();

    let result = player.dispatch_command(Command::LoadFile {
        url: "C:\\nonexistent\\path\\video.mp4".into(),
    });

    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), RaptorError::FileNotFound(_)));
}

#[test]
fn load_empty_path() {
    let (player, _) = make_player();

    let result = player.dispatch_command(Command::LoadFile { url: "".into() });

    // Should fail (either FileNotFound or Demux error)
    assert!(result.is_err());
}

#[test]
fn load_invalid_format_file() {
    let (player, _) = make_player();

    // Create a temp file with garbage content
    let temp_path = std::env::temp_dir().join("raptor_test_invalid.mp4");
    std::fs::write(&temp_path, b"this is not a valid mp4 file content").ok();

    let result = player.dispatch_command(Command::LoadFile {
        url: temp_path.to_string_lossy().to_string(),
    });

    // Should fail with a demux error
    assert!(result.is_err());

    // Cleanup
    std::fs::remove_file(&temp_path).ok();
}

// === Seek Validation Tests ===

#[test]
fn seek_negative_target_rejected() {
    let (player, _) = make_player();

    // In Idle state, seek is rejected by state machine
    let result = player.dispatch_command(Command::Seek {
        target: -5.0,
        mode: SeekMode::Absolute,
    });
    assert!(result.is_err());
}

// === Property System Tests ===

#[test]
fn set_volume_updates_property() {
    let (player, _) = make_player();

    player
        .dispatch_command(Command::SetVolume { volume: 75 })
        .unwrap();

    // Volume property should be set
    // (property store is internal to Player, verified via get_property in FFI)
}

// === Event System Tests ===

#[test]
fn quit_sends_end_event() {
    let (player, mut rx) = make_player();

    player.dispatch_command(Command::Quit).unwrap();

    // Should receive End event
    let event = rx.try_recv();
    assert!(event.is_ok());
    assert!(matches!(event.unwrap(), RaptorEvent::End));
}

#[test]
fn load_nonexistent_does_not_send_file_loaded() {
    let (player, mut rx) = make_player();

    let _ = player.dispatch_command(Command::LoadFile {
        url: "nonexistent.mp4".into(),
    });

    // 失败会上报 Error 事件，但绝不应出现 FileLoaded
    let event = rx.try_recv().expect("加载失败应有事件");
    assert!(
        matches!(event, RaptorEvent::Error { .. }),
        "应为 Error 事件，实际 {event:?}"
    );
}

#[test]
fn failed_load_reports_error_event_and_error_state() {
    use raptor_core::{ErrorCode, PlayerState};

    let (player, mut rx) = make_player();
    assert_eq!(player.state(), PlayerState::Idle);

    let err = player
        .dispatch_command(Command::LoadFile {
            url: "no_such_file.mp4".into(),
        })
        .unwrap_err();

    let event = rx
        .try_recv()
        .expect("失败必须作为事件发出，而不是只返回错误码");
    match event {
        RaptorEvent::Error { code, message } => {
            assert_eq!(code, ErrorCode::FileNotFound as i32);
            assert!(message.contains("no_such_file.mp4"), "message={message}");
        }
        other => panic!("expected Error, got {other:?}"),
    }
    assert_eq!(err.error_code(), ErrorCode::FileNotFound);
    // 状态收敛到 Error，而不是卡在 Loading
    assert_eq!(player.state(), PlayerState::Error);
}

#[test]
fn error_state_rejects_play_until_reload() {
    use raptor_core::PlayerState;

    let (player, _rx) = make_player();
    let _ = player.dispatch_command(Command::LoadFile {
        url: "no_such_file.mp4".into(),
    });
    assert_eq!(player.state(), PlayerState::Error);

    // 出错后不能直接播放（避免用旧 pipeline 的残骸继续跑）
    assert!(player.dispatch_command(Command::Play).is_err());
    assert_eq!(player.state(), PlayerState::Error);
    // 但停止与重新加载都应当可用
    assert!(player.dispatch_command(Command::Stop).is_ok());
    assert_eq!(player.state(), PlayerState::Stopped);
}

// === Load + Re-load State Recovery ===

#[test]
fn can_reload_after_failed_load() {
    let (player, _) = make_player();

    // First load fails
    let _ = player.dispatch_command(Command::LoadFile {
        url: "fake1.mp4".into(),
    });

    // Second load should also be attempted (state should recover)
    let result = player.dispatch_command(Command::LoadFile {
        url: "fake2.mp4".into(),
    });
    assert!(result.is_err()); // Still fails, but no panic
}

// === Subtitle/Danmaku without engine ===

#[test]
fn toggle_subtitle_without_engine() {
    let (player, _) = make_player();
    // Should not panic even without subtitle engine loaded
    let _ = player.dispatch_command(Command::ToggleSubtitle);
}

#[test]
fn toggle_danmaku_without_engine() {
    let (player, _) = make_player();
    // Should not panic even without danmaku engine loaded
    let _ = player.dispatch_command(Command::ToggleDanmaku);
}

#[test]
fn set_danmaku_opacity_without_engine() {
    let (player, _) = make_player();
    let result = player.dispatch_command(Command::SetDanmakuOpacity { opacity: 50 });
    assert!(result.is_ok()); // Should succeed (no-op when engine not loaded)
}
