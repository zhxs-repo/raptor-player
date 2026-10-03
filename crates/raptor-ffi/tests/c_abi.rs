//! C ABI 边界层测试 — 直接调用 `#[no_mangle] extern "C"` 导出函数
//!
//! 覆盖评审报告指出的零测试面：句柄所有权、null/畸形参数、错误码、
//! UTF-8 字符串、观察者/事件回调、Surface 管理入口。
//! 全部不依赖 GPU/媒体文件（LoadFile 只走失败路径）。

use raptor_core::{Command, ErrorCode};
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::atomic::{AtomicUsize, Ordering};

// ═══════════════════════════════════════════════════
// 测试辅助
// ═══════════════════════════════════════════════════

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

/// 读取 FFI 返回的堆字符串并 `raptor_free_string` 释放；null → None
unsafe fn take_string(p: *mut c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = CStr::from_ptr(p).to_string_lossy().into_owned();
    raptor_ffi::raptor_free_string(p);
    Some(s)
}

fn cmd_json(cmd: &Command) -> CString {
    c(&serde_json::to_string(cmd).unwrap())
}

// ═══════════════════════════════════════════════════
// 1. null 参数安全矩阵 — 所有导出入口不得 panic
// ═══════════════════════════════════════════════════

#[test]
fn null_handle_and_args_never_panic() {
    let h = raptor_ffi::raptor_create();
    assert!(!h.is_null());
    let name = c("volume");
    let payload = c("80");

    // 返回错误码的入口
    assert_eq!(
        raptor_ffi::raptor_command(std::ptr::null_mut(), c("{}").as_ptr()),
        ErrorCode::InvalidArgument as i32
    );
    assert_eq!(
        raptor_ffi::raptor_command(h, std::ptr::null()),
        ErrorCode::InvalidArgument as i32
    );
    assert_eq!(
        raptor_ffi::raptor_set_property(std::ptr::null_mut(), name.as_ptr(), payload.as_ptr()),
        ErrorCode::InvalidArgument as i32
    );
    assert_eq!(
        raptor_ffi::raptor_set_property(h, std::ptr::null(), payload.as_ptr()),
        ErrorCode::InvalidArgument as i32
    );
    assert_eq!(
        raptor_ffi::raptor_unobserve_property(std::ptr::null_mut(), 1),
        ErrorCode::InvalidArgument as i32
    );
    assert_eq!(
        raptor_ffi::raptor_set_surface(std::ptr::null_mut(), 0, 0, 1, 1),
        ErrorCode::InvalidArgument as i32
    );
    assert_eq!(
        raptor_ffi::raptor_detach_surface(std::ptr::null_mut()),
        ErrorCode::InvalidArgument as i32
    );
    assert_eq!(
        raptor_ffi::raptor_resize_surface(std::ptr::null_mut(), 1, 1),
        ErrorCode::InvalidArgument as i32
    );

    // 返回指针/-1 的入口
    assert!(raptor_ffi::raptor_get_property(std::ptr::null_mut(), name.as_ptr()).is_null());
    assert!(raptor_ffi::raptor_get_property(h, std::ptr::null()).is_null());
    assert!(raptor_ffi::raptor_poll_event(std::ptr::null_mut()).is_null());
    assert!(raptor_ffi::raptor_last_error(std::ptr::null_mut()).is_null());
    assert_eq!(
        raptor_ffi::raptor_observe_property(
            std::ptr::null_mut(),
            name.as_ptr(),
            None,
            std::ptr::null_mut()
        ),
        -1
    );
    assert_eq!(
        raptor_ffi::raptor_observe_property(h, std::ptr::null(), None, std::ptr::null_mut()),
        -1
    );
    assert_eq!(raptor_ffi::raptor_get_texture_id(std::ptr::null_mut()), -1);

    // void 入口仅要求不 panic（这些导出函数在 Rust 侧调用无需 unsafe）
    raptor_ffi::raptor_set_event_callback(std::ptr::null_mut(), None, std::ptr::null_mut());
    raptor_ffi::raptor_set_renderer(std::ptr::null_mut(), std::ptr::null_mut());
    raptor_ffi::raptor_free_string(std::ptr::null_mut());
    raptor_ffi::raptor_destroy(std::ptr::null_mut()); // 销毁 null 句柄应为 no-op

    raptor_ffi::raptor_destroy(h);
}

// ═══════════════════════════════════════════════════
// 2. 句柄所有权
// ═══════════════════════════════════════════════════

