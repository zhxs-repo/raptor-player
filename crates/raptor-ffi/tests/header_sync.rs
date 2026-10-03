//! 回归：`raptor.h` 必须与 `src/lib.rs` 的 `#[no_mangle] extern "C"` 导出保持一致
//!
//! 历史缺陷：新增 `raptor_set_surface`/`raptor_detach_surface`/`raptor_resize_surface`
//! 后没有重新生成头文件，C 侧声明长期缺失（cbindgen 的 `[export].include` 是白名单，
//! 漏名等于静默不导出）。此测试让"忘记跑 cbindgen"直接在 `cargo test` 里失败。

/// 收集 lib.rs 里所有导出的 C 函数名
fn ffi_exports(src: &str) -> Vec<&str> {
    const PREFIX: &str = "pub extern \"C\" fn ";
    src.lines()
        .filter_map(|line| line.strip_prefix(PREFIX))
        .filter_map(|rest| rest.split('(').next())
        .filter(|name| !name.is_empty())
        .collect()
}

#[test]
fn header_declares_every_ffi_export() {
    let src = include_str!("../src/lib.rs");
    let header = include_str!("../../../raptor.h");

    let exports = ffi_exports(src);
    assert!(
        exports.len() >= 16,
        "解析到的导出函数过少（{}），本测试的解析规则可能已失效",
        exports.len()
    );

    let missing: Vec<&str> = exports
        .iter()
        .filter(|name| !header.contains(&format!("{name}(")))
        .copied()
        .collect();
    assert!(
        missing.is_empty(),
        "raptor.h 缺少导出声明 {missing:?}，请重新生成：\
         cd crates/raptor-ffi && cbindgen --config cbindgen.toml --crate raptor-ffi --output ../../raptor.h"
    );
}

/// cbindgen 无法展开的类型会退化成 `struct Option_Xxx` 占位，C 侧根本没法调用
#[test]
fn header_has_no_opaque_option_placeholders() {
    let header = include_str!("../../../raptor.h");
    assert!(
        !header.contains("Option_"),
        "raptor.h 出现 cbindgen 占位类型（Option_*），回调参数需写成匿名函数指针"
    );
    assert!(
        header.contains("void (*callback)(const char*, void*)"),
        "raptor.h 的回调参数应为函数指针原型"
    );
}

#[test]
fn ffi_exports_are_parsed_from_no_mangle_signatures() {
    let sample = concat!(
        "pub extern \"C\" fn raptor_create() -> *mut RaptorHandle {\n",
        "    unsafe extern \"C\" fn inner() {}\n",
        "fn raptor_not_exported() {}\n",
    );
    assert_eq!(ffi_exports(sample), vec!["raptor_create"]);
}
