//! Smoke test — 验证 Player 命令分发、错误处理和状态流转
//!
//! 此测试不需要真实视频文件或 GPU 窗口，仅验证核心逻辑。
//! 运行: `cargo run --example smoke_test -p raptor-ffi`

use raptor_core::{Command, ErrorCode, PlayerState, RaptorError, RaptorEvent, SeekMode};
use raptor_ffi::Player;

fn main() {
    println!("=== Raptor MVP Smoke Test ===\n");
    let mut passed = 0u32;
    let mut failed = 0u32;

    macro_rules! assert_test {
        ($name:expr, $cond:expr) => {
            if $cond {
                println!("  [PASS] {}", $name);
                passed += 1;
            } else {
                println!("  [FAIL] {}", $name);
                failed += 1;
            }
        };
    }

    // --- 1. 创建播放器 ---
    println!("\n--- 1. Player Creation ---");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let player = Player::new(tx);
    assert_test!("player created successfully", true);

    // --- 2. Idle 状态下非法命令 ---
    println!("\n--- 2. Illegal Commands in Idle State ---");

    let result = player.dispatch_command(Command::Play);
    assert_test!("Play in Idle returns error", result.is_err());

    let result = player.dispatch_command(Command::Pause);
    assert_test!("Pause in Idle returns error", result.is_err());

    let result = player.dispatch_command(Command::Seek {
        target: 5.0,
        mode: SeekMode::Absolute,
    });
    assert_test!("Seek in Idle returns error", result.is_err());

    // --- 3. SetVolume 在任何状态都应成功 ---
    println!("\n--- 3. SetVolume in Idle ---");
    let result = player.dispatch_command(Command::SetVolume { volume: 80 });
    assert_test!("SetVolume in Idle succeeds", result.is_ok());

    // --- 4. LoadFile 不存在文件 ---
    println!("\n--- 4. LoadFile with Non-existent File ---");
    let result = player.dispatch_command(Command::LoadFile {
        url: "C:\\nonexistent\\path\\test.mp4".into(),
    });
    assert_test!("LoadFile nonexistent returns error", result.is_err());
    if let Err(RaptorError::FileNotFound(_)) = &result {
        assert_test!("error is FileNotFound", true);
    } else {
        assert_test!("error is FileNotFound", false);
    }
    // 失败必须让状态收敛到 Error（历史上的缺陷是留在 Loading 永不返回）
    assert_test!(
        &format!("failed load lands in Error (got {:?})", player.state()),
        player.state() == PlayerState::Error
    );
    // 并且要作为事件出去，前端不必逐条命令轮询返回值
    let mut saw_error_event = false;
    while let Ok(event) = rx.try_recv() {
        if let RaptorEvent::Error { code, .. } = event {
            saw_error_event = code == ErrorCode::FileNotFound as i32;
        }
    }
    assert_test!("failure emits Error event", saw_error_event);

    // --- 5. Error 态：可以 Stop 退出，但不能直接播 ---
    println!("\n--- 5. Error State Recovery ---");
    let result = player.dispatch_command(Command::Play);
    assert_test!("Play in Error returns error", result.is_err());
    let result = player.dispatch_command(Command::Stop);
    assert_test!(
        &format!(
            "Stop exits Error (ok={}, {:?})",
            result.is_ok(),
            player.state()
        ),
        result.is_ok() && player.state() == PlayerState::Stopped
    );

    // --- 6. Idle 状态下的非法命令（新实例，未被上面的失败污染）---
    println!("\n--- 6. Illegal Commands in Idle ---");
    let (idle_tx, _idle_rx) = tokio::sync::mpsc::unbounded_channel();
    let idle_player = Player::new(idle_tx);

    // 需要先让播放器进入可 seek 状态，这需要一个有效的 pipeline
    // 在 Idle 状态下 seek 会先被状态机拒绝
    let result = idle_player.dispatch_command(Command::Seek {
        target: -1.0,
        mode: SeekMode::Absolute,
    });
    assert_test!(
        "Seek -1.0 returns error (either InvalidArgument or InvalidState)",
        result.is_err()
    );

    let result = idle_player.dispatch_command(Command::TogglePause);
    assert_test!("TogglePause in Idle returns error", result.is_err());

    let result = idle_player.dispatch_command(Command::Stop);
    assert_test!("Stop in Idle returns error", result.is_err());

    // --- 7. Quit 应成功 ---
    println!("\n--- 7. Quit ---");
    let result = player.dispatch_command(Command::Quit);
    assert_test!("Quit succeeds", result.is_ok());

    // 事件通道里还有步骤 4 的 Error 事件，只要能看到 End 就行
    let mut saw_end = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, RaptorEvent::End) {
            saw_end = true;
        }
    }
    assert_test!("End event received", saw_end);

    // --- 结果汇总 ---
    println!("\n=== Results: {} passed, {} failed ===", passed, failed);
    if failed > 0 {
        std::process::exit(1);
    } else {
        println!("All smoke tests passed!");
    }
}