#[test]
fn create_destroy_roundtrip_multiple_handles() {
    let a = raptor_ffi::raptor_create();
    let b = raptor_ffi::raptor_create();
    assert!(!a.is_null() && !b.is_null() && a != b);

    // 各自独立可用
    assert_eq!(raptor_ffi::raptor_get_texture_id(a), -1);
    assert_eq!(raptor_ffi::raptor_get_texture_id(b), -1);

    raptor_ffi::raptor_destroy(a);
    raptor_ffi::raptor_destroy(b);
}

// ═══════════════════════════════════════════════════
// 3. 畸形 JSON 与错误码
// ═══════════════════════════════════════════════════

#[test]
fn malformed_json_returns_invalid_argument_and_records_error() {
    let h = raptor_ffi::raptor_create();

    assert_eq!(
        raptor_ffi::raptor_command(h, c("not json at all").as_ptr()),
        ErrorCode::InvalidArgument as i32
    );
    // 合法 JSON 但非法 command 形状
    assert_eq!(
        raptor_ffi::raptor_command(h, c("{\"Play\":{\"bogus\":1}}").as_ptr()),
        ErrorCode::InvalidArgument as i32
    );
    assert_eq!(
        raptor_ffi::raptor_command(h, c("[1,2,3]").as_ptr()),
        ErrorCode::InvalidArgument as i32
    );

    // last_error 已写入且取后即清空（take 语义）
    let err = unsafe { take_string(raptor_ffi::raptor_last_error(h)) };
    assert!(err.is_some(), "last_error 应记录解析失败");
    assert!(err.unwrap().contains("parse command"));
    assert!(unsafe { take_string(raptor_ffi::raptor_last_error(h)) }.is_none());

    raptor_ffi::raptor_destroy(h);
}

#[test]
fn invalid_utf8_pointer_returns_invalid_argument() {
    let h = raptor_ffi::raptor_create();
    // 0xFF 不是合法 UTF-8 起始字节；以 NUL 结尾
    let bytes: [u8; 2] = [0xFF, 0x00];
    assert_eq!(
        raptor_ffi::raptor_command(h, bytes.as_ptr() as *const c_char),
        ErrorCode::InvalidArgument as i32
    );
    raptor_ffi::raptor_destroy(h);
}

#[test]
fn invalid_state_command_maps_to_invalid_state_code() {
    let h = raptor_ffi::raptor_create();
    // Idle 状态不允许 Play
    let code = raptor_ffi::raptor_command(h, cmd_json(&Command::Play).as_ptr());
    assert_eq!(code, ErrorCode::InvalidState as i32);
    let err = unsafe { take_string(raptor_ffi::raptor_last_error(h)) };
    assert!(err.unwrap().to_lowercase().contains("state"));
    raptor_ffi::raptor_destroy(h);
}

// ═══════════════════════════════════════════════════
// 4. UTF-8 与 LoadFile 失败路径
// ═══════════════════════════════════════════════════

#[test]
fn load_file_missing_cjk_path_returns_file_not_found() {
    let h = raptor_ffi::raptor_create();
    let url = "没有这个文件_弹幕测试.mp4";
    let code =
        raptor_ffi::raptor_command(h, cmd_json(&Command::LoadFile { url: url.into() }).as_ptr());
    assert_eq!(code, ErrorCode::FileNotFound as i32);
    let err = unsafe { take_string(raptor_ffi::raptor_last_error(h)) }.expect("应有错误信息");
    assert!(err.contains(url), "错误信息应原样携带 UTF-8 路径: {err}");
    raptor_ffi::raptor_destroy(h);
}

#[test]
fn utf8_property_roundtrip_through_c_abi() {
    let h = raptor_ffi::raptor_create();
    let key = c("标题");
    // 传带引号的 JSON 字符串值 → 按 String 存储
    let value = c("\"中文值_with_emoji_🎬\"");
    assert_eq!(
        raptor_ffi::raptor_set_property(h, key.as_ptr(), value.as_ptr()),
        ErrorCode::Ok as i32
    );
    let got = unsafe { take_string(raptor_ffi::raptor_get_property(h, key.as_ptr())) };
    let got = got.expect("CJK 属性名应可读写");
    assert!(got.contains("中文值"), "get 应返回存入的 UTF-8 值: {got}");
    raptor_ffi::raptor_destroy(h);
}

#[test]
fn get_property_unknown_key_returns_null() {
    let h = raptor_ffi::raptor_create();
    assert!(raptor_ffi::raptor_get_property(h, c("不存在").as_ptr()).is_null());
    raptor_ffi::raptor_destroy(h);
}

// ═══════════════════════════════════════════════════
// 5. 属性观察者回调
// ═══════════════════════════════════════════════════

