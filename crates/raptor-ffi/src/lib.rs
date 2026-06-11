//! Raptor C ABI 导出层
//!
//! 所有 `#[no_mangle] pub extern "C"` 函数集中在此 crate。
//! Flutter/Dart 通过 dart:ffi 调用这些函数，以 JSON 字符串交换数据。

// FFI 边界函数接收 raw pointer 参数是 C ABI 要求，由调用方保证有效性
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::Arc;
use std::thread::JoinHandle;

use parking_lot::Mutex;
use raptor_core::{
    Command, CommandResult, DefaultPropertyStore, ErrorCode, MediaInfo, PlayerEvent, PlayerState,
    PropertyStore, PropertyValue, RaptorError, RaptorEvent, SeekMode,
};
use raptor_ffmpeg::{Demuxer, FfmpegDemuxer};
use raptor_pipeline::{Pipeline, PipelineHandles};
use raptor_render::{VideoOutput, WgpuRenderer};

/// 播放器核心 — 持有状态机 + pipeline + 属性存储
pub struct Player {
    state: Mutex<PlayerState>,
    properties: Arc<DefaultPropertyStore>,
    event_tx: tokio::sync::mpsc::UnboundedSender<RaptorEvent>,
    pipeline: Mutex<Option<Arc<Pipeline>>>,
    pipeline_handles: Mutex<Option<PipelineHandles>>,
    /// 位置上报后台线程（定期将 pipeline position 写入 property store）
    position_reporter: Mutex<Option<JoinHandle<()>>>,
}

impl Player {
    pub fn new(event_tx: tokio::sync::mpsc::UnboundedSender<RaptorEvent>) -> Self {
        let properties = Arc::new(DefaultPropertyStore::new(event_tx.clone()));
        Self {
            state: Mutex::new(PlayerState::Idle),
            properties,
            event_tx,
            pipeline: Mutex::new(None),
            pipeline_handles: Mutex::new(None),
            position_reporter: Mutex::new(None),
        }
    }

    /// 处理命令
    pub fn dispatch_command(&self, cmd: Command) -> raptor_core::Result<CommandResult> {
        match cmd {
            Command::LoadFile { url } => self.load_file(&url),
            Command::Play => self.play(),
            Command::Pause => self.pause(),
            Command::TogglePause => self.toggle_pause(),
            Command::Stop => self.stop(),
            Command::Seek { target, mode } => self.seek(target, mode),
            Command::SetVolume { volume } => {
                self.properties
                    .set("volume", PropertyValue::Int(volume as i64));
                // 同步传递到 pipeline 音频输出
                if let Some(pipeline) = self.pipeline.lock().as_ref() {
                    pipeline.set_volume(volume);
                }
                Ok(CommandResult::Empty)
            }
            Command::Quit => {
                self.stop_pipeline();
                let _ = self.event_tx.send(RaptorEvent::End);
                Ok(CommandResult::Empty)
            }
        }
    }

