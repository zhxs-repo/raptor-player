use raptor_core::{Command, RaptorEvent};
use raptor_ffi::Player;

fn main() {
    // 启用 backtrace 方便调试
    std::env::set_var("RUST_BACKTRACE", "1");

    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("raptor=info".parse().unwrap()),
        )
        .init();

    // 自定义 panic hook：在 panic 时记录日志并包含 backtrace
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        eprintln!("\n=== PANIC: {} ===\n{}\n=== END PANIC ===\n", info, bt);
        default_hook(info);
    }));

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: cli_player <video_file>");
        std::process::exit(1);
    }

    let file_path = &args[1];
    println!("Raptor CLI Player");
    println!("Playing: {}", file_path);

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let player = Player::new(event_tx);

    // 加载文件
    if let Err(e) = player.dispatch_command(Command::LoadFile {
        url: file_path.clone(),
    }) {
        eprintln!("Failed to load file: {}", e);
        std::process::exit(1);
    }

    // 开始播放
    if let Err(e) = player.dispatch_command(Command::Play) {
        eprintln!("Failed to play: {}", e);
        std::process::exit(1);
    }

    println!("Playing... (close window or press Ctrl+C to stop)");

    // 事件循环
    loop {
        if let Ok(event) = event_rx.try_recv() {
            match event {
                RaptorEvent::FileLoaded { duration, .. } => {
                    println!("File loaded: {:.2}s", duration);
                }
                RaptorEvent::EndFile { reason } => {
                    println!("End of file: {:?}", reason);
                    break;
                }
                RaptorEvent::Error { code, message } => {
                    eprintln!("Error ({}): {}", code, message);
                    break;
                }
                RaptorEvent::End => {
                    println!("Player ended");
                    break;
                }
                _ => {
                    tracing::debug!("Event: {:?}", event);
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // 停止并清理
    let _ = player.dispatch_command(Command::Stop);
    println!("Playback finished.");
}
