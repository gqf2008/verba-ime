//! C ABI 桥（供 Swift/ObjC IMK 薄壳经 dlopen/FFI 调用 Rust 引擎）。
//!
//! macOS 的 IMK 目前由 Swift/ObjC 薄壳承载；薄壳加载本 crate 的 cdylib（libverba_ime_macos.dylib），
//! 调用以下 `extern "C"` 函数把按键/文本交给 Rust 引擎（连接 daemon、请求提交）。

use std::sync::{Mutex, OnceLock};

use crate::MacIme;

static IME: OnceLock<Mutex<Option<MacIme>>> = OnceLock::new();

fn ime_state() -> &'static Mutex<Option<MacIme>> {
    IME.get_or_init(|| Mutex::new(None))
}

/// 连接 daemon；成功返回 1，失败 0。
#[no_mangle]
pub extern "C" fn verba_mac_connect() -> i32 {
    match MacIme::connect() {
        Ok(ime) => {
            *ime_state().lock().unwrap() = Some(ime);
            1
        }
        Err(_) => 0,
    }
}

/// 健康检查；成功返回 1，失败 0。
#[no_mangle]
pub extern "C" fn verba_mac_ping() -> i32 {
    let mut guard = ime_state().lock().unwrap();
    match guard.as_mut() {
        Some(ime) => match ime.ping() {
            Ok(_) => 1,
            Err(_) => 0,
        },
        None => 0,
    }
}

/// 请求提交文本（Swift 薄壳真正插入编辑上下文；此处仅记录）。
#[no_mangle]
pub extern "C" fn verba_mac_commit_text(text: *const u8, len: usize) {
    if text.is_null() || len == 0 {
        return;
    }
    // SAFETY: 调用方保证 text 指向 len 字节的 UTF-8 缓冲，且本次调用内有效。
    let bytes = unsafe { std::slice::from_raw_parts(text, len) };
    let s = String::from_utf8_lossy(bytes).into_owned();
    let guard = ime_state().lock().unwrap();
    if let Some(ime) = guard.as_ref() {
        ime.request_commit(&s);
    }
}