    fn load_file(&self, url: &str) -> raptor_core::Result<CommandResult> {
        let state = self.state.lock();
        state
            .can_transition_to(&PlayerEvent::Load {
                url: url.to_string(),
            })
            .map_err(RaptorError::InvalidState)?;

        // 先停止旧 pipeline
        drop(state);
        self.stop_pipeline();

        // 转为 Loading
        *self.state.lock() = PlayerState::Loading;

        // 打开文件获取信息
        let mut demuxer = FfmpegDemuxer::new();
        demuxer.open(url)?;
        let info = demuxer
            .info()
            .cloned()
            .ok_or_else(|| raptor_core::RaptorError::Demux("no media info".into()))?;

        let video_info = if info.video_stream_index.is_some() {
            Some(raptor_core::VideoInfo {
                width: info.width,
                height: info.height,
                codec: format!("{:?}", info.video_codec_id),
                fps: info.fps,
            })
        } else {
            None
        };

        let audio_info = if info.audio_stream_index.is_some() {
            Some(raptor_core::AudioInfo {
                codec: format!("{:?}", info.audio_codec_id),
                channels: info.channels,
                sample_rate: info.sample_rate,
            })
        } else {
            None
        };

        let duration = info.duration;

        // 创建 renderer
        let mut renderer = WgpuRenderer::new();
        if let Some(ref vi) = video_info {
            renderer.init(vi.width, vi.height)?;
        }

        // 创建 pipeline（暂停状态）— 将 demuxer 传递给 pipeline，避免重复打开文件
        let (crossbeam_tx, crossbeam_rx) = crossbeam_channel::bounded(64);
        let mut pipeline = Pipeline::new(crossbeam_tx);
        pipeline.pause(); // 创建后先暂停，等 Play 命令再恢复
        tracing::info!("load_file: calling pipeline.start()...");
        match pipeline.start(
            url,
            Box::new(demuxer),
            Box::new(renderer),
            duration,
            video_info.clone(),
            audio_info.clone(),
        ) {
            Ok(()) => tracing::info!("load_file: pipeline.start() returned Ok"),
            Err(e) => {
                tracing::error!("load_file: pipeline.start() FAILED: {}", e);
                return Err(e);
            }
        }
        let pipeline = Arc::new(pipeline);

        *self.pipeline.lock() = Some(pipeline.clone());

        // 启动位置上报线程
        self.start_position_reporter(pipeline.clone());

        // 转发 crossbeam 事件到 tokio channel
        let event_tx = self.event_tx.clone();
        std::thread::Builder::new()
            .name("raptor-evt-fwd".into())
            .spawn(move || {
                while let Ok(event) = crossbeam_rx.recv() {
                    if event_tx.send(event).is_err() {
                        break;
                    }
                }
            })
            .ok();

        // 状态转换 → Ready
        *self.state.lock() = PlayerState::Ready;

        // 初始化 position 属性
        self.properties.set("position", PropertyValue::Float(0.0));

        // 发送 FileLoaded 事件
        let _ = self.event_tx.send(RaptorEvent::FileLoaded {
            duration,
            video: video_info,
            audio: audio_info,
        });

        tracing::info!("load_file: ready, duration={:.2}s", duration);
        Ok(CommandResult::MediaInfo(MediaInfo {
            duration,
            video: None,
            audio: None,
        }))
    }

    fn play(&self) -> raptor_core::Result<CommandResult> {
        let state = self.state.lock();
        state
            .can_transition_to(&PlayerEvent::Play)
            .map_err(RaptorError::InvalidState)?;
        drop(state);

        // 恢复 pipeline
        if let Some(pipeline) = self.pipeline.lock().as_ref() {
            pipeline.resume();
        }

        *self.state.lock() = PlayerState::Playing;
        let _ = self.event_tx.send(RaptorEvent::PlaybackRestart);
        Ok(CommandResult::Empty)
    }

    fn pause(&self) -> raptor_core::Result<CommandResult> {
        let state = self.state.lock();
        state
            .can_transition_to(&PlayerEvent::Pause)
            .map_err(RaptorError::InvalidState)?;
        drop(state);

        // 读取当前位置
        let pos = self
            .pipeline
            .lock()
            .as_ref()
            .map(|p| p.current_position_secs())
            .unwrap_or(0.0);

        // 暂停 pipeline
        if let Some(pipeline) = self.pipeline.lock().as_ref() {
            pipeline.pause();
        }

        *self.state.lock() = PlayerState::Paused;
        self.properties.set("position", PropertyValue::Float(pos));
        Ok(CommandResult::Empty)
    }

    fn toggle_pause(&self) -> raptor_core::Result<CommandResult> {
        let state = self.state.lock().clone();
        match state {
            PlayerState::Playing => self.pause(),
            PlayerState::Paused => self.play(),
            _ => Err(raptor_core::RaptorError::InvalidState(format!(
                "cannot toggle from {:?}",
                state
            ))),
        }
    }

    fn stop(&self) -> raptor_core::Result<CommandResult> {
        let state = self.state.lock();
        state
            .can_transition_to(&PlayerEvent::Stop)
            .map_err(RaptorError::InvalidState)?;
        drop(state);

        self.stop_pipeline();
        *self.state.lock() = PlayerState::Stopped;
        Ok(CommandResult::Empty)
    }

