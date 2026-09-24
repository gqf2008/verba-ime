//! 悬浮按钮窗的「非激活」平台修饰：点击按钮不抢编辑器焦点。
//!
//! 这是本 crate 唯一允许 unsafe 的模块（workspace 级 `forbid(unsafe_code)`
//! 在此按模块放开）：三个平台的「点击不激活」都没有安全的 winit 抽象，
//! 只允许出现在库内部后端（issue #82 的跨平台约束）。
//!
//! - macOS：`NSWindowStyleMask::NonactivatingPanel`（styleMask 位），
//!   配合事件循环的 Accessory 激活策略，点击不激活 helper 应用、
//!   不带走编辑器焦点。
//! - Windows：`WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` 扩展样式：
//!   鼠标消息正常投递但不激活本窗；TOOLWINDOW 让 Alt-Tab 与任务栏
//!   忽略 helper 窗。
//! - X11/Linux：无本阶段修饰——`override_redirect` 在窗口创建属性上
//!   设置（float.rs），WM 不参与管理即不焦点抢占。
//! - 其余平台（Wayland 等）：no-op 并告警（窗口表现为普通置顶小窗，
//!   焦点行为以平台为准，见 docs/architecture.md 已知限制）。

use winit::window::Window;

/// 让 `window` 接收鼠标点击但不抢夺键盘焦点。
pub(crate) fn make_nonactivating(window: &Window) {
    make_nonactivating_impl(window);
}

#[cfg(target_os = "macos")]
fn make_nonactivating_impl(window: &Window) {
    use objc2_app_kit::{NSView, NSWindowStyleMask};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = match window.window_handle() {
        Ok(h) => h,
        Err(e) => {
            log::warn!("取窗口句柄失败，跳过非激活修饰: {e}");
            return;
        }
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        log::warn!("非 AppKit 窗口，跳过非激活修饰");
        return;
    };
    // SAFETY: ns_view 是 winit 刚创建的合法 NSView，所属 NSWindow 有效；
    // 本函数在 winit 事件循环所在主线程调用（resumed 内）。styleMask 位
    // 追加 NonactivatingPanel 只改变窗口激活行为，不变式由 AppKit 维护。
    let view = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
    let Some(ns_window) = view.window() else {
        log::warn!("NSView 无所属窗口，跳过非激活修饰");
        return;
    };
    let mask = ns_window.styleMask() | NSWindowStyleMask::NonactivatingPanel;
    ns_window.setStyleMask(mask);
}

#[cfg(target_os = "windows")]
fn make_nonactivating_impl(window: &Window) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = match window.window_handle() {
        Ok(h) => h,
        Err(e) => {
            log::warn!("取窗口句柄失败，跳过非激活修饰: {e}");
            return;
        }
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        log::warn!("非 Win32 窗口，跳过非激活修饰");
        return;
    };
    // SAFETY: hwnd 来自 winit 活动窗口，生命周期覆盖本函数调用；
    // GWL_EXSTYLE 读写只影响扩展样式位，窗口过程不变。NOACTIVATE 使
    // 鼠标点击不激活本窗（编辑器保持焦点），TOOLWINDOW 让系统任务切换
    // 忽略本窗。
    unsafe {
        let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            style | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize,
        );
    }
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(any(target_os = "android", target_os = "ios"))
))]
fn make_nonactivating_impl(_window: &Window) {
    // X11 override_redirect 在创建属性上设置（float.rs）；Wayland 无
    // 焦点抢占豁免路径，no-op 并提示（已知限制，docs/architecture.md）。
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        log::warn!("Wayland 下悬浮按钮焦点行为以合成器为准（已知限制）");
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(any(target_os = "android", target_os = "ios"))
    )
)))]
fn make_nonactivating_impl(_window: &Window) {
    log::warn!("当前平台无悬浮按钮非激活修饰实现（已知限制）");
}
