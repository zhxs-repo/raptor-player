use raptor_core::{Command, RaptorEvent};
use raptor_ffi::Player;
use std::io::{self, BufRead, Write};

/// 用户交互命令
enum UserCmd {
    LoadSubtitle(String),
    ToggleSubtitle,
    LoadDanmaku(String),
    ToggleDanmaku,
    SetDanmakuOpacity(u8),
    TogglePause,
    Quit,
}

fn main() {
    std::env::set_var("RUST_BACKTRACE", "1");
    // 默认输出 info 级别日志（包括 HUD 诊断信息）
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "raptor=info");
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("raptor=info".parse().unwrap()),
        )
        .init();

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        eprintln!("\n=== PANIC: {} ===\n{}\n=== END PANIC ===\n", info, bt);
        default_hook(info);
    }));

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: cli_player <video_file> [--subtitle <srt/ass>] [--danmaku <xml/json>]");
        std::process::exit(1);
    }

    let file_path = &args[1];

    // 解析可选参数
    let mut subtitle_path: Option<String> = None;
    let mut danmaku_path: Option<String> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--subtitle" | "-s" if i + 1 < args.len() => {
                subtitle_path = Some(args[i + 1].clone());
                i += 2;
            }
            "--danmaku" | "-d" if i + 1 < args.len() => {
                danmaku_path = Some(args[i + 1].clone());
                i += 2;
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                i += 1;
            }
        }
    }

    println!("╔══════════════════════════════════════════╗");
    println!("║         Raptor CLI Player               ║");
    println!("╚══════════════════════════════════════════╝");
    println!("Video: {}", file_path);

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let player = Player::new(event_tx);

    // 加载文件
    if let Err(e) = player.dispatch_command(Command::LoadFile {
        url: file_path.clone(),
    }) {
        eprintln!("Failed to load file: {}", e);
        std::process::exit(1);
    }

    // 自动加载字幕/弹幕
    if let Some(ref sub) = subtitle_path {
        println!("Loading subtitle: {}", sub);
        match player.dispatch_command(Command::LoadSubtitle { path: sub.clone() }) {
            Ok(_) => println!("  Subtitle loaded!"),
            Err(e) => eprintln!("  Warning: {}", e),
        }
    }
    if let Some(ref dm) = danmaku_path {
        println!("Loading danmaku: {}", dm);
        match player.dispatch_command(Command::LoadDanmaku { path: dm.clone() }) {
            Ok(_) => println!("  Danmaku loaded!"),
            Err(e) => eprintln!("  Warning: {}", e),
        }
    }

    // 开始播放
    if let Err(e) = player.dispatch_command(Command::Play) {
        eprintln!("Failed to play: {}", e);
        std::process::exit(1);
    }

    println!();
    println!("Playing... 可用交互命令:");
    println!("  s <path>    加载字幕文件 (SRT/ASS)");
    println!("  t           切换字幕显示/隐藏");
    println!("  d <path>    加载弹幕文件 (B站XML/JSON)");
    println!("  b           切换弹幕显示/隐藏");
    println!("  o <0-100>   设置弹幕不透明度");
    println!("  p           暂停/继续");
    println!("  q           退出");
    println!("────────────────────────────────────────────");

    // stdin → channel 线程
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<UserCmd>();
    let input_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let input_done_clone = input_done.clone();

    std::thread::Builder::new()
        .name("cli-input".into())
        .spawn(move || {
            let stdin = io::stdin();
            let mut reader = stdin.lock();
            loop {
                if input_done_clone.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                print!("> ");
                let _ = io::stdout().flush();
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
                        let cmd = match parts[0] {
                            "s" if parts.len() > 1 => {
                                Some(UserCmd::LoadSubtitle(parts[1].trim().to_string()))
                            }
                            "t" => Some(UserCmd::ToggleSubtitle),
                            "d" if parts.len() > 1 => {
                                Some(UserCmd::LoadDanmaku(parts[1].trim().to_string()))
                            }
                            "b" => Some(UserCmd::ToggleDanmaku),
                            "o" if parts.len() > 1 => {
                                parts[1].trim().parse::<u8>().ok().map(|v| {
                                    UserCmd::SetDanmakuOpacity(v.min(100))
                                })
                            }
                            "p" => Some(UserCmd::TogglePause),
                            "q" => Some(UserCmd::Quit),
                            _ => {
                                eprintln!("  Unknown command: {}", parts[0]);
                                None
                            }
                        };
                        if let Some(c) = cmd {
                            let is_quit = matches!(c, UserCmd::Quit);
                            if cmd_tx.send(c).is_err() {
                                break;
                            }
                            if is_quit {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .expect("spawn input thread");

    // 主循环：处理事件 + 处理用户命令
    let mut last_hud_print = std::time::Instant::now();
    loop {
        // 每 2 秒输出一次 HUD 信息到控制台
        if last_hud_print.elapsed() >= std::time::Duration::from_secs(2) {
            last_hud_print = std::time::Instant::now();
            let stats = player.get_hud_stats();
            let status = if stats.paused { "PAUSE" } else { "PLAY " };
            println!(
                "[HUD] {} | {:.1}/{:.0}s | 字幕:{} | 弹幕:{} ({}条)",
                status, stats.position_secs, stats.duration_secs,
                if stats.subtitle_on { "ON" } else { "OFF" },
                if stats.danmaku_on { "ON" } else { "OFF" },
                stats.danmaku_count,
            );
        }

        // 处理用户输入命令
        while let Ok(user_cmd) = cmd_rx.try_recv() {
            match user_cmd {
                UserCmd::LoadSubtitle(path) => {
                    println!("  Loading subtitle: {}", path);
                    match player.dispatch_command(Command::LoadSubtitle { path }) {
                        Ok(_) => println!("  Subtitle loaded!"),
                        Err(e) => eprintln!("  Error: {}", e),
                    }
                }
                UserCmd::ToggleSubtitle => {
                    match player.dispatch_command(Command::ToggleSubtitle) {
                        Ok(_) => println!("  Subtitle toggled"),
                        Err(e) => eprintln!("  Error: {}", e),
                    }
                }
                UserCmd::LoadDanmaku(path) => {
                    println!("  Loading danmaku: {}", path);
                    match player.dispatch_command(Command::LoadDanmaku { path }) {
                        Ok(_) => println!("  Danmaku loaded!"),
                        Err(e) => eprintln!("  Error: {}", e),
                    }
                }
                UserCmd::ToggleDanmaku => {
                    match player.dispatch_command(Command::ToggleDanmaku) {
                        Ok(_) => println!("  Danmaku toggled"),
                        Err(e) => eprintln!("  Error: {}", e),
                    }
                }
                UserCmd::SetDanmakuOpacity(val) => {
                    match player.dispatch_command(Command::SetDanmakuOpacity { opacity: val }) {
                        Ok(_) => println!("  Danmaku opacity → {}", val),
                        Err(e) => eprintln!("  Error: {}", e),
                    }
                }
                UserCmd::TogglePause => {
                    let _ = player.dispatch_command(Command::TogglePause);
                    println!("  Pause toggled");
                }
                UserCmd::Quit => {
                    let _ = player.dispatch_command(Command::Quit);
                    input_done.store(true, std::sync::atomic::Ordering::Release);
                    let _ = player.dispatch_command(Command::Stop);
                    println!("Playback finished.");
                    return;
                }
            }
        }

        // 处理播放事件
        if let Ok(event) = event_rx.try_recv() {
            match event {
                RaptorEvent::FileLoaded { duration, .. } => {
                    println!("[event] File loaded: {:.2}s", duration);
                }
                RaptorEvent::EndFile { reason } => {
                    println!("[event] End of file: {:?}", reason);
                    break;
                }
                RaptorEvent::Error { code, message } => {
                    eprintln!("[event] Error ({}): {}", code, message);
                    break;
                }
                RaptorEvent::End => {
                    println!("[event] Player ended");
                    break;
                }
                _ => {
                    tracing::debug!("Event: {:?}", event);
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    input_done.store(true, std::sync::atomic::Ordering::Release);
    let _ = player.dispatch_command(Command::Stop);
    println!("Playback finished.");
}