    fn seek(&self, target: f64, _mode: SeekMode) -> raptor_core::Result<CommandResult> {
        // 状态检查：只有 Playing 和 Paused 可以 seek
        let state = self.state.lock();
        state
            .can_transition_to(&PlayerEvent::Seek {
                target,
                mode: SeekMode::Absolute,
            })
            .map_err(RaptorError::InvalidState)?;
        drop(state);

        let pos = self
            .pipeline
            .lock()
            .as_ref()
            .map(|p| p.current_position_secs())
            .unwrap_or(0.0);

        if let Some(pipeline) = self.pipeline.lock().as_ref() {
            *pipeline.seek_request.lock() =
                Some(raptor_pipeline::pipeline::SeekRequest { target, from: pos });
        }

        let _ = self.event_tx.send(RaptorEvent::Seek {
            from: pos,
            to: target,
        });
        Ok(CommandResult::Empty)
    }

    fn stop_pipeline(&self) {
        // 停止位置上报
        if let Some(handle) = self.position_reporter.lock().take() {
            let _ = handle.join();
        }

        if let Some(pipeline) = self.pipeline.lock().take() {
            // 需要获取可变引用来调用 stop
            if let Some(p) = Arc::into_inner(pipeline) {
                let mut p = p;
                p.stop();
            }
        }
        *self.pipeline_handles.lock() = None;
    }

    fn start_position_reporter(&self, pipeline: Arc<Pipeline>) {
        let properties = self.properties.clone();
        let handle = std::thread::Builder::new()
            .name("raptor-pos-rpt".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(200));

                    // 如果 pipeline 已停止，退出
                    if pipeline.shutdown.load(std::sync::atomic::Ordering::Acquire) {
                        break;
                    }

                    // 仅在非暂停状态更新
                    if !pipeline.is_paused() {
                        let pos = pipeline.current_position_secs();
                        properties.set("position", PropertyValue::Float(pos));
                    }
                }
            })
            .expect("spawn position reporter");

        *self.position_reporter.lock() = Some(handle);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop_pipeline();
    }
}

// ═══════════════════════════════════════════════════
// FFI Handle
// ═══════════════════════════════════════════════════

/// 不透明句柄 — Dart 侧只看到指针
pub struct RaptorHandle {
    player: Player,
    event_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<RaptorEvent>>>,
    callback_thread: Mutex<Option<JoinHandle<()>>>,
    observer_ids: Mutex<Vec<i64>>,
    last_error: Mutex<Option<String>>,
}

// ═══════════════════════════════════════════════════
// C ABI Exports
// ═══════════════════════════════════════════════════

/// 创建播放器实例
#[no_mangle]
pub extern "C" fn raptor_create() -> *mut RaptorHandle {
    // 初始化日志
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let player = Player::new(event_tx);

    Box::into_raw(Box::new(RaptorHandle {
        player,
        event_rx: Mutex::new(Some(event_rx)),
        callback_thread: Mutex::new(None),
        observer_ids: Mutex::new(Vec::new()),
        last_error: Mutex::new(None),
    }))
}

/// 销毁播放器实例
#[no_mangle]
pub extern "C" fn raptor_destroy(handle: *mut RaptorHandle) {
    if handle.is_null() {
        return;
    }

    let handle = unsafe { Box::from_raw(handle) };

    // 发送 End 事件通知回调线程退出
    let _ = handle.player.event_tx.send(RaptorEvent::End);

    // 等待回调线程
    let thread = handle.callback_thread.lock().take();
    if let Some(t) = thread {
        let _ = t.join();
    }
}

/// 发送命令（JSON 格式）
#[no_mangle]
pub extern "C" fn raptor_command(handle: *mut RaptorHandle, cmd_json: *const c_char) -> c_int {
    if handle.is_null() || cmd_json.is_null() {
        return ErrorCode::InvalidArgument as c_int;
    }

    let handle = unsafe { &*handle };
    let cmd_str = match unsafe { CStr::from_ptr(cmd_json) }.to_str() {
        Ok(s) => s,
        Err(_) => return ErrorCode::InvalidArgument as c_int,
    };

    let cmd: Command = match serde_json::from_str(cmd_str) {
        Ok(c) => c,
        Err(e) => {
            *handle.last_error.lock() = Some(format!("parse command: {}", e));
            return ErrorCode::InvalidArgument as c_int;
        }
    };

    match handle.player.dispatch_command(cmd) {
        Ok(_) => ErrorCode::Ok as c_int,
        Err(e) => {
            *handle.last_error.lock() = Some(format!("{}", e));
            e.error_code() as c_int
        }
    }
}

