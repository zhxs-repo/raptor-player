//! Smoke test — 验证 Player 命令分发、错误处理和状态流转
//!
//! 此测试不需要真实视频文件或 GPU 窗口，仅验证核心逻辑。
//! 运行: `cargo run --example smoke_test -p raptor-ffi`

use raptor_core::{Command, RaptorError, SeekMode};
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

    // --- 5. Seek 负数 ---
    println!("\n--- 5. Seek Negative Target ---");
    // 需要先让播放器进入可 seek 状态，这需要一个有效的 pipeline
    // 在 Idle 状态下 seek 会先被状态机拒绝
    let result = player.dispatch_command(Command::Seek {
        target: -1.0,
        mode: SeekMode::Absolute,
    });
    assert_test!(
        "Seek -1.0 returns error (either InvalidArgument or InvalidState)",
        result.is_err()
    );

    // --- 6. TogglePause 在 Idle 应失败 ---
    println!("\n--- 6. TogglePause in Idle ---");
    let result = player.dispatch_command(Command::TogglePause);
    assert_test!("TogglePause in Idle returns error", result.is_err());

    // --- 7. Stop 在 Idle 应失败 ---
    println!("\n--- 7. Stop in Idle ---");
    let result = player.dispatch_command(Command::Stop);
    assert_test!("Stop in Idle returns error", result.is_err());

    // --- 8. Quit 应成功 ---
    println!("\n--- 8. Quit ---");
    let result = player.dispatch_command(Command::Quit);
    assert_test!("Quit succeeds", result.is_ok());

    // 检查是否收到 End 事件
    let event = rx.try_recv();
    assert_test!("End event received", event.is_ok());

    // --- 结果汇总 ---
    println!("\n=== Results: {} passed, {} failed ===", passed, failed);
    if failed > 0 {
        std::process::exit(1);
    } else {
        println!("All smoke tests passed!");
    }
}
