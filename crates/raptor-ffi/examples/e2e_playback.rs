//! E2E 播放验证 — 自动化脚本驱动完整播放链路
//!
//! 验证: Load → Play → Pause → Seek → Resume → Stop
//! 运行: `cargo run --example e2e_playback -p raptor-ffi -- test_video.mp4`

use raptor_core::{Command, RaptorEvent, SeekMode};
use raptor_ffi::Player;

fn main() {
    std::env::set_var("RUST_BACKTRACE", "1");
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "raptor=warn");
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().collect();
    let video_path = if args.len() > 1 {
        args[1].clone()
    } else {
        "test_video.mp4".into()
    };

    println!("=== Raptor E2E Playback Verification ===");
    println!("Video: {video_path}\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    macro_rules! check {
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

    // --- Setup ---
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<RaptorEvent>();
    let player = Player::new(event_tx);

    // ================================================================
    // Step 1: Load file
    // ================================================================
    println!("\n--- Step 1: Load File ---");
    let load_result = player.dispatch_command(Command::LoadFile {
        url: video_path.clone(),
    });
    check!("LoadFile succeeds", load_result.is_ok());

    // Drain events until FileLoaded (timeout 5s)
    let mut file_loaded = false;
    let mut loaded_duration = 0.0f64;
    let mut loaded_width = 0u32;
    let mut loaded_height = 0u32;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Ok(RaptorEvent::FileLoaded {
            duration,
            video,
            audio: _,
        }) = event_rx.try_recv()
        {
            file_loaded = true;
            loaded_duration = duration;
            if let Some(vi) = video {
                loaded_width = vi.width;
                loaded_height = vi.height;
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    check!("FileLoaded event received", file_loaded);
    check!(
        &format!("Duration ~= 5.0s (got {loaded_duration:.2}s)"),
        (loaded_duration - 5.0).abs() < 1.0
    );
    check!(
        &format!("Video 640x360 (got {loaded_width}x{loaded_height})"),
        loaded_width == 640 && loaded_height == 360
    );

    if !file_loaded {
        eprintln!("FATAL: FileLoaded not received, aborting.");
        std::process::exit(1);
    }

    // ================================================================
    // Step 2: Play
    // ================================================================
    println!("\n--- Step 2: Play ---");
    let play_result = player.dispatch_command(Command::Play);
    check!("Play command succeeds", play_result.is_ok());

    // Let it play for ~1.5 seconds, sampling position
    std::thread::sleep(std::time::Duration::from_millis(1500));

    let stats = player.get_hud_stats();
    let pos_after_play = stats.position_secs;
    let paused_after_play = stats.paused;
    check!(
        &format!("Position advanced > 0 (got {pos_after_play:.2}s)"),
        pos_after_play > 0.0
    );
    check!(
        &format!("Not paused during play (got paused={paused_after_play})"),
        !paused_after_play
    );

    // ================================================================
    // Step 3: Pause
    // ================================================================
    println!("\n--- Step 3: Pause ---");
    let pause_result = player.dispatch_command(Command::Pause);
    check!("Pause command succeeds", pause_result.is_ok());

    // Wait briefly for pause to take effect
    std::thread::sleep(std::time::Duration::from_millis(200));

    let stats_paused = player.get_hud_stats();
    check!(
        &format!("Paused = true (got {})", stats_paused.paused),
        stats_paused.paused
    );
    let pos_at_pause = stats_paused.position_secs;
    check!(
        &format!("Position at pause > 0 (got {pos_at_pause:.2}s)"),
        pos_at_pause > 0.0
    );

    // Record position, wait 300ms, verify position did NOT advance
    std::thread::sleep(std::time::Duration::from_millis(300));
    let pos_after_wait = player.get_hud_stats().position_secs;
    check!(
        &format!(
            "Position frozen while paused (pause={pos_at_pause:.2}s, after={pos_after_wait:.2}s)"
        ),
        (pos_after_wait - pos_at_pause).abs() < 0.1
    );

    // ================================================================
    // Step 4: Seek to 2.0s (while paused)
    // ================================================================
    println!("\n--- Step 4: Seek to 2.0s ---");
    let seek_result = player.dispatch_command(Command::Seek {
        target: 2.0,
        mode: SeekMode::Absolute,
    });
    check!("Seek command succeeds", seek_result.is_ok());

    // 暂停状态下 position reporter 不刷新，需要先恢复播放再验证位置
    // ================================================================
    // Step 5: Resume play from seek position
    // ================================================================
    println!("\n--- Step 5: Resume Play ---");
    let resume_result = player.dispatch_command(Command::Play);
    check!("Play (resume) command succeeds", resume_result.is_ok());

    // 等待 seek 生效 + 位置更新（~800ms）
    std::thread::sleep(std::time::Duration::from_millis(800));
    let stats_resume = player.get_hud_stats();
    let pos_after_resume = stats_resume.position_secs;
    check!(
        &format!("Position jumped past seek target ~2.0s (got {pos_after_resume:.2}s)"),
        pos_after_resume >= 1.5
    );
    check!(
        &format!("Not paused after resume (got {})", stats_resume.paused),
        !stats_resume.paused
    );

    // 回归：seek 之前在途的 packet/frame 已被 generation 标记拦下，
    // 位置不得被旧数据拽回去（历史缺陷：seek 后进度条 2.0s → 1.5s → 2.0s 回跳）
    let mut positions = vec![pos_after_resume];
    for _ in 0..12 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        positions.push(player.get_hud_stats().position_secs);
    }
    let max_regress = positions
        .windows(2)
        .map(|w| w[0] - w[1])
        .fold(0.0_f64, f64::max);
    check!(
        &format!(
            "Position never regresses after seek (worst drop {max_regress:.2}s, samples {positions:?})"
        ),
        max_regress < 0.1
    );
    check!(
        &format!(
            "Position keeps advancing after seek ({:.2}s -> {:.2}s)",
            positions[0],
            positions[positions.len() - 1]
        ),
        positions[positions.len() - 1] >= positions[0]
    );

    // ================================================================
    // Step 6: Stop (Stop 不发送 EndFile，EndFile 仅由自然播放到末尾触发)
    // ================================================================
    println!("\n--- Step 6: Stop ---");
    let stop_result = player.dispatch_command(Command::Stop);
    check!("Stop command succeeds", stop_result.is_ok());

    // 验证 Stop 后 pipeline 已关闭（position reporter 停止）
    std::thread::sleep(std::time::Duration::from_millis(200));
    let stats_stopped = player.get_hud_stats();
    check!(
        &format!("Pipeline paused after Stop (got {})", stats_stopped.paused),
        stats_stopped.paused
    );

    // ================================================================
    // Step 7: Clean shutdown (drop Player)
    // ================================================================
    println!("\n--- Step 7: Clean Shutdown ---");
    let quit_result = player.dispatch_command(Command::Quit);
    check!("Quit command succeeds", quit_result.is_ok());

    // Wait for End event
    let mut got_end = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if let Ok(RaptorEvent::End) = event_rx.try_recv() {
            got_end = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    check!("End event received after Quit", got_end);

    // Drop player — should not panic
    drop(player);
    check!("Player dropped without panic", true);

    // Give threads a moment to join
    std::thread::sleep(std::time::Duration::from_millis(200));

    // ================================================================
    // Summary
    // ================================================================
    println!("\n{}", "=".repeat(40));
    println!("=== Results: {passed} passed, {failed} failed ===");
    if failed > 0 {
        println!("E2E VERIFICATION FAILED");
        std::process::exit(1);
    } else {
        println!("E2E VERIFICATION PASSED");
    }
}