/// 读取属性（返回 JSON 字符串，调用者需调用 raptor_free_string 释放）
#[no_mangle]
pub extern "C" fn raptor_get_property(
    handle: *mut RaptorHandle,
    name: *const c_char,
) -> *mut c_char {
    if handle.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }

    let handle = unsafe { &*handle };
    let name = match unsafe { CStr::from_ptr(name) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };

    match handle.player.properties.get(name) {
        Some(val) => {
            let json = val.to_json();
            match CString::new(json) {
                Ok(cstr) => cstr.into_raw(),
                Err(_) => std::ptr::null_mut(),
            }
        }
        None => std::ptr::null_mut(),
    }
}

/// 设置属性（JSON 格式）
#[no_mangle]
pub extern "C" fn raptor_set_property(
    handle: *mut RaptorHandle,
    name: *const c_char,
    value_json: *const c_char,
) -> c_int {
    if handle.is_null() || name.is_null() || value_json.is_null() {
        return ErrorCode::InvalidArgument as c_int;
    }

    let handle = unsafe { &*handle };
    let name = match unsafe { CStr::from_ptr(name) }.to_str() {
        Ok(s) => s,
        Err(_) => return ErrorCode::InvalidArgument as c_int,
    };
    let value_str = match unsafe { CStr::from_ptr(value_json) }.to_str() {
        Ok(s) => s,
        Err(_) => return ErrorCode::InvalidArgument as c_int,
    };

    // 尝试解析 JSON 值
    let value: PropertyValue = if let Ok(n) = value_str.parse::<i64>() {
        PropertyValue::Int(n)
    } else if let Ok(f) = value_str.parse::<f64>() {
        PropertyValue::Float(f)
    } else if value_str == "true" {
        PropertyValue::Bool(true)
    } else if value_str == "false" {
        PropertyValue::Bool(false)
    } else {
        // 当作字符串（去掉引号）
        let s = value_str.trim_matches('"').to_string();
        PropertyValue::String(s)
    };

    handle.player.properties.set(name, value);
    ErrorCode::Ok as c_int
}

// === Property Observer ===

/// 属性观察者回调函数类型
pub type RaptorPropertyCallback = extern "C" fn(*const c_char, *mut c_void);

/// 订阅属性变化
///
/// 当指定属性发生变化时，调用 C 函数指针回调。
/// 返回观察者 ID（>= 0），失败返回 -1。
/// 回调参数：(value_json: *const c_char, user_data: *mut c_void)
#[no_mangle]
pub extern "C" fn raptor_observe_property(
    handle: *mut RaptorHandle,
    name: *const c_char,
    callback: Option<RaptorPropertyCallback>,
    user_data: *mut c_void,
) -> i64 {
    if handle.is_null() || name.is_null() {
        return -1;
    }

    let Some(cb) = callback else {
        return -1;
    };

    let handle = unsafe { &*handle };
    let name = match unsafe { CStr::from_ptr(name) }.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };

    // 将 user_data 转为 usize（Send-safe）
    let user_data_addr = user_data as usize;

    let observer_id = handle.player.properties.observe(
        name,
        Arc::new(move |value: &PropertyValue| {
            let json = value.to_json();
            if let Ok(cstr) = CString::new(json) {
                let ud = user_data_addr as *mut c_void;
                cb(cstr.as_ptr(), ud);
            }
        }),
    );

    handle.observer_ids.lock().push(observer_id);
    observer_id
}

/// 取消属性订阅
///
/// 传入 `raptor_observe_property` 返回的 observer ID。
/// 成功返回 0，失败返回负数错误码。
#[no_mangle]
pub extern "C" fn raptor_unobserve_property(handle: *mut RaptorHandle, observer_id: i64) -> c_int {
    if handle.is_null() {
        return ErrorCode::InvalidArgument as c_int;
    }

    let handle = unsafe { &*handle };
    // 遍历所有已知的属性名来取消订阅（简化实现）
    // 实际使用中应该存储 observer_id → property_name 的映射
    handle.observer_ids.lock().retain(|&id| id != observer_id);
    ErrorCode::Ok as c_int
}