extern "C" fn record_property_cb(json: *const c_char, user: *mut c_void) {
    assert!(!json.is_null());
    let s = unsafe { CStr::from_ptr(json) }
        .to_string_lossy()
        .into_owned();
    let log = unsafe { &*(user as *const parking_lot::Mutex<Vec<String>>) };
    log.lock().push(s);
}

#[test]
fn observe_property_fires_callback_and_unobserve_stops() {
    let h = raptor_ffi::raptor_create();
    let log = Box::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let user = Box::into_raw(log) as *mut c_void; // 所有权交给测试，结束前手动回收

    let name = c("volume");
    let id = raptor_ffi::raptor_observe_property(h, name.as_ptr(), Some(record_property_cb), user);
    assert!(id >= 0, "observe 应返回非负 id");

    assert_eq!(
        raptor_ffi::raptor_set_property(h, name.as_ptr(), c("80").as_ptr()),
        ErrorCode::Ok as i32
    );
    {
        let entries = unsafe { &*(user as *const parking_lot::Mutex<Vec<String>>) }.lock();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], "80"); // Int 的 JSON 形式
    }

    assert_eq!(
        raptor_ffi::raptor_unobserve_property(h, id),
        ErrorCode::Ok as i32
    );
    raptor_ffi::raptor_set_property(h, name.as_ptr(), c("81").as_ptr());
    {
        let entries = unsafe { &*(user as *const parking_lot::Mutex<Vec<String>>) }.lock();
        assert_eq!(entries.len(), 1, "unobserve 后不应再触发");
    }

    raptor_ffi::raptor_destroy(h);
    unsafe { drop(Box::from_raw(user as *mut parking_lot::Mutex<Vec<String>>)) };
}

// ═══════════════════════════════════════════════════
// 6. 事件回调与 poll_event
// ═══════════════════════════════════════════════════

static END_EVENT_COUNT: AtomicUsize = AtomicUsize::new(0);

extern "C" fn count_event_cb(json: *const c_char, _user: *mut c_void) {
    assert!(!json.is_null());
    END_EVENT_COUNT.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn poll_event_receives_end_after_quit_command() {
    let h = raptor_ffi::raptor_create();
    assert!(
        raptor_ffi::raptor_poll_event(h).is_null(),
        "无事件时应返回 null"
    );

    assert_eq!(
        raptor_ffi::raptor_command(h, cmd_json(&Command::Quit).as_ptr()),
        ErrorCode::Ok as i32
    );
    let evt =
        unsafe { take_string(raptor_ffi::raptor_poll_event(h)) }.expect("Quit 应产生 End 事件");
    assert_eq!(evt, "\"End\"");
    assert!(raptor_ffi::raptor_poll_event(h).is_null(), "事件只送达一次");

    raptor_ffi::raptor_destroy(h);
}

#[test]
fn event_callback_takes_over_and_receives_end_on_destroy() {
    let before = END_EVENT_COUNT.load(Ordering::SeqCst);
    let h = raptor_ffi::raptor_create();
    raptor_ffi::raptor_set_event_callback(h, Some(count_event_cb), std::ptr::null_mut());
    // 回调接管后 poll_event 永久返回 null（文档化行为）
    raptor_ffi::raptor_command(h, cmd_json(&Command::Quit).as_ptr());
    assert!(raptor_ffi::raptor_poll_event(h).is_null());

    // destroy 会发送 End 并 join 回调线程 → 返回时计数必然已增加
    raptor_ffi::raptor_destroy(h);
    assert!(
        END_EVENT_COUNT.load(Ordering::SeqCst) > before,
        "事件回调线程应至少收到一次回调"
    );
}

// ═══════════════════════════════════════════════════
// 7. Surface 管理入口（无 pipeline 的桌面场景）
// ═══════════════════════════════════════════════════

#[test]
fn surface_commands_without_pipeline_succeed_as_noop() {
    let h = raptor_ffi::raptor_create();
    // load_file 之前：set_surface 仅暂存 pending，不触碰 GPU
    assert_eq!(
        raptor_ffi::raptor_set_surface(h, 0x1234, 0, 640, 360),
        ErrorCode::Ok as i32
    );
    assert_eq!(raptor_ffi::raptor_detach_surface(h), ErrorCode::Ok as i32);
    assert_eq!(
        raptor_ffi::raptor_resize_surface(h, 1280, 720),
        ErrorCode::Ok as i32
    );
    // 带着 pending surface 直接销毁不得 panic
    raptor_ffi::raptor_destroy(h);
}