// === 事件 ===

/// 事件回调函数类型
pub type RaptorEventCallback = extern "C" fn(*const c_char, *mut c_void);

/// 设置事件回调
///
/// 注册后启动后台线程，从事件通道读取事件并调用回调函数。
/// 注意：设置回调后 `raptor_poll_event` 将不再可用（event_rx 已被回调线程接管）。
/// 传入 `None` 作为 callback 不会取消回调（需要销毁实例）。
#[no_mangle]
pub extern "C" fn raptor_set_event_callback(
    handle: *mut RaptorHandle,
    callback: Option<RaptorEventCallback>,
    user_data: *mut c_void,
) {
    if handle.is_null() {
        return;
    }
    let handle = unsafe { &*handle };

    let Some(cb) = callback else { return };

    // 取出 event_rx（只能调用一次，后续 poll_event 不可用）
    let event_rx = handle.event_rx.lock().take();
    let Some(mut rx) = event_rx else {
        tracing::warn!("raptor_set_event_callback: event_rx already taken by previous callback");
        return;
    };

    // 将 user_data 指针转为 usize（Send-safe），线程内再转回
    let user_data_addr = user_data as usize;

    let thread = std::thread::Builder::new()
        .name("raptor-evt-cb".into())
        .spawn(move || {
            while let Some(event) = rx.blocking_recv() {
                let is_end = matches!(event, RaptorEvent::End);
                match serde_json::to_string(&event) {
                    Ok(json) => {
                        if let Ok(cstr) = CString::new(json) {
                            let ud = user_data_addr as *mut c_void;
                            cb(cstr.as_ptr(), ud);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("event serialize error: {}", e);
                    }
                }
                if is_end {
                    break;
                }
            }
        })
        .expect("spawn event callback thread");

    *handle.callback_thread.lock() = Some(thread);
}

/// 轮询事件（非阻塞，返回 JSON 字符串或 null）
///
/// 注意：如果已调用 `raptor_set_event_callback`，此函数将始终返回 null（event_rx 已被回调线程接管）。
#[no_mangle]
pub extern "C" fn raptor_poll_event(handle: *mut RaptorHandle) -> *mut c_char {
    if handle.is_null() {
        return std::ptr::null_mut();
    }

    let handle = unsafe { &*handle };
    let mut guard = handle.event_rx.lock();
    let Some(ref mut rx) = *guard else {
        return std::ptr::null_mut();
    };

    match rx.try_recv() {
        Ok(event) => match serde_json::to_string(&event) {
            Ok(json) => match CString::new(json) {
                Ok(cstr) => cstr.into_raw(),
                Err(_) => std::ptr::null_mut(),
            },
            Err(_) => std::ptr::null_mut(),
        },
        Err(_) => std::ptr::null_mut(),
    }
}

/// 获取 GPU 纹理 ID（供 Flutter Texture Widget 使用）
#[no_mangle]
pub extern "C" fn raptor_get_texture_id(handle: *mut RaptorHandle) -> i64 {
    if handle.is_null() {
        return -1;
    }
    // TODO: 从 renderer 获取实际纹理 ID
    -1
}

/// 设置平台原生渲染器
#[no_mangle]
pub extern "C" fn raptor_set_renderer(handle: *mut RaptorHandle, _renderer: *mut c_void) {
    if handle.is_null() {
        // TODO: 集成平台原生渲染器
    }
}

/// 释放由 raptor 分配的字符串
#[no_mangle]
pub extern "C" fn raptor_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            let _ = CString::from_raw(s);
        }
    }
}

/// 获取最近的错误信息（返回 JSON 字符串或 null）
#[no_mangle]
pub extern "C" fn raptor_last_error(handle: *mut RaptorHandle) -> *mut c_char {
    if handle.is_null() {
        return std::ptr::null_mut();
    }

    let handle = unsafe { &*handle };
    let err = handle.last_error.lock().take();
    match err {
        Some(msg) => match CString::new(msg) {
            Ok(cstr) => cstr.into_raw(),
            Err(_) => std::ptr::null_mut(),
        },
        None => std::ptr::null_mut(),
    }
}
