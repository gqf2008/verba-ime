//! macOS IMK 输入控制器（全 Rust：objc2 + objc2-input-method-kit）。
//!
//! 输入链路：`inputText:key:modifiers:client:` 收按键 → `verba-core` 组合状态机
//! （拼音组合 / `//` AI 模式）→ 上屏 / 标记文本 / 候选窗；LLM 流式经 daemon：
//! 工作线程把 `StreamEvent` 推入全局队列，主线程定时器排空喂给状态机。

#![cfg(target_os = "macos")]

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, Once, OnceLock};

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFTypeRef, TCFType};
use core_foundation::dictionary::{CFDictionary, CFDictionaryGetValue, CFDictionaryRef};
use core_foundation::number::{CFNumber, CFNumberRef};
use core_foundation::string::{CFString, CFStringRef};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{
    define_class, msg_send, sel, AnyThread, ClassType, DefinedClass, MainThreadMarker,
    MainThreadOnly,
};
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventModifierFlags, NSFont, NSFontAttributeName, NSMenu, NSMenuItem,
    NSScreen,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSBundle, NSDefaultRunLoopMode, NSDictionary, NSInteger,
    NSNotFound, NSNumber, NSObject, NSObjectProtocol, NSRange, NSRect, NSRunLoop, NSString,
    NSTimer, NSUInteger,
};
use objc2_input_method_kit::{
    kIMKLocateCandidatesBelowHint, kIMKSingleColumnScrollingCandidatePanel, IMKCandidates,
    IMKCandidatesSendServerKeyEventFirst, IMKInputController, IMKServer, IMKStateSetting,
};

use verba_core::machine::{
    failure_overlay_body, result_hint, Action, CompositionMachine, LlmCandidateRequest,
    MachineState, PreviewKey, ResultPhase, PLACEHOLDER_RESULT_BODY, REWRITE_SYSTEM_PROMPT,
};
use verba_core::{parse_ai_command, AiCommand};
use verba_ipc::name::local_entropy_u64;
use verba_ipc::{CommitForwarder, LlmSession};
use verba_protos::{stream_event, StreamEvent};

use crate::ipc;

/// 文件日志管道（verba-mac 由 launchd 拉起、无控制台，stderr 不可见）。
struct LogPipe(std::sync::Mutex<std::fs::File>);
impl std::io::Write for LogPipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut f = self.0.lock().unwrap();
        f.write_all(buf)?;
        f.flush()?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 启动文件日志：落盘到用户数据目录 logs/verba-mac.log（与 verba-daemon.log
/// 同目录），超过 5MB 开机截断。排查输入链路问题与 `/usr/bin/log show`
/// 宿主侧日志配合使用。
fn init_file_logger() {
    let Ok(dirs) = verba_config::VerbaDirs::locate() else {
        return;
    };
    let _ = std::fs::create_dir_all(dirs.log_dir());
    let path = dirs.log_dir().join("verba-mac.log");
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 5 * 1024 * 1024 {
        let _ = std::fs::remove_file(&path);
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug"))
        .target(env_logger::Target::Pipe(Box::new(LogPipe(
            std::sync::Mutex::new(file),
        ))))
        .init();

    // panic 必须留痕：本进程由 launchd 拉起，stderr 无处可去，panic 的默认输出
    // 会整个丢失——而 Rust panic 一旦从 CoreFoundation 的 C++ 帧里往上展开，就会
    // 以 SIGABRT 收场（真机 2026-09-17 两次），那时**唯一**能定位根因的就只剩这条
    // 日志。默认 hook 照旧保留（stderr 那路丢就丢）。
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "?".to_owned());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_owned());
        log::error!("[Verba] RUST-PANIC at {loc}: {msg}");
        log::error!(
            "[Verba] RUST-PANIC backtrace:\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        default_hook(info);
    }));
}

/// 关键链路调试日志（激活/按键/候选回达/提交），走文件日志 debug 级。
/// crate 级可见：float_panel 的点击回调（ObjC 方法）也走同一管道。
pub(crate) fn dbg_log(msg: &str) {
    log::debug!("{msg}");
}

/// 用 @try 语义捕获 ObjC 异常：IMK 客户端包装器对部分选择器会抛
/// NSInvalidArgumentException（如已移除的 unmarkText），放任其穿透 Rust
/// 帧会在异常清理途中 abort（真机崩溃）。所有发送点返回值均为 ()，
/// 捕获后记录日志，不改变调用语义；host_call 以此为基座实现重入防护。
fn catch_void(label: &str, f: impl FnOnce()) {
    // 闭包捕获的 Retained<AnyObject> 非 UnwindSafe，此处仅用于诊断日志记录，
    // 异常路径不改动共享状态，AssertUnwindSafe 可接受。
    // 再包一层 catch_unwind：objc2 的异常 shim 是 C++ try/catch，Rust panic
    // 从它里面穿出时会在 __cxa_end_catch 里二次 panic（rust_drop_panic）→ abort。
    // 输入法的正确取舍是「放弃这一次宿主调用」，而不是「整个进程死掉」。
    let caught =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || match objc2::exception::catch(std::panic::AssertUnwindSafe(f)) {
                Ok(()) => {}
                Err(e) => {
                    let desc = e
                        .map(|x| x.to_string())
                        .unwrap_or_else(|| "unknown".to_owned());
                    log::warn!("[Verba] OBJC-EXC at {label}: {desc}");
                }
            },
        ));
    if caught.is_err() {
        log::error!("[Verba] RUST-PANIC swallowed at {label}（否则会穿出 ObjC 异常帧 abort）");
    }
}

/// 候选面板（IMK 私有 IMKUIPanel 窗口）外观校正：默认圆角过大（真机反馈）。
/// 面板窗口不在 IMKCandidates API 暴露范围，经 [NSApp windows] 找到后把
/// contentView 层的圆角收敛到 6pt 并裁剪。首个展示周期执行一次。
fn style_candidate_panel(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    for win in app.windows().iter() {
        // SAFETY: class 为 NSObject 方法，win 恒有效。
        let cls: *const AnyClass = unsafe { msg_send![&*win, class] };
        // SAFETY: cls 来自上一行对 win 的合法 msg_send（NSObject class），
        // 指向的元类在进程生命周期内恒有效。
        let name = unsafe { (*cls).name() }.to_string_lossy().into_owned();
        if name.contains("IMKUIPanel") {
            // SAFETY: contentView/setWantsLayer 为 NSView 公开方法，layer 经
            // msg_send 读取 CALayer 公开属性；win 是当前应用窗口列表中的存活对象，
            // 消息仅操作 layer 视觉参数。ObjC 异常由 catch_void 兜底。
            catch_void("panel.style", || unsafe {
                let Some(cv) = win.contentView() else { return };
                cv.setWantsLayer(true);
                let layer: Option<Retained<AnyObject>> = msg_send![&cv, layer];
                if let Some(layer) = layer {
                    let _: () = msg_send![&layer, setCornerRadius: 6.0f64];
                    let _: () = msg_send![&layer, setMasksToBounds: true];
                    log::debug!("[Verba] 候选面板圆角校正完成");
                }
            });
        }
    }
}

/// 现代 macOS 的 IMK 连接名约定：`<CFBundleIdentifier>_Connection`。
///
/// 旧式 `Verba_1_Connection` 会让 IMKLaunchAgent 拒绝 XPC endpoint（真机日志：
/// `requestIMKXPCEndpointInvalid`），输入源因此无法进入菜单/被选中。运行时从
/// 主 bundle 的 CFBundleIdentifier 派生，避免常量与 Info.plist 双源漂移。
fn connection_name_for_bundle_id(bundle_id: &str) -> String {
    format!("{bundle_id}_Connection")
}

/// 控制器 ObjC 类名（与 app/Info.plist 的 `InputMethodServerControllerClass` 保持一致）。
pub const CONTROLLER_CLASS: &str = "VerbaIMKController";
/// daemon 兼容的错误事件（无真实请求 id，序号匹配由全局 seq 完成）。
fn error_event(message: &str) -> StreamEvent {
    StreamEvent {
        id: 0,
        kind: Some(stream_event::Kind::Error(verba_protos::Error {
            code: 500,
            message: message.to_owned(),
        })),
    }
}

/// 读取 Rime 方案（`config.rime_schema`）。单引擎（Rime）下不再有引擎开关。
/// 读取失败时回退 `luna_pinyin_simp`。
fn load_rime_schema() -> String {
    let dirs = match verba_config::VerbaDirs::locate() {
        Ok(d) => d,
        Err(_) => return "luna_pinyin_simp".to_owned(),
    };
    let mgr = verba_config::ConfigManager::new(dirs);
    match mgr.load() {
        Ok(cfg) => cfg.rime_schema,
        Err(_) => "luna_pinyin_simp".to_owned(),
    }
}

/// LLM 流式事件队列项：`seq` 为全局唯一序号，规避不同客户端本地请求 id 冲突。
struct LlmItem {
    seq: u64,
    event: StreamEvent,
}

/// 重入窗口内到达的按键暂存项。`input_text`/`candidate_selected` 在
/// `host_call_depth > 0`（正处于 show()/commit() 等会同步泵运行循环的宿主
/// 调用段）时不得立即经 `apply_action` 对同一客户端发起嵌套调用——那正是
/// 真机崩溃的形态之一；键先入队，退栈后由 16ms 定时器补放（认领事件防
/// 宿主直插，无键丢失）。
enum PendingKey {
    Char(char),
    Paste(String),
    Backspace,
    Enter,
    Escape,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
}

fn llm_queue() -> &'static Mutex<VecDeque<LlmItem>> {
    static Q: OnceLock<Mutex<VecDeque<LlmItem>>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// 全局自增序号（从 1 起，0 表示「无活跃请求」）。序号全局唯一：事件带 seq，
/// 各控制器按自己的活跃 seq 消费，互不干扰（架构审查 P2-1 per-controller）。
static LLM_SEQ: AtomicU64 = AtomicU64::new(1);

/// 全局自增会话序号（从 1 起）：每个输入控制器独占一个 AI 多轮上下文会话，
/// daemon 按 session_id 分组隔离历史（架构审查会话维度 B4b）。
static SESSION_ID_SEQ: AtomicU64 = AtomicU64::new(1);

/// 窗口身份不可得时的一次性会话 token：每次强制刷新分配新值，不与其他窗口共享。
static SESSION_TOKEN_SEQ: AtomicU64 = AtomicU64::new(1);

/// 悬浮气泡 OCR 槽位的 session 代际（deactivate 递阶，见 float_ocr_slot）。
static FLOAT_OCR_EPOCH: AtomicU64 = AtomicU64::new(1);

/// 每进程随机盐（惰性生成一次）：daemon 是按用户单例、按 session_id 分组历史，
/// 而本 IME 进程可独立于 daemon 重启（崩溃/重装/系统回收）——重启后
/// SESSION_ID_SEQ 从 1 重排，会撞回 daemon 侧残留的历史槽并**继承**陈旧上下文
/// （与 Windows 端 process_salt 同源问题，复审 V4 对称修复；macOS 为单进程
/// 多控制器模型，无需防进程间碰撞，盐只为跨重启唯一性）。无 rand 依赖，
/// 同 Windows text_service.rs 的本地熵方案。
fn process_salt() -> u32 {
    static SALT: OnceLock<u32> = OnceLock::new();
    // 本地熵实现统一收敛到 verba-ipc name::local_entropy_u64（复用评审：
    // 原三处内联 xorshift 实现合一，便于审计与保持一致）。
    *SALT.get_or_init(|| (local_entropy_u64() >> 32) as u32)
}

/// 分配全局唯一的 AI 会话 id：高 32 位进程随机盐、低 32 位进程内自增序号，
/// IME 进程重启后不撞 daemon 侧残留历史槽。
fn alloc_session_id() -> u64 {
    let seq = SESSION_ID_SEQ.fetch_add(1, Ordering::SeqCst);
    ((process_salt() as u64) << 32) | (seq & 0xffff_ffff)
}

/// 组合 macOS 窗口级 session_key：PID + CGWindowNumber。
fn compose_macos_window_key(pid: i32, window_number: u32) -> String {
    format!("macos:{:08x}:{pid}:w{window_number}", process_salt())
}

/// 窗口身份不可得时的 input-session 级 fallback：每次强制刷新分配新 token，
/// 宁可丢掉多轮记忆，也绝不跨窗口共享。
fn compose_macos_fallback_key(token: u64) -> String {
    format!("macos:{:08x}:ephemeral:{token:x}", process_salt())
}

fn alloc_session_token() -> u64 {
    SESSION_TOKEN_SEQ.fetch_add(1, Ordering::SeqCst)
}

/// IMK client 的 windowNumber 路径（当前 _IPMDServerClientWrapperLegacy 通常
/// 没有该 selector，保留为兼容快路径）。
fn client_window_number(client: &AnyObject) -> Option<u32> {
    let responds: Bool = unsafe { msg_send![client, respondsToSelector: sel!(window)] };
    if !responds.as_bool() {
        return None;
    }
    let window: Option<Retained<AnyObject>> = unsafe { msg_send![client, window] };
    let window = window?;
    let responds_wn: Bool = unsafe { msg_send![&window, respondsToSelector: sel!(windowNumber)] };
    if !responds_wn.as_bool() {
        return None;
    }
    let n: NSInteger = unsafe { msg_send![&window, windowNumber] };
    (n > 0).then_some(n as u32)
}

fn client_process_identifier(client: &AnyObject) -> Option<i32> {
    let responds: Bool = unsafe { msg_send![client, respondsToSelector: sel!(processIdentifier)] };
    if !responds.as_bool() {
        return None;
    }
    let pid: i32 = unsafe { msg_send![client, processIdentifier] };
    (pid > 0).then_some(pid)
}

/// 由 firstRectForCharacterRange 的原始矩形得到插入点（Cocoa 底左坐标）。
///
/// 零长度 range 返回 width=0 的矩形是 NSTextView 的正常行为，必须接受；
/// width<0 表示 range 向左延伸，插入点仍取 origin.x。只拒绝 NSNotFound、
/// 非有限坐标与 <=0 的行高。
fn caret_point_from_rect(
    actual_location: NSUInteger,
    origin_x: f64,
    origin_y: f64,
    width: f64,
    height: f64,
) -> Option<(f64, f64)> {
    if actual_location == NSNotFound as NSUInteger
        || !origin_x.is_finite()
        || !origin_y.is_finite()
        || !height.is_finite()
        || height <= 0.0
    {
        return None;
    }
    let dx = if width.is_finite() {
        width.max(0.0)
    } else {
        0.0
    };
    Some((origin_x + dx, origin_y + height))
}

/// 原始光标矩形（Cocoa 底左坐标 x/y/w/h）；有效性校验与 caret_point_from_rect
/// 一致，额外保留 width 供 vision 取上/下边界。该调用会经 XPC 查宿主，必须在
/// host_call 重入保护内执行。
fn client_caret_rect(client: &AnyObject) -> Option<(f64, f64, f64, f64)> {
    let responds_range: Bool =
        unsafe { msg_send![client, respondsToSelector: sel!(selectedRange)] };
    if !responds_range.as_bool() {
        return None;
    }
    let range: NSRange = unsafe { msg_send![client, selectedRange] };
    let mut actual = NSRange::new(NSNotFound as NSUInteger, 0);
    let rect: NSRect = unsafe {
        msg_send![
            client,
            firstRectForCharacterRange: range,
            actualRange: &mut actual as *mut NSRange
        ]
    };
    let _ = caret_point_from_rect(
        actual.location,
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
    )?;
    Some((
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
    ))
}

fn client_caret_point(client: &AnyObject) -> Option<(f64, f64)> {
    let (x, y, w, h) = client_caret_rect(client)?;
    caret_point_from_rect(0, x, y, w, h)
}

// CGWindowListCopyWindowInfo 与 key 常量（CoreGraphics）。
#[repr(C)]
#[derive(Clone, Copy)]
struct CGPointF {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSizeF {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGRectF {
    origin: CGPointF,
    size: CGSizeF,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> CFArrayRef;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRectF;
    static kCGWindowNumber: CFStringRef;
    static kCGWindowLayer: CFStringRef;
    static kCGWindowBounds: CFStringRef;
    static kCGWindowOwnerPID: CFStringRef;
}

/// Cocoa 屏幕坐标（主屏左下原点）→ CGWindowBounds 坐标（主屏左上原点）。
fn cocoa_to_cg_point(point: (f64, f64), main_display_height: f64) -> (f64, f64) {
    (point.0, main_display_height - point.1)
}

/// CG 顶左气泡锚点（气泡矩形左上角）→ Cocoa 底左 frame 原点（复审 F1）：
/// `frame_y = 屏高 - cg_y - 边长`。与 `cocoa_to_cg_point` 互逆（按矩形
/// 而非点换算），NSWindow.setFrame 只吃后者。
fn cg_bubble_to_cocoa_frame((x, y): (i32, i32), screen_h: f64, size: f64) -> (f64, f64) {
    (x as f64, screen_h - y as f64 - size)
}

/// 由 CG 顶左坐标的光标行（top/bottom）与所在显示器工作区计算 vision
/// 截屏矩形。逐行镜像 Windows `fit_eye_rect`：优先光标上方（留 offset 间隙），
/// 上方放不下翻到下方，再不行贴空间更大的一侧；水平 clamp 到工作区。
fn vision_rect_from_caret(
    caret_x: i32,
    caret_top: i32,
    caret_bottom: i32,
    work: (i32, i32, i32, i32),
    width: i32,
    height: i32,
    offset_y: i32,
) -> (i32, i32, i32, i32) {
    let (work_left, work_top, work_right, work_bottom) = work;
    let w = width.max(64);
    let h = height.max(64);
    let px = caret_x.clamp(work_left, (work_right - w).max(work_left));
    let off = offset_y.max(0);
    let above_space = caret_top - off - work_top;
    let below_space = work_bottom - caret_bottom;
    let py = if h <= above_space {
        caret_top - off - h
    } else if h <= below_space {
        caret_bottom
    } else if above_space >= below_space {
        work_top
    } else {
        (work_bottom - h).max(work_top)
    };
    (px, py, w, h)
}

/// 光标所在显示器的真实工作区（NSScreen.visibleFrame，排除菜单栏/Dock），
/// 转成 CG 顶左坐标 (left, top, right, bottom)。
fn display_work_area_containing(
    cg_point: (f64, f64),
    main_height: f64,
) -> Option<(i32, i32, i32, i32)> {
    // SAFETY: IMK 回调在主线程执行（NSScreen 为 MainThreadOnly）。
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let screens = NSScreen::screens(mtm);
    for screen in screens.iter() {
        let frame = screen.frame();
        let left = frame.origin.x;
        let right = frame.origin.x + frame.size.width;
        let top = main_height - (frame.origin.y + frame.size.height);
        let bottom = main_height - frame.origin.y;
        if cg_point.0 >= left && cg_point.0 <= right && cg_point.1 >= top && cg_point.1 <= bottom {
            let visible = screen.visibleFrame();
            let v_top = main_height - (visible.origin.y + visible.size.height);
            let v_bottom = main_height - visible.origin.y;
            return Some((
                visible.origin.x.round() as i32,
                v_top.round() as i32,
                (visible.origin.x + visible.size.width).round() as i32,
                v_bottom.round() as i32,
            ));
        }
    }
    None
}

/// CGDisplayBounds → 工作区兜底（NSScreen 查询失败时；含菜单栏/Dock 的粗略值）。
fn cg_bounds_work(b: CGRectF) -> (i32, i32, i32, i32) {
    (
        b.origin.x.round() as i32,
        b.origin.y.round() as i32,
        (b.origin.x + b.size.width).round() as i32,
        (b.origin.y + b.size.height).round() as i32,
    )
}

fn cf_dict_f64(dict: &CFDictionary, key: &str) -> Option<f64> {
    let key = CFString::new(key);
    let raw = unsafe { CFDictionaryGetValue(dict.as_concrete_TypeRef(), key.as_CFTypeRef()) };
    if raw.is_null() {
        return None;
    }
    let number = unsafe { CFNumber::wrap_under_get_rule(raw as CFNumberRef) };
    number.to_f64()
}

fn bounds_contains(bounds: (f64, f64, f64, f64), point: (f64, f64)) -> bool {
    let (x, y, w, h) = bounds;
    point.0 >= x - 2.0 && point.0 <= x + w + 2.0 && point.1 >= y - 2.0 && point.1 <= y + h + 2.0
}

/// 用 PID + 光标矩形匹配当前窗口；若光标匹配失败但该 PID 只有一个
/// layer=0 窗口，则接受该唯一窗口。多窗口且无法匹配时返回 None。
fn cg_window_number_for_client(client: &AnyObject, pid: i32) -> Option<u32> {
    const ON_SCREEN_ONLY: u32 = 1 << 0;
    const EXCLUDE_DESKTOP: u32 = 1 << 4;
    let caret = client_caret_point(client);
    let raw = unsafe { CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0) };
    if raw.is_null() {
        return None;
    }
    let windows: CFArray<CFDictionary> = unsafe { TCFType::wrap_under_create_rule(raw) };
    let number_key = unsafe { CFString::wrap_under_get_rule(kCGWindowNumber) };
    let layer_key = unsafe { CFString::wrap_under_get_rule(kCGWindowLayer) };
    let bounds_key = unsafe { CFString::wrap_under_get_rule(kCGWindowBounds) };
    let pid_key = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerPID) };
    let screen_h = unsafe { CGDisplayBounds(CGMainDisplayID()).size.height };
    let mut candidates = Vec::new();
    for dict in windows.iter() {
        let raw_layer = unsafe {
            CFDictionaryGetValue(
                dict.as_concrete_TypeRef(),
                layer_key.as_CFTypeRef() as CFTypeRef,
            )
        };
        if raw_layer.is_null() {
            continue;
        }
        let layer = unsafe { CFNumber::wrap_under_get_rule(raw_layer as CFNumberRef) };
        if layer.to_i32() != Some(0) {
            continue;
        }
        let raw_owner = unsafe {
            CFDictionaryGetValue(
                dict.as_concrete_TypeRef(),
                pid_key.as_CFTypeRef() as CFTypeRef,
            )
        };
        if raw_owner.is_null() {
            continue;
        }
        let owner = unsafe { CFNumber::wrap_under_get_rule(raw_owner as CFNumberRef) };
        if owner.to_i32() != Some(pid) {
            continue;
        }
        let raw_number = unsafe {
            CFDictionaryGetValue(
                dict.as_concrete_TypeRef(),
                number_key.as_CFTypeRef() as CFTypeRef,
            )
        };
        if raw_number.is_null() {
            continue;
        }
        let number = unsafe { CFNumber::wrap_under_get_rule(raw_number as CFNumberRef) };
        let Some(window_number) = number.to_i32().filter(|n| *n > 0) else {
            continue;
        };
        let raw_bounds = unsafe {
            CFDictionaryGetValue(
                dict.as_concrete_TypeRef(),
                bounds_key.as_CFTypeRef() as CFTypeRef,
            )
        };
        if raw_bounds.is_null() {
            candidates.push(window_number as u32);
            continue;
        }
        let bounds_dict =
            unsafe { CFDictionary::wrap_under_get_rule(raw_bounds as CFDictionaryRef) };
        let Some(bounds) = (|| {
            Some((
                cf_dict_f64(&bounds_dict, "X")?,
                cf_dict_f64(&bounds_dict, "Y")?,
                cf_dict_f64(&bounds_dict, "Width")?,
                cf_dict_f64(&bounds_dict, "Height")?,
            ))
        })() else {
            candidates.push(window_number as u32);
            continue;
        };
        if let Some(point) = caret {
            let cg_point = cocoa_to_cg_point(point, screen_h);
            if bounds_contains(bounds, cg_point) {
                return Some(window_number as u32);
            }
        }
        candidates.push(window_number as u32);
    }
    if candidates.len() == 1 {
        candidates.pop()
    } else {
        None
    }
}

fn window_identity_for_client(client: &AnyObject) -> Option<(i32, u32)> {
    let pid = client_process_identifier(client)?;
    if let Some(window_number) = client_window_number(client) {
        return Some((pid, window_number));
    }
    cg_window_number_for_client(client, pid).map(|window_number| (pid, window_number))
}

/// 客户端应用前台窗口的屏幕 bounds（CG 顶左全局点）——v2 气泡锚定的
/// 「激活窗口」参照系：光标坐标收进该 bounds，光标无效退窗口右上。
///
/// 选取：光标点落在某窗口 bounds 内 → 该窗口；否则 pid 的首个 layer-0
/// 窗口（CGWindowList 按 z 序前→后，首个即最前）。与
/// `cg_window_number_for_client` 的枚举逻辑刻意不共用：那边以「窗口
/// number + 光标匹配」服务 vision 截屏的身份辨认，这边要的是 bounds
/// 本身，选择口径不同，拆开各保持单一职责。
fn client_window_bounds(client: &AnyObject) -> Option<(i32, i32, i32, i32)> {
    let pid = client_process_identifier(client)?;
    let screen_h = unsafe { CGDisplayBounds(CGMainDisplayID()).size.height };
    let caret = client_caret_point(client).map(|p| cocoa_to_cg_point(p, screen_h));
    const ON_SCREEN_ONLY: u32 = 1 << 0;
    const EXCLUDE_DESKTOP: u32 = 1 << 4;
    let raw = unsafe { CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0) };
    if raw.is_null() {
        return None;
    }
    let windows: CFArray<CFDictionary> = unsafe { TCFType::wrap_under_create_rule(raw) };
    let layer_key = unsafe { CFString::wrap_under_get_rule(kCGWindowLayer) };
    let bounds_key = unsafe { CFString::wrap_under_get_rule(kCGWindowBounds) };
    let pid_key = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerPID) };
    let mut frontmost: Option<(f64, f64, f64, f64)> = None;
    for dict in windows.iter() {
        let layer = unsafe {
            CFDictionaryGetValue(
                dict.as_concrete_TypeRef(),
                layer_key.as_CFTypeRef() as CFTypeRef,
            )
        };
        if layer.is_null()
            || unsafe { CFNumber::wrap_under_get_rule(layer as CFNumberRef) }.to_i32() != Some(0)
        {
            continue;
        }
        let owner = unsafe {
            CFDictionaryGetValue(
                dict.as_concrete_TypeRef(),
                pid_key.as_CFTypeRef() as CFTypeRef,
            )
        };
        if owner.is_null()
            || unsafe { CFNumber::wrap_under_get_rule(owner as CFNumberRef) }.to_i32() != Some(pid)
        {
            continue;
        }
        let raw_bounds = unsafe {
            CFDictionaryGetValue(
                dict.as_concrete_TypeRef(),
                bounds_key.as_CFTypeRef() as CFTypeRef,
            )
        };
        if raw_bounds.is_null() {
            continue;
        }
        let bounds_dict =
            unsafe { CFDictionary::wrap_under_get_rule(raw_bounds as CFDictionaryRef) };
        let Some(bounds @ (bx, by, bw, bh)) = (|| {
            Some((
                cf_dict_f64(&bounds_dict, "X")?,
                cf_dict_f64(&bounds_dict, "Y")?,
                cf_dict_f64(&bounds_dict, "Width")?,
                cf_dict_f64(&bounds_dict, "Height")?,
            ))
        })() else {
            continue;
        };
        if bw <= 0.0 || bh <= 0.0 {
            continue;
        }
        if let Some(point) = caret {
            if bounds_contains(bounds, point) {
                return Some((
                    bx.round() as i32,
                    by.round() as i32,
                    bw.round() as i32,
                    bh.round() as i32,
                ));
            }
        }
        if frontmost.is_none() {
            frontmost = Some(bounds);
        }
    }
    frontmost.map(|(bx, by, bw, bh)| {
        (
            bx.round() as i32,
            by.round() as i32,
            bw.round() as i32,
            bh.round() as i32,
        )
    })
}

/// 窗口是否仍存在（CGWindowList 按 owner PID + window number 查）。
/// 用于窗口关闭检测：旧会话的窗口身份已不在窗口列表 = 窗口已关闭。
/// 注意：**不能用 ON_SCREEN_ONLY**——最小化/切到其它 Space 的窗口不在屏，
/// 会被误判为已关闭而销毁会话（评审 a2bb79cb 抓出）。只排除桌面壁纸。
fn cg_window_exists(pid: i32, window_number: u32) -> bool {
    const EXCLUDE_DESKTOP: u32 = 1 << 4;
    let raw = unsafe { CGWindowListCopyWindowInfo(EXCLUDE_DESKTOP, 0) };
    if raw.is_null() {
        return false;
    }
    let windows: CFArray<CFDictionary> = unsafe { TCFType::wrap_under_create_rule(raw) };
    let number_key = unsafe { CFString::wrap_under_get_rule(kCGWindowNumber) };
    let pid_key = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerPID) };
    for dict in windows.iter() {
        let raw_owner = unsafe {
            CFDictionaryGetValue(
                dict.as_concrete_TypeRef(),
                pid_key.as_CFTypeRef() as CFTypeRef,
            )
        };
        if raw_owner.is_null() {
            continue;
        }
        let owner = unsafe { CFNumber::wrap_under_get_rule(raw_owner as CFNumberRef) };
        if owner.to_i32() != Some(pid) {
            continue;
        }
        let raw_number = unsafe {
            CFDictionaryGetValue(
                dict.as_concrete_TypeRef(),
                number_key.as_CFTypeRef() as CFTypeRef,
            )
        };
        if raw_number.is_null() {
            continue;
        }
        let number = unsafe { CFNumber::wrap_under_get_rule(raw_number as CFNumberRef) };
        if number.to_i32() == Some(window_number as i32) {
            return true;
        }
    }
    false
}

/// 当前输入字段是否安全输入（NSSecureTextField 等实现 `secureTextEntry` 的
/// 客户端）。尽力而为：仅覆盖原生 AppKit 安全字段，探不到一律按普通字段处理。
fn client_is_secure(client: &AnyObject) -> bool {
    let responds: Bool = unsafe { msg_send![client, respondsToSelector: sel!(secureTextEntry)] };
    if !responds.as_bool() {
        return false;
    }
    let secure: Bool = unsafe { msg_send![client, secureTextEntry] };
    secure.as_bool()
}

/// seq → daemon 侧请求 id（取消用）。seq 全局唯一，映射查询安全；工作线程
/// 无法访问控制器 Ivars（主线程独占），经此表传递 daemon id。
fn daemon_ids() -> &'static Mutex<HashMap<u64, u64>> {
    static M: OnceLock<Mutex<HashMap<u64, u64>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 已取消的 seq 集合：cancel_stream 先登记，工作线程在 llm_start 返回后检查——
/// 闭合「取消发生在 llm_start 返回前」的竞态窗口（此时 daemon id 尚不可知，
/// 无法直接取消；worker 检查到后立即取消并退出）。
fn cancelled_seqs() -> &'static Mutex<HashSet<u64>> {
    static S: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashSet::new()))
}

fn push_llm(seq: u64, event: StreamEvent) {
    if let Ok(mut q) = llm_queue().lock() {
        q.push_back(LlmItem { seq, event });
    }
}

/// 选区 OCR 结果单槽位（issue #82）：verba-trigger 子进程的后台线程写入，
/// 主线程 drain 定时器取走并进预览。与 LLM 流共用「后台产、主线程消」管线。
fn ocr_result_slot() -> &'static Mutex<Option<String>> {
    static S: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

/// 定位 verba-trigger（同目录 sibling，安装布局与 daemon 一致；env 可覆盖）。
fn trigger_exe_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("VERBA_TRIGGER_PATH") {
        let p = std::path::PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            let candidate = d.join("verba-trigger");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

/// `//看图` 截屏：spawn 同目录 verba-trigger vision-shot，PNG 经 stdout
/// 回传；IMK 进程不链接 xcap/image，截图与 PNG 编码复用 verba-trigger
/// 共享实现（与 Windows/Linux 前端同源）。
fn spawn_vision_shot(rect: Option<(i32, i32, i32, i32)>) -> Result<Vec<u8>, String> {
    let exe = trigger_exe_path().ok_or_else(|| "未找到 verba-trigger".to_owned())?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("vision-shot");
    if let Some((x, y, w, h)) = rect {
        cmd.arg("--rect").arg(format!("{x},{y},{w},{h}"));
    }
    let out = cmd
        .output()
        .map_err(|e| format!("启动 verba-trigger vision-shot 失败: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "verba-trigger vision-shot 退出码 {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if out.stdout.is_empty() {
        return Err("verba-trigger vision-shot 未输出 PNG".to_owned());
    }
    Ok(out.stdout)
}

/// spawn verba-trigger 采集子命令（`ocr` 全屏截图 OCR / `asr` 录音转写）：
/// stdout 文本经 ocr_result_slot 进 OCR 预览——与 `///`（region-ocr）同一
/// 「后台产、主线程 drain 消费」管线，无新通道。
fn spawn_trigger_capture(sub: &str) {
    let sub = sub.to_owned();
    std::thread::spawn(move || {
        let Some(exe) = trigger_exe_path() else {
            dbg_log(&format!("trigger {sub}: 未找到 verba-trigger"));
            return;
        };
        match std::process::Command::new(exe).arg(&sub).output() {
            Ok(out) => {
                dbg_log(&format!(
                    "trigger {sub}: 退出 {:?} stdout_len={}",
                    out.status.code(),
                    out.stdout.len()
                ));
                if !out.status.success() {
                    return;
                }
                let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if text.is_empty() {
                    return; // 无识别文本
                }
                if let Ok(mut slot) = ocr_result_slot().lock() {
                    *slot = Some(text);
                }
            }
            Err(e) => dbg_log(&format!("trigger {sub}: 启动 verba-trigger 失败: {e}")),
        }
    });
}

/// spawn verba-trigger speak（daemon TTS 合成 + rodio 播放，不落盘文本）。
fn spawn_trigger_speak(text: String) {
    std::thread::spawn(move || {
        let Some(exe) = trigger_exe_path() else {
            dbg_log("speak: 未找到 verba-trigger");
            return;
        };
        match std::process::Command::new(exe)
            .arg("speak")
            .arg(&text)
            .output()
        {
            Ok(out) => dbg_log(&format!(
                "speak: 退出 {:?} stderr={:?}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            )),
            Err(e) => dbg_log(&format!("speak: 启动 verba-trigger 失败: {e}")),
        }
    });
}

/// 查用户定义短语（`//短语 名称`）；无配置/无此名称返回 None（调用方按
/// 普通生成兜底，与 Windows 前端一致——config 依赖留在前端）。
fn lookup_phrase(name: &str) -> Option<String> {
    let dirs = verba_config::VerbaDirs::locate().ok()?;
    verba_config::phrases::get(&dirs, name).ok().flatten()
}

/// 读悬浮按钮开关（config 文件；默认关 = 隐私默认最小出网）。
pub(crate) fn float_button_enabled() -> bool {
    let Ok(dirs) = verba_config::VerbaDirs::locate() else {
        return false;
    };
    verba_config::ConfigManager::new(dirs)
        .load()
        .map(|c| c.float_button_enable)
        .unwrap_or(false)
}

/// 读悬浮按钮回复模板（config 文件；单一权威 = verba-config 内置默认，
/// helper 不在此重复字面量）。
pub(crate) fn float_button_prompt_template() -> Option<String> {
    let dirs = verba_config::VerbaDirs::locate().ok()?;
    verba_config::ConfigManager::new(dirs)
        .load()
        .ok()
        .map(|c| c.float_button_prompt)
}

/// 气泡管道（v2）的 OCR 文本槽：`float-run` 子进程后台线程写入，
/// drain_stream 主线程取走 → 拼 float_button_prompt 模板 → start_llm
/// 流式预览（与改写同通道，无新 UI 管线）。
///
/// 槽内带 session 代际（复审 F3）：点击时取样 `FLOAT_OCR_EPOCH`，
/// deactivate 递阶——A 窗口点击后焦点切到 B（含密码字段），迟到结果
/// 在 drain 侧按代际作废，不会把 A 的 OCR 灌进 B 的 start_llm
/// （v1 F2 stale-push 守卫的 v2 等价物）。
fn float_ocr_slot() -> &'static Mutex<Option<(u64, String)>> {
    static S: OnceLock<Mutex<Option<(u64, String)>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

/// 气泡管道 session 代际：deactivate 递阶，使在途 OCR 结果作废。
fn bump_float_ocr_epoch() {
    FLOAT_OCR_EPOCH.fetch_add(1, Ordering::SeqCst);
}

/// v2 触发（气泡点击 / `//`+TAB 共用入口）：后台 spawn `verba-trigger
/// float-run`（截前台窗口 → daemon OCR → stdout），识别文本落
/// float_ocr_slot。点击频率级 spawn（同 /// 选区模式），**不在激活频率级**
/// ——v1 的 per-activation spawn 在飞书/终端类客户端引发 IME 会话自激
/// 抖动（D2，verba-float-button-d2-session-churn）。
pub(crate) fn trigger_float_run_async() {
    // 点击时的 session 代际：结果到达时若代际已变（deactivate 过），
    // drain 侧丢弃（复审 F3）。
    let epoch = FLOAT_OCR_EPOCH.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        let Some(exe) = trigger_exe_path() else {
            dbg_log("float(v2): 未找到 verba-trigger");
            crate::float_panel::clear_busy();
            return;
        };
        dbg_log("float(v2): spawn verba-trigger float-run");
        match std::process::Command::new(exe).arg("float-run").output() {
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                dbg_log(&format!(
                    "float(v2): float-run 退出 {:?} stdout_len={} stderr={:?}",
                    out.status.code(),
                    out.stdout.len(),
                    stderr.trim()
                ));
                if !out.status.success() {
                    // 失败原因已完整记日志（首跑缺屏幕录制权限等）。
                    crate::float_panel::clear_busy();
                    return;
                }
                let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if text.is_empty() {
                    crate::float_panel::clear_busy();
                    return;
                }
                if let Ok(mut slot) = float_ocr_slot().lock() {
                    *slot = Some((epoch, text));
                }
            }
            Err(e) => {
                dbg_log(&format!("float(v2): 启动 verba-trigger 失败: {e}"));
                crate::float_panel::clear_busy();
            }
        }
    });
}

/// OCR 识别文本写剪贴板（静默失败）。与 Windows 行为对齐：识别文本随手可粘贴。
fn set_clipboard_text_quiet(text: &str) {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let s = NSString::from_str(text);
        let _ = pb.setString_forType(&s, NSPasteboardTypeString);
    }
}

/// 最新候选查询请求（单槽位：新请求覆盖旧请求）。
struct CandRequest {
    seq: u64,
    pinyin: String,
    schema: String,
}

fn cand_slot() -> &'static (Mutex<Option<CandRequest>>, Condvar) {
    static S: OnceLock<(Mutex<Option<CandRequest>>, Condvar)> = OnceLock::new();
    S.get_or_init(|| (Mutex::new(None), Condvar::new()))
}

/// 全局单例候选查询 worker：只查槽位里的**最新**拼音，中间态自然合并。
///
/// 原先每次击键 spawn 一个线程并新建 daemon 连接：长拼音（数十字符）快速
/// 连打时几十个查询在 daemon 侧排队，最新拼音的响应被积压在前序查询之后，
/// 候选窗反应慢、空格也因候选未到而暂缓（「输入卡」）。改为单 worker +
/// 单连接 + 最新槽位后：daemon 侧积压消失，响应延迟 ≈ 一次查询耗时
/// （数 ms），中间拼音直接跳过（其序号进 dead_seqs，事件不来，无滞留）。
fn ensure_cand_worker() {
    static WORKER: Once = Once::new();
    WORKER.call_once(|| {
        std::thread::spawn(|| {
            let mut client: Option<verba_ipc::VerbaClient> = None;
            let mut last_seq: u64 = 0;
            loop {
                let (seq, pinyin, schema) = {
                    let (lock, cv) = cand_slot();
                    let mut slot = lock.lock().unwrap();
                    loop {
                        match slot.as_ref() {
                            Some(req) if req.seq != last_seq => {
                                break (req.seq, req.pinyin.clone(), req.schema.clone());
                            }
                            _ => slot = cv.wait(slot).unwrap(),
                        }
                    }
                };
                let result = (|| -> Result<Vec<String>, verba_ipc::IpcError> {
                    if client.is_none() {
                        // 首次/重连：完整 ensure_daemon（含拉起与退避重试）。
                        client = Some(ipc::ensure_daemon()?);
                    }
                    match client
                        .as_mut()
                        .expect("刚填充")
                        .rime_candidates(&pinyin, &schema, 27)
                    {
                        Ok(cands) => Ok(cands),
                        Err(_) => {
                            // 连接失效（daemon 重启 / 服务端回收空闲连接）：丢弃
                            // 连接，先用新连接**静默重试一次**——旧连接失效属于
                            // 传输层瞬态，直接上抛会让用户平白错过一轮候选（面板
                            // 闪出合成原文条目）。重试仍失败才作为错误回传。
                            client = None;
                            match ipc::ensure_daemon() {
                                Ok(mut c2) => {
                                    let r = c2.rime_candidates(&pinyin, &schema, 27);
                                    if r.is_ok() {
                                        client = Some(c2);
                                    }
                                    r
                                }
                                Err(e) => Err(e),
                            }
                        }
                    }
                })();
                let event = match result {
                    Ok(cands) => StreamEvent {
                        id: 0,
                        kind: Some(stream_event::Kind::Candidates(verba_protos::Candidates {
                            pinyin,
                            candidates: cands,
                            done: true,
                        })),
                    },
                    Err(e) => error_event(&format!("Rime 候选查询失败: {e}")),
                };
                push_llm(seq, event);
                last_seq = seq;
            }
        });
    });
}

/// 废弃序号集合容量上限：超出时丢弃最旧记录。取较大值（256）：Rime 冷部署/守护
/// 重启时首查可达数秒（守护侧超时 30s），期间每次按键都可能烧一个新候选序号——
/// 容量太小会在响应到达前逐出旧序号，其迟到事件滞留全局队列（复审 LOW）。元素
/// 仅 u64，常驻占用可忽略。
const DEAD_SEQ_MAX: usize = 256;

/// 把序号记入本控制器的废弃集合（0 忽略）：其迟到事件将在 drain 丢弃，
/// 防全局 llm_queue 无界滞留（复审 V7）。
fn record_dead(ivars: &Ivars, seq: u64) {
    if seq == 0 {
        return;
    }
    let mut dead = ivars.dead_seqs.borrow_mut();
    if dead.len() >= DEAD_SEQ_MAX {
        dead.pop_front();
    }
    dead.push_back(seq);
}

/// 控制器实例变量（主线程独占；define_class 只暴露 `&Ivars`，故用内部可变性）。
/// `caps_host_cleanup` 的清理位：bit0 = 收起候选面板、bit1 = 推空组合清宿
/// 主 marked 残留。补做时 CLEAR 位走 clear_composition（内部已含面板收
/// 起），仅 HIDE 位时才单走 hide_candidate_window。欠什么必须记位、不能
/// 在补做时重查现场反推——caps 分支的 reset() 已清空 composed ivar，宿
/// 主侧 marked 文本却仍残留（两份状态不同步正是挂起清理的原因）。
const CAPS_OWE_HIDE: u8 = 1 << 0;
const CAPS_OWE_CLEAR_MARKED: u8 = 1 << 1;

/// AI 结果浮层的面板显示截断（字符数，与 OCR 预览同款）。仅影响显示，
/// 提交永远取 core 的全文（显示截断、提交取全文）。
const AI_RESULT_DISPLAY_CHARS: usize = 40;

/// AI 结果面板条目（纯函数，供单测）：截断结果 + 阶段提示两条；空结果
/// （失败于首块前）只剩提示一条。
///
/// 面板是单列、不换行的候选列表，40 字是既有安全宽度；失败提示因此要求
/// daemon 把“换视觉模型 / 改用 //截图”等动作前置在 40 字内，面板只展示
/// 前缀，完整错误详情看 daemon 日志。
fn ai_result_display_items(text: &str, phase: ResultPhase) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    if !text.is_empty() {
        let mut disp: String = text.chars().take(AI_RESULT_DISPLAY_CHARS).collect();
        if text.chars().count() > AI_RESULT_DISPLAY_CHARS {
            disp.push('…');
        }
        items.push(disp);
    }
    items.push(result_hint(phase).to_owned());
    items
}

struct Ivars {
    machine: RefCell<CompositionMachine>,
    /// 融合后的展示候选（当前页索引由 `page` 给出）。
    candidates: RefCell<Vec<String>>,
    /// 改写对照预览（Some((改写, 原文))：期间数字 1/2/Enter/Esc 由预览拦截）。
    rewrite_preview: RefCell<Option<(String, String)>>,
    /// OCR 预览文本（Some = 预览中）。
    ocr_preview: RefCell<Option<String>>,
    page: Cell<usize>,
    /// 输入会话客户端（保留引用以跨回调上屏/标记）。
    client: RefCell<Option<Retained<AnyObject>>>,
    /// 流式排空定时器。
    timer: RefCell<Option<Retained<NSTimer>>>,
    /// 在途候选融合请求的拼音。
    candidate_pinyin: RefCell<Option<String>>,
    /// 当前组合文本（composedString: 数据源，供 updateComposition 使用）。
    composed: RefCell<String>,
    /// 本控制器的活跃 LLM 流序号（0=无）。per-controller（架构审查 P2-1）：
    /// 多会话（多应用文本域）各自消费自己 seq 的事件，互不串流。
    active_stream: Cell<u64>,
    /// 本控制器的活跃候选请求序号（0=无）。
    active_candidates: Cell<u64>,
    /// 最近废弃的序号集合（取消的流 / 被取代的候选请求）：迟到事件（取消后
    /// 补发的 Final、慢响应的旧候选）入队时 active_* 已归 0/换号无法匹配——
    /// 按此集合在 drain 丢弃，防全局队列无界滞留（复审 V7）。有界
    /// （DEAD_SEQ_MAX = 256 条）：更早的序号其 worker 早已退出、无在途事件。
    dead_seqs: RefCell<VecDeque<u64>>,
    /// 本控制器的旧版 AI 会话 id（兼容 fallback；daemon 无 session_key 时使用）。
    session_id: Cell<u64>,
    /// 当前窗口级 AI 会话 key：set_client 时按 IMK client/NSWindow 刷新。
    /// daemon 优先按它隔离多轮上下文，窗口切回时复用同一槽。
    session_key: RefCell<String>,
    /// 上次计算 session_key 的 client 指针：inputText 同 client 直接复用，
    /// 避免每键枚举 WindowServer 窗口。
    session_key_client: Cell<usize>,
    /// 当前 key 是否来自真实窗口身份（false 表示 input-session fallback）。
    session_key_is_window: Cell<bool>,
    /// 上次会话的窗口身份（PID, CGWindowNumber）：窗口关闭检测用。
    session_identity: RefCell<Option<(i32, u32)>>,
    /// session_key 刷新重入保护：firstRect XPC 可能泵 runloop。
    session_key_refreshing: Cell<bool>,
    /// Rime 方案（单引擎，缓存；配置变更时热更新）。
    candidate_rime_schema: RefCell<String>,
    /// 配置 mtime（用于 Rime 方案热更新检测）。
    candidate_config_mtime: Cell<Option<std::time::SystemTime>>,
    /// IMK 候选窗（惰性创建；updateCandidates/show 驱动显示）。
    candidates_ui: RefCell<Option<Retained<IMKCandidates>>>,
    /// 候选面板外观校正是否已执行（仅首次展示时探针 + 设圆角）。
    panel_probe_done: Cell<bool>,
    /// 宿主调用深度（>0 表示正处于会泵运行循环的同步客户端/面板调用段）。
    host_call_depth: Cell<usize>,
    /// 重入窗内积压的待补放按键（见 `PendingKey` 与 drain 顶部的补放循环）。
    pending_keys: RefCell<VecDeque<PendingKey>>,
    /// CapsLock 切英文时因重入窗（host_call_depth>0）无法即时执行的宿主侧
    /// 清理欠账（CAPS_OWE_* 组合），挂起待下一键补做（见 input_text 顶部
    /// 的补做块与 caps 分支）。
    caps_host_cleanup: Cell<u8>,
    /// `input_text` 同步 drain 的嵌套深度（>0 表示正在处理按键）。
    /// OCR 结果恰好在此期间到达时不能进预览：同一按键会立刻走预览 Other
    /// 分支清掉结果（真机 2026-09-09：OCR 560 字被同 tick 的 `o` 吞掉）。
    drain_during_input: Cell<usize>,
}

impl Default for Ivars {
    fn default() -> Self {
        Self {
            machine: RefCell::new(CompositionMachine::new()),
            candidates: RefCell::new(Vec::new()),
            rewrite_preview: RefCell::new(None),
            ocr_preview: RefCell::new(None),
            page: Cell::new(0),
            client: RefCell::new(None),
            timer: RefCell::new(None),
            candidate_pinyin: RefCell::new(None),
            composed: RefCell::new(String::new()),
            active_stream: Cell::new(0),
            active_candidates: Cell::new(0),
            dead_seqs: RefCell::new(VecDeque::new()),
            session_id: Cell::new(alloc_session_id()),
            session_key: RefCell::new(String::new()),
            session_key_client: Cell::new(0),
            session_key_is_window: Cell::new(false),
            session_identity: RefCell::new(None),
            session_key_refreshing: Cell::new(false),
            candidate_rime_schema: RefCell::new("luna_pinyin_simp".to_owned()),
            candidate_config_mtime: Cell::new(None),
            candidates_ui: RefCell::new(None),
            panel_probe_done: Cell::new(false),
            host_call_depth: Cell::new(0),
            pending_keys: RefCell::new(VecDeque::new()),
            caps_host_cleanup: Cell::new(0),
            drain_during_input: Cell::new(0),
        }
    }
}

define_class!(
    // SAFETY: IMKInputController 的子类化无需额外约束。
    // 必须显式指定 ObjC 类名：objc2 未命名时自动生成「模块路径::结构名+版本号」
    // 之类的名称，而 IMKServer 按 Info.plist 的 InputMethodServerControllerClass
    // 用 NSClassFromString 查找类——查不到则不实例化控制器，按键直通上屏
    // （真机排查：激活进不了 activateServer/inputText）。
    #[name = "VerbaIMKController"]
    #[unsafe(super = IMKInputController)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct VerbaIMKController;

    // SAFETY: NSObjectProtocol 无安全要求。
    unsafe impl NSObjectProtocol for VerbaIMKController {}

    // SAFETY: IMKStateSetting 协议无安全要求。
    unsafe impl IMKStateSetting for VerbaIMKController {
        #[unsafe(method(activateServer:))]
        fn activate_server(&self, sender: Option<&AnyObject>) {
            dbg_log("activateServer");
            // 预热 daemon：冷启动慢的根源是 daemon 懒拉起——首个候选请求才
            // spawn 进程，tokio 启动 + IPC 绑定 + Rime 引擎加载全部落在首串
            // 按键的窗口里（真机感知数秒，「候选框没出现就上屏」）。激活即
            // 后台连一次（进程级一次；daemon 已在则快路径返回）。
            static DAEMON_WARMED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !DAEMON_WARMED.swap(true, Ordering::SeqCst) {
                std::thread::spawn(|| match ipc::ensure_daemon() {
                    Ok(_) => log::info!("[VerbaIMK] daemon 预热完成"),
                    Err(e) => log::warn!("[VerbaIMK] daemon 预热失败（按键时重试）: {e}"),
                });
            }
            self.set_client(sender, true);
            // 先重置再预置空标记文本：控制器可能跨会话复用，composed 残留
            // 上一会话的拼音——若先 updateComposition 会把残留拼音标记进
            // 新会话（随后被宿主当作普通文本上屏）。
            self.reset();
            // 清粘滞预览（issue #83 调试发现）：OCR/改写预览槽只靠 1/2/Esc
            // 清除，会话切换不清理时新会话继承旧预览 → 拦截分支吞掉全部
            // 按键、输入法整体静默失灵（真机踩坑）。会话边界强制清理。
            if self.clear_previews() {
                dbg_log("activate: 清理粘滞预览");
            }
            // 预置空标记文本：首个按键前建立 marked 状态，防首字母在标记状态
            // 建立前被宿主当普通文本直插（快打/刚启动漏字，真机排查）。
            self.host_call("activate_server.prime", || unsafe {
                // SAFETY: updateComposition 为 IMKInputText 非正式协议方法，
                // self 即 IMK 控制器对象。
                let _: () = msg_send![self, updateComposition];
            });
            // 悬浮 AI 回复气泡（v2，默认关）：进程内 Panel，激活显、失活隐。
            // 密码类安全输入不弹（屏幕内容出网链路与 // 同一收口标准）。
            if float_button_enabled() {
                let secure = self
                    .ivars()
                    .client
                    .borrow()
                    .as_deref()
                    .map(client_is_secure)
                    .unwrap_or(false);
                if secure {
                    dbg_log("float(v2): 安全输入字段，跳过");
                    crate::float_panel::hide_bubble();
                } else {
                    // 锚点：光标正下方（左对齐光标）收进前台窗口 bounds
                    // （CG 顶左全局点；真机验收：原右下压插入列遮挡光标，
                    // 改为正下方，下方放不下翻上方）；光标矩形无效（终端
                    // 类客户端的瞬时垃圾矩形）退窗口右上。D1 的几何守卫
                    // 由「收进窗口」语义接管：垃圾坐标最多退化为角落定位，
                    // 不再可能画到屏幕外闪一下。
                    let screen_h = unsafe { CGDisplayBounds(CGMainDisplayID()).size.height };
                    let caret = self
                        .ivars()
                        .client
                        .borrow()
                        .as_deref()
                        .and_then(client_caret_point)
                        .map(|p| cocoa_to_cg_point(p, screen_h))
                        .map(|(x, y)| (x.round() as i32, y.round() as i32));
                    let win = self
                        .ivars()
                        .client
                        .borrow()
                        .as_deref()
                        .and_then(client_window_bounds);
                    match win {
                        Some(bounds) => {
                            let at = crate::float_panel::bubble_anchor(caret, bounds);
                            let origin = cg_bubble_to_cocoa_frame(
                                at,
                                screen_h,
                                crate::float_panel::BUBBLE_SIZE,
                            );
                            crate::float_panel::show_bubble(origin);
                        }
                        None => {
                            dbg_log("float(v2): 无前台窗口 bounds，跳过");
                            crate::float_panel::hide_bubble();
                        }
                    }
                }
            } else {
                crate::float_panel::hide_bubble();
            }
            log::info!("[VerbaIMK] activateServer");
        }

        #[unsafe(method(deactivateServer:))]
        fn deactivate_server(&self, _sender: Option<&AnyObject>) {
            dbg_log("deactivateServer");
            self.cancel_stream();
            self.invalidate_timer();
            // 气泡生命周期 = 输入 session：失活即隐藏（v2 进程内 Panel，
            // 无跨进程 kill；v1 的 helper kill 随 per-activation spawn 一并移除）。
            crate::float_panel::hide_bubble();
            // 作废在途气泡 OCR 结果：换 session 后迟到文本不得灌进新会话
            // 的 start_llm（复审 F3，v1 F2 stale-push 守卫的 v2 等价物）。
            bump_float_ocr_epoch();
            // 重入窗内积压的键属于本会话：换会话（应用/输入位置）后语义已失效
            // （原目标文本域可能已滚动/失焦），丢弃而非带入新会话。
            self.ivars().pending_keys.borrow_mut().clear();
            // 预览槽与会话绑定：换应用/换输入位置后旧预览无效，清掉
            // （同 pending_keys 的会话失效语义；否则拦截分支吞键）。
            let _ = self.clear_previews();
            // 清宿主的标记文本再重置状态机：会话切换（换应用/换输入位置）时
            // 宿主会把残留 marked text 当普通文本上屏——组合中的拼音字母漏进
            // 文档（真机排查：「你 s会」中的 s）。丢弃组合，不提交原文。
            self.clear_composition();
            self.reset();
            // 上面的 clear_composition 已替 caps 挂起的宿主侧清理还账——撤欠
            // 账位，防新会话首键补做一次多余的推空组合（空叠空无实害，但混
            // 淆补做日志语义）。
            self.ivars().caps_host_cleanup.set(0);
            log::info!("[VerbaIMK] deactivateServer");
        }
    }

    // SAFETY: 覆盖父类（NSObjectIMKServerInput 类别）的输入方法。
    impl VerbaIMKController {
        /// 方式一：未映射到 action method 的按键以纯文本投递（IMKInputController
        /// 三参变体）。不实现时父类默认返回 false → 宿主把字符直插文档——快速
        /// 输入偶发「漏字母上屏」的候选根因（TEMP probe 验证中）。与四参变体
        /// 同一处理链（无 keyCode/修饰键，按纯文本逐字符进入状态机）。
        #[unsafe(method(inputText:client:))]
        fn input_text_client(&self, string: Option<&NSString>, sender: Option<&AnyObject>) -> Bool {
            dbg_log(&format!(
                "inputText:client: 3-arg s={:?}",
                string.map(|s| s.to_string())
            ));
            self.input_text(sel!(inputText:key:modifiers:client:), string, 0, 0, sender)
        }

        /// 方式二：接收全部按键的 Unicode / keyCode / 修饰键。
        #[unsafe(method(inputText:key:modifiers:client:))]
        fn input_text(
            &self,
            string: Option<&NSString>,
            key_code: NSInteger,
            flags: NSUInteger,
            sender: Option<&AnyObject>,
        ) -> Bool {
            // 进门口志（先于一切门/路由）：区分「键未投递」vs「被中英门吞」。
            dbg_log(&format!(
                "inputText enter s={:?} key={}",
                string.map(|x| x.to_string()),
                key_code
            ));
            self.set_client(sender, false);
            // 补做 caps 切英文时因重入窗推迟的宿主侧清理（此刻须已出窗；
            // 仍在窗内则继续挂起，下一键再试）。
            if self.ivars().caps_host_cleanup.get() != 0
                && self.ivars().host_call_depth.get() == 0
            {
                let owed = self.ivars().caps_host_cleanup.replace(0);
                if owed & CAPS_OWE_CLEAR_MARKED != 0 {
                    self.clear_composition();
                } else if owed & CAPS_OWE_HIDE != 0 {
                    self.hide_candidate_window();
                }
            }
            // CapsLock 跟随（系统拼音同款）：会话级锁存位 ON = 英文直输，OFF =
            // 中文。非 toggle——revert AI v1 的裸 toggle 教训（issue #83 真机：
            // 系统「CapsLock 切 ABC」开启时切源动作把 keyCode 57 投给刚激活
            // 的控制器，toggle 与系统状态打架致静默失灵）；系统管理状态、
            // LED 即中英指示，输入法每次按键只读会话级修饰锁存态。
            let caps_on: bool = unsafe {
                let flags: NSEventModifierFlags = msg_send![NSEvent::class(), modifierFlags];
                flags.contains(NSEventModifierFlags::CapsLock)
            };
            if key_code == 57 {
                // CapsLock 键本身：透传交系统翻转锁定态（下一次按键即新态）。
                return Bool::new(false);
            }
            if caps_on {
                // 英文态门：**先于 drain**（审查 F1）——切英文即刻作废中文侧
                // 现场（组合/流/预览/积压键）再透传宿主直插。若 drain 在前：
                // OCR 预览刚 show 就被同帧消费销毁、在途流式 chunk 与迟到候选
                // 继续写回宿主、caps OFF 期间积压的拼音键被重放进英文现场。
                // 此前本分支只清预览（had=false 时零清理），宿主 marked 残留、
                // 16ms 定时器继续回流、关 caps 后空格提交陈旧候选（审查 #1）。
                let had_marked = !self.ivars().composed.borrow().is_empty();
                let had_previews = self.ivars().ocr_preview.borrow().is_some()
                    || self.ivars().rewrite_preview.borrow().is_some();
                let has_state = !matches!(
                    self.ivars().machine.borrow().state(),
                    MachineState::Idle
                );
                let has_pending = !self.ivars().pending_keys.borrow().is_empty();
                if had_marked || had_previews || has_state || has_pending {
                    // 核心侧/队列侧清理不触宿主 XPC，重入窗内也安全：
                    // cancel_stream 另起线程走 daemon IPC；feed_escape 纯核心。
                    // had_marked 须在 reset() 清空 composed 前捕获。
                    if had_previews {
                        let _ = self.clear_previews();
                    }
                    // 积压键属 caps OFF 时的中文会话，切英文后语义失效——
                    // 丢弃而非重放（同 deactivateServer 的会话失效语义）。
                    self.ivars().pending_keys.borrow_mut().clear();
                    self.reset();
                    // 宿主侧（面板 hide + 推空组合）走 host_call XPC：重入窗内
                    // 执行会与外层客户端调用交错（NSInvalidArgumentException
                    // 真机崩溃，见 host_call 注释）——按欠账记位，挂起待下一
                    // 键补做。
                    if self.ivars().host_call_depth.get() == 0 {
                        if had_previews {
                            self.hide_candidate_window();
                        }
                        if had_marked {
                            self.clear_composition();
                        }
                    } else {
                        let mut owed = 0u8;
                        if had_previews {
                            owed |= CAPS_OWE_HIDE;
                        }
                        if had_marked {
                            owed |= CAPS_OWE_CLEAR_MARKED;
                        }
                        // 累积不覆盖：重入窗内二次进 caps 清理（翻两次
                        // CapsLock）时，前一笔未补做的欠账不得被抹掉
                        // （独立复审 P3）。
                        self.ivars()
                            .caps_host_cleanup
                            .set(self.ivars().caps_host_cleanup.get() | owed);
                    }
                }
                return Bool::new(false);
            }
            // 带修饰键的组合键不吞（Cmd/Ctrl/Option 留给系统或宿主应用）。
            // 必须放在 OCR 预览/同步 drain 之前：否则 Cmd+C、Cmd+Space 等
            // 可能在透传前误触发 OCR 文本上屏。
            let mods = NSEventModifierFlags(flags);
            if mods.intersects(
                NSEventModifierFlags::Command
                    | NSEventModifierFlags::Control
                    | NSEventModifierFlags::Option,
            ) {
                return Bool::new(false);
            }
            // 先排空在途候选事件再处理按键：Rime 响应通常在下一击键前已入队
            // （worker 查询仅数 ms），先送达状态机可让空格/数字立即看到候选，
            // 免去 16ms 定时器延迟被感知为「输入卡」。
            //
            // 用深度计数标记本次 drain 来自输入回调：若 OCR 结果恰在此时到达，
            // 不能进入预览（否则当前按键会立刻清掉预览，识别文本丢失）。
            let drain_depth = self.ivars().drain_during_input.get();
            self.ivars().drain_during_input.set(drain_depth + 1);
            self.drain_stream(sel!(drainVerbaStream));
            self.ivars()
                .drain_during_input
                .set(self.ivars().drain_during_input.get().saturating_sub(1));
            // 改写对照预览拦截（**core 槽位为门**，与 Windows 同构单源：
            // RewriteReady 臂已调 begin_rewrite_preview，ai_preview 同时撤销
            // ——旁路直喂（重入窗重放/候选点击）的 r/e 只会从 feed_char 得
            // 到 None，不会误触重试/改提示词后让前端预览槽与 core 失同步，
            // 独立复审 P2）：1/Enter/空格=改写上屏、2=原文上屏、Esc 取消；
            // 其余键不属于预览，落回正常路由（此刻仍 ResultReady：可打印
            // 键被认领后 feed_char 返 None 照常吞掉，不会字母泄漏进宿主）。
            if self.ivars().machine.borrow().rewrite_previewing() {
                if let Some(pk) = preview_key_of(classify_key(string, key_code)) {
                    dbg_log(&format!("改写预览键: {pk:?}"));
                    self.handle_rewrite_preview_key(pk);
                    return Bool::new(true);
                }
            }
            // OCR 预览拦截：与 replay_pending 共用同一 helper，避免重入队列
            // 绕过「确认/可打印键先提交」语义。返回 true 表示本键已被消费。
            if self.handle_ocr_preview_key(classify_key(string, key_code)) {
                return Bool::new(true);
            }
            let panel_visible = self
                .ivars()
                .candidates_ui
                .borrow()
                .as_ref()
                // SAFETY: isVisible 为 NSPanel/NSWindow 公开方法，ui 仅在主线程访问。
                .map(|ui| unsafe { ui.isVisible() })
                .unwrap_or(false);
            // 调试日志：sender 类名辅助区分真宿主 client 与包装器（会话切
            // 换排查时 sender 指针变化是关键线索）。
            let sender_cls = sender.map(|s| {
                // SAFETY: class 为 NSObject 公开方法，sender 由 IMK 回调传入且本帧存活。
                let cls: &AnyClass = unsafe { msg_send![s, class] };
                cls.name().to_string_lossy().into_owned()
            });
            dbg_log(&format!(
                "inputText s={:?} key={} flags={:#x} state={:?} our_client={} panel_visible={} sender={:?} sender_ptr={:p}",
                string.map(|s| s.to_string()),
                key_code,
                flags,
                self.ivars().machine.borrow().state(),
                self.ivars().client.borrow().is_some(),
                panel_visible,
                sender_cls,
                sender.map_or(std::ptr::null(), |s| s as *const AnyObject),
            ));

            let key = classify_key(string, key_code);
            // Shift+方向键等：交给宿主做文本选择，不当候选翻页。
            if mods.contains(NSEventModifierFlags::Shift)
                && matches!(key, Some(ImkKey::PageUp | ImkKey::PageDown))
            {
                return Bool::new(false);
            }

            // 粘贴整串（keyCode=0 时到达）提前解析：重入门卫也需要据此分类。
            let pasted = if key_code == 0 {
                string.map(|s| s.to_string())
            } else {
                None
            };

            // 重入门卫（独立审查阻塞项①）：show()/commit() 等宿主调用段的
            // XPC 泵循环会把新按键**嵌套**派发进来；此时继续 apply_action
            // （setMarkedText/insertText/候选刷新会再次触泵）会与外层未完成
            // 的客户端调用交错——定时器路径已被深度检查挡住，这里是另一条
            // 同形态重入源。可处理的键入队待补放（返回 true 认领，防宿主
            // 直插造成漏字）；直通类判定保持原语义原样放行。
            if self.ivars().host_call_depth.get() > 0 {
                let is_paste = pasted.as_ref().is_some_and(|t| t.chars().count() > 1);
                let plain_char = !is_paste && matches!(key, Some(ImkKey::Char(_)));
                let composing_control = matches!(
                    self.ivars().machine.borrow().state(),
                    MachineState::Pinyin
                        | MachineState::PendingSlash
                        | MachineState::Prompt
                        | MachineState::Streaming
                        | MachineState::ResultReady
                        | MachineState::Failed
                ) && matches!(
                    key,
                    Some(ImkKey::Backspace | ImkKey::Enter | ImkKey::Escape)
                        | Some(ImkKey::PageUp | ImkKey::PageDown)
                );
                if let Some(text) = pasted.filter(|_| is_paste) {
                    self.ivars()
                        .pending_keys
                        .borrow_mut()
                        .push_back(PendingKey::Paste(text));
                    return Bool::new(true);
                }
                if plain_char || composing_control {
                    let pk = match key.expect("上方 match 已确认 key 非空") {
                        ImkKey::Char(c) => PendingKey::Char(c),
                        ImkKey::Backspace => PendingKey::Backspace,
                        ImkKey::Enter => PendingKey::Enter,
                        ImkKey::Escape => PendingKey::Escape,
                        ImkKey::PageUp => PendingKey::PageUp,
                        ImkKey::PageDown => PendingKey::PageDown,
                        ImkKey::ArrowUp => PendingKey::ArrowUp,
                        ImkKey::ArrowDown => PendingKey::ArrowDown,
                    };
                    self.ivars().pending_keys.borrow_mut().push_back(pk);
                    return Bool::new(true);
                }
                // 其余（Idle 控制键交宿主 / 未识别键直插）：维持原有 false 语义。
                return Bool::new(false);
            }

            // 多字符粘贴（keyCode=0 时整串到达）：逐字符喂入状态机并逐步应用动作。
            // 此前 classify_key 只取首字符，其余全部丢失（架构审查 P1-2）。
            // 候选查询按粘贴整体合并为一次（见下）：原先每字符一个 UpdatePinyin
            // 触发一次 start_candidates → Rime worker 线程 + daemon 查询，超长
            // 粘贴（数百字）会瞬时放大为同等规模的线程/查询/主线程 marked-text
            // 更新风暴（复审发现）；中间态仅按状态机候选刷新显示，循环结束后
            // 对最终拼音补发一次查询，候选结果由 seq 过滤保证只消费最新代。
            if let Some(text) = pasted.filter(|t| t.chars().count() > 1) {
                let mut applied = false;
                let mut last_candidate_req: Option<LlmCandidateRequest> = None;
                for ch in text.chars().filter(|&ch| is_pasteable_char(ch)) {
                    let mut action = self.ivars().machine.borrow_mut().feed_char(ch);
                    // 摘出候选请求合并到循环末尾一次发送；余下动作照常逐步应用。
                    if let Action::UpdatePinyin { llm_request, .. } = &mut action {
                        last_candidate_req = llm_request.take();
                    }
                    self.apply_action(action);
                    applied = true;
                }
                if let Some(req) = last_candidate_req {
                    self.start_candidates(req);
                }
                return Bool::new(applied);
            }

            // 占位期判定（发送后、首 token 前：浮层已武装且结果仍空）：下面
            // 记一条可读日志——本轮 Enter/空格/1 会被 core 有意吞掉（空结果绝不
            // 结算，见 feed_enter 的 Streaming/ResultReady 臂），真机上「按了没
            // 反应」是设计语义，不是丢键。
            let placeholder_phase = {
                let m = self.ivars().machine.borrow();
                m.ai_previewing() && m.result().is_empty()
            };
            let was_idle = matches!(self.ivars().machine.borrow().state(), MachineState::Idle);
            let action = match key {
                Some(ImkKey::Char(c)) => {
                    // 大写/小写统一交给状态机：Idle 大写直上屏、Pinyin/Prompt
                    // 按候选提交 + 字符（见 verba-core machine 大写分支）。
                    self.ivars().machine.borrow_mut().feed_char(c)
                }
                Some(ImkKey::Backspace) => self.ivars().machine.borrow_mut().feed_backspace(),
                Some(ImkKey::Enter) => self.ivars().machine.borrow_mut().feed_enter(),
                Some(ImkKey::Escape) => self.ivars().machine.borrow_mut().feed_escape(),
                Some(ImkKey::PageUp) => self.ivars().machine.borrow_mut().feed_page_up(),
                Some(ImkKey::PageDown) => self.ivars().machine.borrow_mut().feed_page_down(),
                Some(ImkKey::ArrowUp) | Some(ImkKey::ArrowDown) => {
                    // 无候选时方向键交宿主（光标在组合串内移动）；有候选才选字。
                    if self.ivars().candidates.borrow().is_empty() {
                        dbg_log("inputText arrow without candidates -> return false（宿主光标）");
                        return Bool::new(false);
                    }
                    if matches!(key, Some(ImkKey::ArrowUp)) {
                        self.ivars().machine.borrow_mut().feed_arrow_up()
                    } else {
                        self.ivars().machine.borrow_mut().feed_arrow_down()
                    }
                }
                None => {
                    dbg_log("inputText classify None -> return false（宿主直插）");
                    return Bool::new(false);
                }
            };
            if placeholder_phase
                && matches!(action, Action::None)
                && matches!(
                    key,
                    Some(ImkKey::Enter) | Some(ImkKey::Char(' ')) | Some(ImkKey::Char('1'))
                )
            {
                dbg_log("占位期确认键已吞（结果为空，绝不提交空串）");
            }
            // 空闲态且状态机无动作（如 Enter/Backspace/Esc）：交给宿主处理。
            if was_idle && matches!(action, Action::None) {
                dbg_log("inputText idle+None -> return false");
                return Bool::new(false);
            }
            dbg_log(&format!("  -> key={:?} action={:?} was_idle={}", key, action, was_idle));
            let _ = self.apply_action(action);
            Bool::new(true)
        }

        /// 标记文本内光标位置（UTF-16，末尾）。
        ///
        /// IMKInputController 的默认 selectionRange 会向客户端查询 markedRange；
        /// 宿主 XPC 在部分应用/系统版本上会抛 ObjC 异常，并在 Rust 异常清理途中
        /// abort（crash report：_IPMDServerClientWrapperLegacy markedRange →
        /// invocationAwaitXPCReply）。组合文本由本控制器单源维护，直接返回其
        /// UTF-16 末尾即可，既避免宿主往返也消除该崩溃面。
        #[unsafe(method(selectionRange))]
        fn selection_range(&self) -> NSRange {
            selection_range_for(&self.ivars().composed.borrow())
        }

        /// 组合文本数据源：updateComposition 调用它取当前 preedit 发给 client。
        #[unsafe(method_id(composedString:))]
        fn composed_string(&self, _sender: Option<&AnyObject>) -> Option<Retained<NSString>> {
            let s = NSString::from_str(&self.ivars().composed.borrow());
            dbg_log(&format!("composedString -> {}", s));
            Some(s)
        }

        /// 候选窗数据源（0 参）：部分运行时按 `candidates` 查询。
        #[unsafe(method_id(candidates))]
        fn candidates(&self) -> Option<Retained<NSArray<NSString>>> {
            self.current_candidates()
        }

        /// 候选窗数据源（1 参）：SDK 头文件 IMKServerInput 类别声明为 `candidates:`。
        #[unsafe(method_id(candidates:))]
        fn candidates_with_sender(&self, _sender: Option<&AnyObject>) -> Option<Retained<NSArray<NSString>>> {
            self.current_candidates()
        }

        /// 候选窗点击选中。
        #[unsafe(method(candidateSelected:))]
        fn candidate_selected(&self, candidate: Option<&NSAttributedString>) {
            let Some(attr) = candidate else {
                return;
            };
            let text = attr.string().to_string();
            dbg_log(&format!("candidateSelected text={}", text));
            let ivars = self.ivars();
            let Some(global_idx) = ivars.candidates.borrow().iter().position(|c| c == &text)
            else {
                dbg_log("  -> 候选不在当前列表");
                return;
            };
            let page = ivars.page.get();
            let Some(digit) = selection_digit(global_idx, page, CompositionMachine::PINYIN_PAGE_SIZE) else {
                dbg_log("  -> 无对应数字键");
                return;
            };
            // 重入门卫（同 input_text）：面板定位完成前的抢点会把数字选字
            // 嵌套进宿主调用段——入队退栈后补放。
            if self.ivars().host_call_depth.get() > 0 {
                self.ivars()
                    .pending_keys
                    .borrow_mut()
                    .push_back(PendingKey::Char(digit));
                return;
            }
            // 改写对照预览期间点击条目：数字即预览选取键（1=改写 2=原文）
            // ——core 已武装 rewrite_preview，直喂 feed_char 会因 ai_preview
            // 已撤销而得 None 成死点（独立复审 P2）。
            if self.ivars().machine.borrow().rewrite_previewing() {
                match digit {
                    '1' => {
                        self.handle_rewrite_preview_key(PreviewKey::Digit1);
                        return;
                    }
                    '2' => {
                        self.handle_rewrite_preview_key(PreviewKey::Digit2);
                        return;
                    }
                    _ => {}
                }
            }
            let action = self.ivars().machine.borrow_mut().feed_char(digit);
            dbg_log(&format!("  -> digit={} action={:?}", digit, action));
            let _ = self.apply_action(action);
        }

        /// 宿主要求结束组合（如焦点切换）：把当前组合内容提交。
        #[unsafe(method(commitComposition:))]
        fn commit_composition(&self, _sender: Option<&AnyObject>) {
            dbg_log("commitComposition called");
            let text = {
                let mut m = self.ivars().machine.borrow_mut();
                let text = match m.state() {
                    MachineState::Pinyin | MachineState::Prompt => m.preedit(),
                    // Failed 保留已生成的部分结果：宿主强制结束组合时按流同款
                    // 提交部分文本（空部分提交空）。
                    MachineState::Streaming | MachineState::ResultReady | MachineState::Failed => {
                        m.result().to_owned()
                    }
                    _ => String::new(),
                };
                m.feed_escape();
                text
            };
            if !text.is_empty() {
                self.commit(&text);
                // 宿主要求结束组合时提交的原文同样是上屏内容，入上下文会话。
                self.forward_commit_to_context(text);
            }
            self.cancel_stream();
            self.invalidate_timer();
        }

        /// 输入法菜单：提供「设置…」入口打开 verba-settings 设置面板。
        #[unsafe(method_id(menu))]
        fn menu(&self) -> Option<Retained<NSMenu>> {
            // SAFETY: IMK 回调均发生在主线程。
            let mtm = unsafe { MainThreadMarker::new_unchecked() };
            let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Verba"));
            // SAFETY: NSMenu/NSMenuItem 构造为主线程 AppKit 公开 API（mtm 已断言主线程）。
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str("设置…"),
                    Some(sel!(openSettings:)),
                    &NSString::from_str(""),
                )
            };
            // SAFETY: self 与 AnyObject 指向同一 ObjC 对象；controller 由 IMKServer 持有，
            // NSMenuItem 对 target 为弱引用，生命周期安全。
            let target: &AnyObject =
                unsafe { &*(self as *const VerbaIMKController as *const AnyObject) };
            // SAFETY: setTarget 为 NSMenuItem 公开方法，target 刚借用于存活的 self。
            unsafe { item.setTarget(Some(target)) };
            menu.addItem(&item);
            Some(menu)
        }

        /// 打开设置面板（菜单项 action）。
        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            // 优先经 **LaunchServices** 打开设置 .app：已在运行则激活到前台、不新开
            // 实例；直接 spawn 裸二进制起不到前台（2026-09-18 真机实测）。
            if let Some(app) = crate::ipc::settings_app_path() {
                log::info!("[VerbaIMK] 打开设置面板（LaunchServices）: {}", app.display());
                let _ = std::process::Command::new("/usr/bin/open")
                    .arg(&app)
                    .spawn();
                return;
            }
            // 开发态（仓库里只有裸二进制、没有 .app）：退回 spawn。
            // 注意此路径窗口可能不在前台——仅开发用，打包形态走上面的分支。
            match crate::ipc::settings_exe_path() {
                Some(p) => {
                    log::info!("[VerbaIMK] 打开设置面板（开发态 spawn）: {}", p.display());
                    let _ = std::process::Command::new(&p).spawn();
                }
                None => log::warn!(
                    "[VerbaIMK] 未找到设置面板（VERBA_SETTINGS_APP / VERBA_SETTINGS_PATH 或 bundle 内 Library/Verba Settings.app）"
                ),
            }
        }

        /// 主线程定时器：排空本控制器的事件。
        ///
        /// 只取走本控制器活跃序号的事件，其余留在队列（多会话场景下另一控制器
        /// 的定时器会消费自己的部分——整体清空会互相丢弃事件）。废弃序号
        /// （已取消流、被取代的旧候选）的残留事件直接丢弃，防全局队列无界滞留。
        #[unsafe(method(drainVerbaStream))]
        fn drain_stream(&self) {
            // 重入保护：宿主调用段（标记文本/面板定位会同步泵运行循环）期间
            // 嵌套触发本定时器时，不得再次对客户端发起调用——跳过本次，
            // 退栈后下一个 tick 补排（见 host_call 与真机崩溃分析）。
            if self.ivars().host_call_depth.get() > 0 {
                return;
            }
            // #105 item3 预览 TTL：超时自动作废（前端槽位 + core）+ 收起候选窗。
            if self.ivars().machine.borrow_mut().expire_stale_ocr_preview() {
                *self.ivars().ocr_preview.borrow_mut() = None;
                self.hide_candidate_window();
            }
            // 补放重入窗积压的键：必须先于事件排空（「键 → 由它触发的查询」
            // 顺序不可倒置），也必须先于下方空队列快速路径（积压键 + 空事件
            // 队列正是最常见的补放场景）。若补放中又被泵重入，新到的键安全
            // 地回队等待下一 tick。
            loop {
                let next = self.ivars().pending_keys.borrow_mut().pop_front();
                let Some(pk) = next else { break };
                dbg_log("replay pending key");
                self.replay_pending(pk);
            }
            // 气泡管道 OCR 结果（v2）：float-run 的识别文本 → 拼
            // float_button_prompt 模板（含 {ocr} 占位校验，缺占位即配置错误，
            // 记日志拒绝静默丢 OCR 上下文）→ start_llm 流式预览（与改写同
            // 通道：候选面板 ✨ 生成中 → 结果 Enter 上屏）。
            if let Some((epoch, ocr_text)) =
                float_ocr_slot().lock().ok().and_then(|mut s| s.take())
            {
                crate::float_panel::clear_busy();
                if epoch != FLOAT_OCR_EPOCH.load(Ordering::SeqCst) {
                    // session 已切换（deactivate 递阶）：A 窗口的迟到 OCR
                    // 不得灌进 B session 的 start_llm（复审 F3）。
                    dbg_log("float(v2): OCR 结果迟到（session 已切换），丢弃");
                } else {
                // 模板/占位校验失败：记日志丢弃本次 OCR 文本（配置错误不
                // 静默吞上下文），不 return——下方 ocr_result_slot 等消费
                // 方同 tick 仍要跑。
                const PLACEHOLDER: &str = "{ocr}";
                let prompt = float_button_prompt_template()
                    .filter(|t| t.contains(PLACEHOLDER))
                    .map(|t| t.replace(PLACEHOLDER, &ocr_text));
                match prompt {
                    Some(prompt) => {
                        dbg_log(&format!(
                            "float(v2): OCR {} 字 → start_llm",
                            ocr_text.chars().count()
                        ));
                        self.start_llm(prompt, None, false, None);
                    }
                    None => {
                        dbg_log("float(v2): float_button_prompt 模板缺失或缺 {ocr} 占位符");
                    }
                }
                }
            }
            // 选区 OCR 结果槽位（issue #82）：与 LLM 事件同管线消费。
            // 与 Windows 对齐：剪贴板照常；上屏走 OCR 预览（候选窗单候选，
            // Enter/空格/1 上屏，Esc 取消）；非 Idle（组合中触发等罕见场景）
            // 回退直接上屏。
            if let Some(text) = ocr_result_slot().lock().ok().and_then(|mut s| s.take()) {
                dbg_log(&format!("OCR: 槽位消费 len={}", text.chars().count()));
                set_clipboard_text_quiet(&text);
                if should_commit_ocr_during_input(self.ivars().drain_during_input.get() > 0) {
                    // 输入回调期间结果到达：当前按键尚未处理完，若进预览会被
                    // 同一按键的 Other 分支立即清掉（真机 560 字丢失）。直接
                    // 上屏，当前按键随后继续走正常路由。
                    dbg_log("OCR: 输入期间结果到达，直接上屏");
                    self.commit(&text);
                } else {
                    match self
                        .ivars()
                        .machine
                        .borrow_mut()
                        .begin_ocr_preview(text.clone())
                    {
                        Some(Action::OcrPreview { text: t }) => {
                            dbg_log("OCR: 进入候选窗预览");
                            self.apply_action(Action::OcrPreview { text: t });
                        }
                        _ => {
                            dbg_log("OCR: 非 Idle 回退直接上屏");
                            self.commit(&text);
                        }
                    }
                }
                return;
            }
            static DRAIN_LOGGED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            let stream_seq = self.ivars().active_stream.get();
            let cand_seq = self.ivars().active_candidates.get();
            // 空队列快速路径：避免每 tick（16ms）无事件时仍 borrow dead_seqs。
            if llm_queue().lock().unwrap().is_empty() {
                return;
            }
            if !DRAIN_LOGGED.swap(true, Ordering::SeqCst) {
                dbg_log(&format!(
                    "drain: first run stream_seq={} cand_seq={}",
                    stream_seq, cand_seq
                ));
            }
            let dead_any = {
                let dead = self.ivars().dead_seqs.borrow();
                !dead.is_empty()
            };
            let mine: Vec<LlmItem> = {
                let mut q = llm_queue().lock().unwrap();
                let mut kept = VecDeque::new();
                let mut mine = Vec::new();
                let mut dead_hit = Vec::new();
                for item in q.drain(..) {
                    if dead_any && self.ivars().dead_seqs.borrow().contains(&item.seq) {
                        // 本控制器废弃序号的残留事件（取消后补发的 Final、旧候选迟到
                        // 响应）：丢弃并记录。dead_seqs 条目**保留**——取消流的迟到
                        // 事件常跨多个 tick（缓冲 chunk + 守护补发的 Final），
                        // 首命中即删会让下一 tick 的 Final 无匹配而滞留（复审 LOW）；
                        // 仅随后清 cancelled_seqs（其竞态窗口已闭合）。
                        dead_hit.push(item.seq);
                        continue;
                    }
                    if item.seq == stream_seq || item.seq == cand_seq {
                        mine.push(item);
                    } else {
                        kept.push_back(item);
                    }
                }
                *q = kept;
                // 全局队列上限：既不属于本控制器活跃序号、也不在任何控制器的
                // dead_seqs 的孤儿事件（控制器 deallocated 前未 drain 的残留）
                // 无人回收，长期运行会无界滞留且每次 tick 全量遍历（复审发现）。
                // 超限丢弃最旧条目——常规下活跃流事件每个 tick 被其控制器取走，
                // 滞留的几乎全是孤儿/迟到事件；极端场景（主线程被长粘贴阻塞且
                // 多控制器并发大流）也可能丢到活跃流头部 chunk，但需数千事件
                // 积压才触发，取舍可接受（对抗审查 F2）。
                const LLM_QUEUE_MAX: usize = 1024;
                let len = q.len();
                if len > LLM_QUEUE_MAX {
                    q.drain(..len - LLM_QUEUE_MAX);
                }
                if !dead_hit.is_empty() {
                    // 只清 cancelled_seqs；dead_seqs 条目保留（见上），供后续 tick
                    // 继续丢弃同一取消流的迟到事件。
                    let mut cancelled = cancelled_seqs().lock().unwrap();
                    for seq in dead_hit {
                        cancelled.remove(&seq);
                    }
                }
                mine
            };
            for item in mine {
                if item.seq == stream_seq {
                    self.feed_stream_event(item.event);
                } else {
                    self.feed_candidates_event(item.event);
                }
            }
        }
    }
);

/// 按键分类（可打印字符优先，其次按 keyCode 识别功能键）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImkKey {
    Char(char),
    Backspace,
    Enter,
    Escape,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
}

/// `selectionRange` 的生产实现：返回 composed text 的 UTF-16 末尾。
///
/// 抽成纯函数供控制器与单测共用，防止测试复制实现而无法捕获回归。
fn selection_range_for(text: &str) -> NSRange {
    NSRange::new(text.encode_utf16().count() as NSUInteger, 0)
}

/// OCR 结果在输入回调期间到达时，是否直接提交上屏。
///
/// 抽成纯函数供单测钉住：输入期间必须 commit（否则当前按键会清掉预览），
/// 空闲时仍走预览确认。
fn should_commit_ocr_during_input(during_input: bool) -> bool {
    during_input
}

/// OCR 预览确认键（提交识别文本）：Enter/空格/1。
fn ocr_preview_confirm_key(key: Option<ImkKey>) -> bool {
    matches!(
        key,
        Some(ImkKey::Char('1')) | Some(ImkKey::Char(' ')) | Some(ImkKey::Enter)
    )
}

/// OCR 预览期间遇到可打印非确认键：先提交识别文本再继续处理该键。
/// `2` 保持既有 F10 语义（OCR 预览下不选取），不作为隐式确认。
fn ocr_preview_other_commits(key: Option<ImkKey>) -> bool {
    matches!(key, Some(ImkKey::Char(c)) if c != '2')
}

/// 候选分页切片（纯逻辑，供 candidates: 数据源与测试复用）。
fn page_slice(candidates: &[String], page: usize, page_size: usize) -> Vec<String> {
    candidates
        .iter()
        .skip(page * page_size)
        .take(page_size)
        .cloned()
        .collect()
}

/// 候选全局索引 → 当前页数字选择键（1 起；越界返回 None）。
fn selection_digit(global_idx: usize, page: usize, page_size: usize) -> Option<char> {
    let offset = page * page_size;
    if global_idx < offset {
        return None;
    }
    let rel = global_idx + 1 - offset;
    if (1..=page_size).contains(&rel) {
        char::from_digit(rel as u32, 10)
    } else {
        None
    }
}

/// 可喂入状态机的字符判定：与 classify_key 的字符过滤同款规则
/// （排除控制字符与 NS*FunctionKey 私有区 0xF700..=0xF8FF）。
fn is_pasteable_char(c: char) -> bool {
    c >= ' ' && !(0xF700..=0xF8FF).contains(&(c as u32))
}

fn classify_key(string: Option<&NSString>, key_code: NSInteger) -> Option<ImkKey> {
    match key_code {
        // delete / backspace
        51 => Some(ImkKey::Backspace),
        // return / keypad enter
        36 | 76 => Some(ImkKey::Enter),
        // esc
        53 => Some(ImkKey::Escape),
        // 左箭头：上一页；右箭头：下一页
        123 => Some(ImkKey::PageUp),
        124 => Some(ImkKey::PageDown),
        // 上/下箭头：候选选中移动（跨页遍历）
        126 => Some(ImkKey::ArrowUp),
        125 => Some(ImkKey::ArrowDown),
        // 其余 keyCode 交给字符分支
        _ => string
            .and_then(|s| s.to_string().chars().next())
            .filter(|c| {
                // 过滤控制字符与 NS*FunctionKey（0xF700..0xF8FF）：这些走 keyCode 已处理，
                // 避免被误当作可打印字符提交。
                is_pasteable_char(*c)
            })
            .map(ImkKey::Char),
    }
}

/// 改写对照预览键分类（与 Windows `classify_preview_key` 同一份语义清单，
/// 防两条路由各自漂移）：仅 1/2/空格/Enter/Esc 属于预览键；其余（字母、
/// 退格、方向键）返回 None 落回正常路由——ResultReady 态可打印键被认领后
/// feed_char 返 None 照常吞掉，不泄漏进宿主文档。
fn preview_key_of(key: Option<ImkKey>) -> Option<PreviewKey> {
    match key? {
        ImkKey::Enter => Some(PreviewKey::Enter),
        ImkKey::Escape => Some(PreviewKey::Escape),
        ImkKey::Char(' ') => Some(PreviewKey::Space),
        ImkKey::Char('1') => Some(PreviewKey::Digit1),
        ImkKey::Char('2') => Some(PreviewKey::Digit2),
        _ => None,
    }
}

/// 待重放键 → IMK 按键分类（OCR 预览重入队列与 input_text 共用路由）。
fn pending_key_to_imk_key(pk: &PendingKey) -> Option<ImkKey> {
    match pk {
        PendingKey::Char(c) => Some(ImkKey::Char(*c)),
        PendingKey::Backspace => Some(ImkKey::Backspace),
        PendingKey::Enter => Some(ImkKey::Enter),
        PendingKey::Escape => Some(ImkKey::Escape),
        PendingKey::PageUp => Some(ImkKey::PageUp),
        PendingKey::PageDown => Some(ImkKey::PageDown),
        PendingKey::ArrowUp => Some(ImkKey::ArrowUp),
        PendingKey::ArrowDown => Some(ImkKey::ArrowDown),
        PendingKey::Paste(_) => None,
    }
}

/// 待重放键 → 改写预览键（预览期间入队的 1/2/空格/Enter/Esc；replay_pending
/// 用，与 preview_key_of 同一份语义清单）。
fn pending_preview_key(pk: &PendingKey) -> Option<PreviewKey> {
    match pk {
        PendingKey::Char(' ') => Some(PreviewKey::Space),
        PendingKey::Char('1') => Some(PreviewKey::Digit1),
        PendingKey::Char('2') => Some(PreviewKey::Digit2),
        PendingKey::Enter => Some(PreviewKey::Enter),
        PendingKey::Escape => Some(PreviewKey::Escape),
        _ => None,
    }
}

/// 上屏文本后台投递器（进程级单例）：把用户上屏的文本异步投递到 daemon
/// 的窗口级 AI 上下文会话，不阻塞输入线程。
fn commit_forwarder() -> &'static CommitForwarder {
    static FWD: OnceLock<CommitForwarder> = OnceLock::new();
    FWD.get_or_init(CommitForwarder::start)
}

impl VerbaIMKController {
    /// 把上屏文本推入当前窗口的 AI 上下文会话（异步，失败静默重试）。
    fn forward_commit_to_context(&self, text: String) {
        // 敏感字段（密码框等）不上屏外发：检测到安全输入即跳过。
        let is_secure = self
            .ivars()
            .client
            .borrow()
            .as_ref()
            .map(|c| client_is_secure(c))
            .unwrap_or(false);
        if is_secure {
            log::info!("[VerbaIMK] 安全输入字段，跳过上屏上下文投递");
            return;
        }
        let session_id = self.ivars().session_id.get();
        let session_key = self.ivars().session_key.borrow().clone();
        let session = if session_key.is_empty() {
            LlmSession::legacy(session_id)
        } else {
            LlmSession::window(session_id, &session_key)
        };
        commit_forwarder().push(session, text);
    }

    /// 当前页候选（candidates / candidates: 共用）。
    fn current_candidates(&self) -> Option<Retained<NSArray<NSString>>> {
        let ivars = self.ivars();
        let page = ivars.page.get();
        let page_candidates = page_slice(
            &ivars.candidates.borrow(),
            page,
            CompositionMachine::PINYIN_PAGE_SIZE,
        );
        let items: Vec<Retained<NSString>> = page_candidates
            .iter()
            .map(|s| NSString::from_str(s))
            .collect();
        Some(NSArray::from_retained_slice(&items))
    }

    /// 更新 IMK client，并在必要时重算窗口级 session_key。
    /// `force_new_fallback`：activate 边界为 true；若拿不到真实窗口身份，
    /// 分配新的 input-session token，避免 controller 跨窗口复用时共享历史。
    fn set_client(&self, sender: Option<&AnyObject>, force_new_fallback: bool) {
        if let Some(s) = sender {
            let client_ptr = s as *const AnyObject as usize;
            let cached_client = self.ivars().session_key_client.get();
            let need_refresh = force_new_fallback || cached_client != client_ptr;
            if need_refresh && !self.ivars().session_key_refreshing.get() {
                struct RefreshGuard<'a>(&'a Cell<bool>);
                impl Drop for RefreshGuard<'_> {
                    fn drop(&mut self) {
                        self.0.set(false);
                    }
                }
                self.ivars().session_key_refreshing.set(true);
                let _guard = RefreshGuard(&self.ivars().session_key_refreshing);
                let fallback = compose_macos_fallback_key(alloc_session_token());
                let slot = Cell::new(None::<Option<(i32, u32)>>);
                self.host_call("session_key.refresh", || {
                    slot.set(Some(window_identity_for_client(s)));
                });
                let identity = slot.take().unwrap_or(None);
                let (key, is_window) = match identity {
                    Some((pid, w)) => (compose_macos_window_key(pid, w), true),
                    None => (fallback, false),
                };
                self.ivars().session_key_client.set(client_ptr);
                self.ivars().session_key_is_window.set(is_window);
                // 窗口关闭检测：旧会话的窗口身份已不在窗口列表 = 窗口已关闭，
                // 结束并删除该会话（隐私边界：窗口关闭 = 上下文销毁）。
                let old_identity = *self.ivars().session_identity.borrow();
                let old_key = self.ivars().session_key.borrow().clone();
                if let Some((old_pid, old_w)) = old_identity {
                    if old_key != key && !cg_window_exists(old_pid, old_w) {
                        let sid = self.ivars().session_id.get();
                        commit_forwarder().push_end(LlmSession::window(sid, &old_key));
                        log::info!("[VerbaIMK] 旧窗口已关闭，结束会话: {old_key}");
                    }
                }
                *self.ivars().session_identity.borrow_mut() = identity;
                let changed = old_key != key;
                *self.ivars().session_key.borrow_mut() = key.clone();
                if changed {
                    log::info!("[VerbaIMK] window session_key={key} (window={is_window})");
                }
            }
            // SAFETY: sender 是 IMK 会话客户端对象，保留以跨回调使用。
            let retained = unsafe { Retained::retain(s as *const AnyObject as *mut AnyObject) }
                .expect("client 有效");
            self.ivars().client.borrow_mut().replace(retained);
        }
    }

    /// 应用状态机动作：提交 / 标记 / 候选窗 / LLM 调度。
    fn apply_action(&self, action: Action) -> bool {
        match action {
            Action::None => true,
            Action::CommitImmediate(text) => {
                self.commit(&text);
                // 上屏文本进窗口级 AI 上下文会话（AI 结果上屏走 CommitResult，
                // 已作为 assistant 轮入历史，此处不重复投递）。
                self.forward_commit_to_context(text);
                true
            }
            Action::CommitResult { text } => {
                // Streaming/ResultReady 提前 Enter：停流并停表，避免空转。
                self.cancel_stream();
                self.invalidate_timer();
                self.commit(&text);
                true
            }
            Action::EnterPrompt { preedit } | Action::UpdatePrompt { preedit } => {
                // `e`/退格离开结果浮层回提示词编辑：残留的 AI 结果面板若不
                // 收起，其条目被点击（candidateSelected → feed_char('1')）
                // 会把数字字面量插进提示词。
                if !self.ivars().machine.borrow().ai_previewing() && self.ai_result_panel_showing()
                {
                    self.ivars().candidates.borrow_mut().clear();
                    self.hide_candidate_window();
                }
                self.set_marked(&preedit);
                true
            }
            Action::UpdateResult { preedit, body } => {
                // 流式增量：组合只持**短状态串**（set_marked 去重——此前每
                // chunk 整串 setMarkedText 长 preedit，既挤爆窄组合又每 chunk
                // 一次宿主往返）；全文经 show_ai_result 截断进候选面板。
                self.set_marked(&preedit);
                let phase = self
                    .ivars()
                    .machine
                    .borrow()
                    .result_phase()
                    .unwrap_or(ResultPhase::Streaming);
                self.show_ai_result(&body, phase);
                true
            }
            Action::UpdatePinyin {
                preedit,
                candidates,
                page,
                // 选中下标由 IMKCandidates 系统组件自行管理，macOS 端暂不消费。
                selected: _selected,
                llm_request,
            } => {
                // 在途空候选不覆盖面板数据源：updateComposition 会按 candidates:
                // 重绘面板，逐击键清空会让面板内容空闪（候选窗一闪一闪的根因）。
                // 真实收起只发生在提交/清空组合/会话切换。
                if !candidates.is_empty() {
                    self.ivars().candidates.borrow_mut().clone_from(&candidates);
                }
                self.ivars().page.set(page);
                if let Some(req) = llm_request {
                    self.start_candidates(req);
                }
                self.set_marked(&preedit);
                // 状态机同步候选先展示，Rime 结果到达后 feed_candidates_event 再刷新。
                self.refresh_candidate_window();
                true
            }
            Action::StartCapture => {
                // 裸 `//` + Tab：与点击悬浮气泡同一入口（v2，用户裁定
                // 「双斜杠+tab 触发 llm 调用」）——结束组合后经
                // float_panel::on_bubble_click 走 float-run（截前台窗口 →
                // OCR → drain 侧拼 float_button_prompt 模板 → start_llm
                // 流式预览）。`///` 选区 OCR 绑定随 v2 下线。
                dbg_log("StartCapture fired");
                self.set_marked("");
                self.reset();
                crate::float_panel::on_bubble_click();
                true
            }
            Action::StartLlm { prompt, system } => {
                // 多模态命令路由统一走 core commands::parse_ai_command
                // （与 Windows 同一份判定；此前 macOS 完全没有命令路由，
                // 同一 `//截图` 在两端行为分叉，且结果浮层的重试也依赖此
                // 收口还原命令语义）。
                let cmd = parse_ai_command(prompt.trim());
                match cmd {
                    // `//朗读 <文本>`：spawn verba-trigger speak（daemon TTS
                    // 合成 + 播放，不落盘文本）。
                    AiCommand::Tts { text } => {
                        dbg_log(&format!("朗读命令: text={text}"));
                        self.set_marked("");
                        self.reset();
                        spawn_trigger_speak(text);
                        true
                    }
                    // `//短语 名称`：查配置直插 + 剪贴板；未命中（无配置/
                    // 无此名称）按普通生成兜底（与 Windows 一致）。
                    AiCommand::Phrase { name } => match lookup_phrase(&name) {
                        Some(text) => {
                            dbg_log(&format!("插入快捷短语: {name}"));
                            set_clipboard_text_quiet(&text);
                            self.set_marked("");
                            self.reset();
                            self.commit(&text);
                            true
                        }
                        None => {
                            self.start_llm(prompt, system, false, None);
                            true
                        }
                    },
                    // `//截图` / `//听写`：spawn verba-trigger（stdout 文本经
                    // ocr_result_slot 进 OCR 预览，与 `///` 同管线）。
                    AiCommand::FullScreenOcr | AiCommand::Asr => {
                        let sub = if matches!(cmd, AiCommand::Asr) {
                            "asr"
                        } else {
                            "ocr"
                        };
                        dbg_log(&format!("触发命令: {sub}"));
                        self.set_marked("");
                        self.reset();
                        spawn_trigger_capture(sub);
                        true
                    }
                    // `//看图`：与 Windows 对齐——抓光标上方区域（取不到
                    // 光标回退主屏全屏）→ PNG 交给当前 LLM 做 vision。
                    AiCommand::Vision => {
                        let vision_rect = self.vision_rect_for_client();
                        self.start_llm(prompt, system, true, vision_rect);
                        true
                    }
                    // 普通生成与 daemon 命令（`//new`/`//会话`——前端不得
                    // 拦截，原样送 daemon）。
                    AiCommand::Llm => {
                        self.start_llm(prompt, system, false, None);
                        true
                    }
                }
            }
            Action::StartRewrite { content } => {
                dbg_log(&format!(
                    "apply StartRewrite len={}",
                    content.chars().count()
                ));
                // `//<内容>` + Tab：改写管道（与 Windows 同一套固定系统提示词，
                // 常量收口在 verba-core）。流式结果沿用 Streaming/ResultReady
                // 通道（Enter 上屏）。
                self.start_llm(content, Some(REWRITE_SYSTEM_PROMPT.to_owned()), false, None);
                true
            }
            Action::OcrPreview { text } => {
                dbg_log(&format!("apply OcrPreview len={}", text.chars().count()));
                // OCR 预览候选窗：复用 IMKCandidates 数据源（单候选=识别文本），
                // 数字 1/Enter/Esc 由 input_text 的 ocr_preview 拦截路由。
                // 候选显示截断（真机踩坑：大段识别文本 1059 字符塞单行候选，
                // 渲染不可读、用户以为没出字）；提交/剪贴板仍是全文——
                // 全文在 ocr_preview 槽位，pick 路径从槽位取。
                *self.ivars().ocr_preview.borrow_mut() = Some(text.clone());
                let mut disp: String = text.chars().take(40).collect();
                if text.chars().count() > 40 {
                    disp.push('…');
                }
                *self.ivars().candidates.borrow_mut() = vec![disp];
                self.ivars().page.set(0);
                self.set_marked("");
                self.refresh_candidate_window();
                true
            }
            Action::RewriteReady { rewritten, source } => {
                dbg_log(&format!(
                    "apply RewriteReady rewritten_len={} source_len={}",
                    rewritten.chars().count(),
                    source.chars().count()
                ));
                // 改写对照预览：候选窗双候选（1=改写 2=原文），数字选字由
                // input_text 的 rewrite_preview 拦截路由处理。组合标记清空
                // （预览期间不显示流式文本，候选窗即预览）。
                // 停表：流已终结（本臂此前只被 feed_stream_event 内联处理、
                // 从未真正走到；统一派发后成为活路径，保留原路径停表行为）。
                // core 同步进对照预览态（与 Windows 同构）：ai_preview 撤销
                // （互斥），旁路直喂的 r/e 从 feed_char 得 None，不再误触
                // 重试/改提示词（独立复审 P2——此前只写前端槽，core 停在
                // ResultReady 且浮层态仍武装）。
                let armed = self
                    .ivars()
                    .machine
                    .borrow_mut()
                    .begin_rewrite_preview(rewritten.clone(), source.clone());
                if !armed {
                    // 迟到的 RewriteReady：core 已离开结果就绪态（Idle/Prompt/
                    // OCR 预览…），必须整条丢弃——既不写前端镜像槽也不弹对照
                    // 预览窗。否则陈旧对照预览会重新接管按键，吞掉下一次
                    // 空格/回车并上屏陈旧文本（真机 2026-09-18 首次候选不上屏）。
                    dbg_log("apply RewriteReady 被拒（core 非 ResultReady），已丢弃");
                    return true;
                }
                *self.ivars().rewrite_preview.borrow_mut() =
                    Some((rewritten.clone(), source.clone()));
                *self.ivars().candidates.borrow_mut() = vec![rewritten, source];
                self.ivars().page.set(0);
                self.set_marked("");
                self.refresh_candidate_window();
                self.invalidate_timer();
                true
            }
            Action::ResultReady { text } => {
                // 就绪：组合换短状态串（不再塞全文），面板换就绪提示
                // （Enter/空格/1 上屏、r 重试、e 改提示词）。
                let preedit = self.ivars().machine.borrow().preedit();
                self.set_marked(&preedit);
                self.show_ai_result(&text, ResultPhase::Ready);
                self.invalidate_timer();
                true
            }
            Action::Cancel => {
                // 旁路 Cancel（重入窗重放 Esc、浮层取消）也要清前端预览槽：
                // clear_composition 会收面板，但 rewrite/ocr 前端槽不清则残留
                // （core 侧已被 feed_escape 的 clear_composition_state 清掉，
                // 两源失同步正是复审 P2 要消灭的形态）。
                self.clear_previews();
                self.cancel_stream();
                self.invalidate_timer();
                self.clear_composition();
                true
            }
            Action::LlmFailed { message } => {
                // 失败浮层保留（core 已入 Failed 态并保留 last_request：
                // Enter/`r` 重试、`e` 改提示词）——**不清组合、不收面板**，
                // 清掉即「错误一闪而过、按 r 无反应」（与 Windows 前端同款
                // 修复，计划风险 5；body 为已生成的部分结果，失败于首块前
                // 为空——面板只剩重试提示条）。
                log::warn!("[VerbaIMK] LLM 失败: {message}");
                let (body, preedit) = {
                    let m = self.ivars().machine.borrow();
                    (m.result().to_owned(), m.preedit())
                };
                self.set_marked(&preedit);
                // 首块前失败时 result 为空：把 daemon 的可执行错误显示在
                // 面板上，避免只剩一条空失败提示（与 Windows 共用 core 策略）。
                self.show_ai_result(failure_overlay_body(&body, &message), ResultPhase::Failed);
                self.invalidate_timer();
                true
            }
        }
    }

    /// 包裹会对客户端/面板发起同步 XPC 的调用段（setMarkedText/insertText/
    /// updateComposition/show：IMK 面板定位 attributesForCharacterIndex 是
    /// 等待回复时**泵运行循环**的）。深度 >0 期间的重入源有三处，全部有闸：
    /// ① 嵌套触发的 drain 定时器直接跳过（退栈后下一 tick 补排）；
    /// ② `input_text` 入口把可处理的键转入 `pending_keys` 待补放队列；
    /// ③ `candidate_selected` 的抢点选字同样入队。
    /// 否则会在 XPC 等待中再次对同一客户端发起调用（重入），抛
    /// NSInvalidArgumentException 且在异常清理途中 abort（真机崩溃
    /// 00:18/00:19：16ms 定时器使重入窗口较 50ms 版放大 3 倍后撞上）。
    /// ObjC 异常就地捕获记录（沿用 catch_void），不让外层 Rust 帧展开。
    fn host_call(&self, label: &str, f: impl FnOnce()) {
        /// RAII：无论正常返回还是 panic（Rust 层）都把重入深度退栈——
        /// 深度一旦泄漏，drain 定时器会永久跳过，输入法整体哑火。
        struct DepthGuard<'a>(&'a Cell<usize>);
        impl Drop for DepthGuard<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get().saturating_sub(1));
            }
        }
        let d = &self.ivars().host_call_depth;
        d.set(d.get() + 1);
        let _guard = DepthGuard(d);
        catch_void(label, f);
    }

    /// 补放一条重入窗内积压的按键：与 `input_text` 的正常处理尾部同构
    /// （feed → apply）。空闲态 + 无动作的组合在入队时已被排除，这里不再
    /// 复判；补放路径的宿主调用同样全部走 `apply_action` 内部的 host_call
    /// 包装，再次被泵重入时新键会重新入队。
    fn replay_pending(&self, pk: PendingKey) {
        match pk {
            PendingKey::Paste(text) => {
                // 粘贴重放沿用 input_text 的合并查询逻辑；过滤后为空的粘贴
                // （纯表情等）此窗口内已按认领处理，仅记录不回传宿主。
                let mut applied = false;
                let mut last_candidate_req: Option<LlmCandidateRequest> = None;
                for ch in text.chars().filter(|&ch| is_pasteable_char(ch)) {
                    let mut action = self.ivars().machine.borrow_mut().feed_char(ch);
                    if let Action::UpdatePinyin { llm_request, .. } = &mut action {
                        last_candidate_req = llm_request.take();
                    }
                    self.apply_action(action);
                    applied = true;
                }
                if let Some(req) = last_candidate_req {
                    self.start_candidates(req);
                }
                if !applied {
                    dbg_log("replay paste: 过滤后无可入库字符");
                }
            }
            _ => {
                // 改写对照预览期间的重放键（独立复审 P2 旁路面）：预览键经
                // 统一 helper 路由（core 直喂 feed_char 会得 None 成死点）；
                // 其余键照常落 core——ResultReady + ai_preview=None → None，
                // 吞掉保护结果（与 Windows 正常路由同语义）。
                if self.ivars().machine.borrow().rewrite_previewing() {
                    if let Some(pk) = pending_preview_key(&pk) {
                        self.handle_rewrite_preview_key(pk);
                        return;
                    }
                }
                // OCR 预览重入队列：必须走与 input_text 同一 helper，否则
                // 直接喂 core 会由 OcrPreviewing 防御分支结束预览而丢掉文本。
                if self.ivars().machine.borrow().ocr_previewing()
                    && self.handle_ocr_preview_key(pending_key_to_imk_key(&pk))
                {
                    return;
                }
                let action = {
                    let mut m = self.ivars().machine.borrow_mut();
                    match pk {
                        PendingKey::Char(c) => m.feed_char(c),
                        PendingKey::Backspace => m.feed_backspace(),
                        PendingKey::Enter => m.feed_enter(),
                        PendingKey::Escape => m.feed_escape(),
                        PendingKey::PageUp => m.feed_page_up(),
                        PendingKey::PageDown => m.feed_page_down(),
                        PendingKey::ArrowUp => m.feed_arrow_up(),
                        PendingKey::ArrowDown => m.feed_arrow_down(),
                        PendingKey::Paste(_) => unreachable!("上方分支已处理"),
                    }
                };
                let _ = self.apply_action(action);
            }
        }
    }

    fn commit(&self, text: &str) {
        let client = self.ivars().client.borrow().clone();
        dbg_log(&format!("commit text={} client={}", text, client.is_some()));
        if let Some(client) = client {
            // 先清空标记文本，避免上屏后残留 preedit。
            let empty = NSString::from_str("");
            // SAFETY: client 是 IMK 输入会话客户端，setMarkedText/unmarkText/insertText
            // 均为 IMKInputText 非正式协议方法。
            // 注意：replacementRange 长度必须为 NSNotFound（而非 0）——现代 IMK
            // 的 _IPMDServerClientWrapperLegacy 会校验区间，{NSNotFound,0} 被当作
            // 非法丢弃，导致 insertText 静默失败、候选词不上屏（真机排查）。
            // setMarkedText 同样会经 updateComposition 触发 attributesForCharacterIndex
            // 的 XPC 往返泵运行循环，必须与 insertText 一样纳住重入窗口。
            self.host_call("commit.setMarkedText", || unsafe {
                let _: () = msg_send![
                    &client,
                    setMarkedText: &*empty,
                    selectionRange: NSRange::new(NSNotFound as NSUInteger, 0),
                    replacementRange: NSRange::new(NSNotFound as NSUInteger, NSNotFound as NSUInteger),
                ];
            });
            let ns = NSString::from_str(text);
            self.host_call("commit.insertText", || unsafe {
                let _: () = msg_send![
                    &client,
                    insertText: &*ns,
                    replacementRange: NSRange::new(NSNotFound as NSUInteger, NSNotFound as NSUInteger),
                ];
            });
        }
        self.ivars().candidates.borrow_mut().clear();
        self.ivars().page.set(0);
        self.ivars().candidate_pinyin.borrow_mut().take();
        *self.ivars().composed.borrow_mut() = String::new();
        self.hide_candidate_window();
    }

    fn set_marked(&self, text: &str) {
        // 相邻动作常重推同一 preedit（状态机 UpdatePinyin 已标记，候选事件
        // 融合后原样再推一遍）：文本未变则跳过 updateComposition 宿主往返。
        // 长句时整串 setMarkedText 代价可观（真机：长句候选「慢」的因素之一）。
        if *self.ivars().composed.borrow() == text {
            return;
        }
        // SAFETY: client 为 IMKInputController 公开只读属性，self 恒有效。
        let fc: Option<Retained<AnyObject>> = unsafe { msg_send![self, client] };
        dbg_log(&format!(
            "set_marked text={} framework_client={}",
            text,
            fc.is_some()
        ));
        *self.ivars().composed.borrow_mut() = text.to_owned();
        // SAFETY: updateComposition 为 IMKInputController 方法：取 composedString:
        // 并经 client setMarkedText: 发送，同时触发候选窗刷新。
        self.host_call("set_marked.updateComposition", || unsafe {
            let _: () = msg_send![self, updateComposition];
        });
    }

    /// 处理 OCR 预览中的按键；返回 true 表示该键已被消费。
    ///
    /// `input_text` 与 `replay_pending` 共用此实现，保证重入队列不会绕过
    /// 「Enter/空格/1 确认、Esc 取消、可打印非确认键先提交再继续」语义。
    fn handle_ocr_preview_key(&self, key: Option<ImkKey>) -> bool {
        // #105 item3 预览 TTL：过期预览先作废（前端槽位 + core），本键按
        // 正常路由处理——陈旧识别文本不得随本键误上屏。
        if self.ivars().machine.borrow().ocr_preview_ttl_expired() {
            let _ = self.clear_previews();
            self.ivars().machine.borrow_mut().end_ocr_preview();
            self.hide_candidate_window();
        }
        if self.ivars().ocr_preview.borrow().is_none() {
            return false;
        }
        dbg_log(&format!("OCR 预览拦截: key={key:?}"));
        if key == Some(ImkKey::Escape) {
            let _ = self.clear_previews();
            self.ivars().machine.borrow_mut().end_ocr_preview();
            self.hide_candidate_window();
            return true;
        }
        let text = self.ivars().ocr_preview.borrow().clone();
        let confirm = ocr_preview_confirm_key(key);
        let commit_first = confirm || ocr_preview_other_commits(key);
        let _ = self.clear_previews();
        self.ivars().machine.borrow_mut().end_ocr_preview();
        self.hide_candidate_window();
        if commit_first {
            if let Some(t) = text {
                dbg_log("OCR: 先安全上屏识别文本，再继续当前键");
                self.commit(&t);
            }
        }
        confirm
    }

    /// 清空 OCR/改写双预览槽并清空候选数据源；返回是否确有预览被清。
    ///
    /// 面板动作（hide/refresh）语义各调用点不同，由调用方在清槽后自行决定，
    /// 不并入本函数。此前双槽同清点散落在 input_text 的 caps/esc/选中/其他键
    /// 四个臂与 activate/deactivate，拷贝多份易漂移（审查 F6 收敛）。
    fn clear_previews(&self) -> bool {
        let had = self.ivars().ocr_preview.borrow().is_some()
            || self.ivars().rewrite_preview.borrow().is_some();
        let _ = self.ivars().rewrite_preview.borrow_mut().take();
        let _ = self.ivars().ocr_preview.borrow_mut().take();
        *self.ivars().candidates.borrow_mut() = Vec::new();
        had
    }

    /// AI 结果浮层（macOS 一期形态）：IMKCandidates 单列面板显示
    /// 「截断结果 + 阶段提示」两条候选。**提交语义全在 core**——
    /// Enter/空格/1/`r`/`e` 走 input_text 的 feed_* 正常路由（机器
    /// ResultReady/Failed 态按键由 core 分派），上屏取 machine.result()
    /// **全文**；显示截断仅限面板（显示截断、提交取全文——OCR 预览真机
    /// 1059 字符教训）。提示条目点击无语义（对应数字键在流态被 core 吞
    /// 掉），仅供阅读。多行 NSPanel 自绘浮层列二期（issue #89）。
    fn show_ai_result(&self, text: &str, phase: ResultPhase) {
        let items = ai_result_display_items(text, phase);
        // 去重：同一 drain 批内多条 chunk 连续刷新时显示串未变则跳过
        // updateCandidates 宿主往返（与 set_marked 去重同一动机）。
        if *self.ivars().candidates.borrow() == items {
            return;
        }
        *self.ivars().candidates.borrow_mut() = items;
        self.ivars().page.set(0);
        self.refresh_candidate_window();
    }

    /// 当前候选面板是否为 AI 结果浮层形态（末条为 result_hint 的阶段
    /// 提示文案；面板条目由 ai_result_display_items 构造）。仅在「离开
    /// 结果态回提示词编辑」时用于收起残留面板，不承担状态机职责。
    fn ai_result_panel_showing(&self) -> bool {
        let last = self.ivars().candidates.borrow().last().cloned();
        last.is_some_and(|s| {
            s == result_hint(ResultPhase::Streaming)
                || s == result_hint(ResultPhase::Ready)
                || s == result_hint(ResultPhase::Failed)
        })
    }

    /// 改写对照预览键统一处理（input_text 拦截 / 面板点击 / 重放三条路径
    /// 共用一份，独立复审 P2——分头实现必致按键语义漂移）：core
    /// feed_rewrite_preview 消费（上屏/取消）后前端槽位与面板同步收起
    /// （commit/clear_composition 不清前端预览槽）。
    fn handle_rewrite_preview_key(&self, pk: PreviewKey) {
        let action = self.ivars().machine.borrow_mut().feed_rewrite_preview(pk);
        if !self.ivars().machine.borrow().rewrite_previewing() {
            self.clear_previews();
            self.hide_candidate_window();
        }
        let _ = self.apply_action(action.unwrap_or(Action::None));
    }

    fn clear_composition(&self) {
        *self.ivars().composed.borrow_mut() = String::new();
        // 先清候选再推空组合：updateComposition 可能借 candidates: 数据源
        // 刷新候选面板，必须先让数据源为空，否则旧候选在清除后复现。
        self.ivars().candidates.borrow_mut().clear();
        self.ivars().page.set(0);
        self.hide_candidate_window();
        // SAFETY: updateComposition 发空组合 → client 标记文本清空；再 unmark 兜底。
        self.host_call("clear_composition.updateComposition", || unsafe {
            let _: () = msg_send![self, updateComposition];
        });
        if let Some(client) = self.ivars().client.borrow().clone() {
            // 显式清空标记文本兜底：现代 IMK 客户端包装器（
            // _IPMDServerClientWrapperLegacy）不响应 unmarkText——此前每次
            // deactivate 都抛 unrecognized selector（真机 OBJC-EXC 日志，
            // 宿主日志中的 NSInvalidArgumentException 即此），可能把残留
            // 标记态留在宿主导致拼音原文泄露上屏。setMarkedText:@"" 等效
            // 且为已验证路径（commit 同款参数）。
            let empty = NSString::from_str("");
            self.host_call("clear_composition.setMarkedEmpty", || unsafe {
                let _: () = msg_send![
                    &client,
                    setMarkedText: &*empty,
                    selectionRange: NSRange::new(NSNotFound as NSUInteger, 0),
                    replacementRange: NSRange::new(NSNotFound as NSUInteger, NSNotFound as NSUInteger),
                ];
            });
        }
    }

    /// 刷新候选窗：首次惰性创建 IMKCandidates，之后 update + show。
    /// 候选为空时不隐藏、保持原状：击键同步态 candidates 恒为空（Rime 查询
    /// 在途），若每击键都隐藏、结果到达再显示，候选窗会一闪一闪；真正的
    /// 收起只发生在提交/清空组合/会话切换（见 commit/clear_composition/
    /// deactivate_server）与「当前拼音查询终结且无候选」（feed_candidates_event）。
    /// IMK 候选窗需要控制器显式驱动（updateComposition 只推标记文本，
    /// 不会自动弹候选窗）。
    fn refresh_candidate_window(&self) {
        if self.ivars().candidates.borrow().is_empty() {
            return;
        }
        // SAFETY: drain 定时器 / 输入回调均在主线程。
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let mut ui = self.ivars().candidates_ui.borrow_mut();
        if ui.is_none() {
            // SAFETY: server 为 IMKInputController 的只读访问器。
            let server: Option<Retained<IMKServer>> = unsafe { msg_send![self, server] };
            let Some(server) = server else {
                log::warn!("[VerbaIMK] 无法获取 IMKServer，候选窗不可用");
                return;
            };
            // SAFETY: initWithServer:panelType: 的 server 必填（已判空）。
            let created = unsafe {
                IMKCandidates::initWithServer_panelType(
                    IMKCandidates::alloc(mtm),
                    Some(&server),
                    kIMKSingleColumnScrollingCandidatePanel as NSUInteger,
                )
            };
            let Some(c) = created else {
                log::warn!("[VerbaIMK] IMKCandidates 创建失败");
                return;
            };
            // 属性：按键先送控制器（默认面板优先——数字/退格被面板吞掉且不回调
            // candidateSelected:，导致选词失效）；字体加大、近不透明，改善默认样式。
            let font = NSFont::systemFontOfSize(17.0);
            let yes: Retained<NSNumber> =
                unsafe { msg_send![NSNumber::class(), numberWithBool: true] };
            let font_obj: &NSObject = &font;
            let yes_obj: &NSObject = &yes;
            // SAFETY: extern static 常量由系统框架提供，只读访问。
            let font_key: &NSObject = unsafe { NSFontAttributeName };
            let first_key: &NSObject = unsafe { IMKCandidatesSendServerKeyEventFirst };
            let obj_arr: [&NSObject; 2] = [font_obj, yes_obj];
            let key_arr: [&NSObject; 2] = [font_key, first_key];
            // SAFETY: arrayWithObjects:count: 指针指向有效对象数组，count=2。
            let objs: Retained<NSArray<NSObject>> = unsafe {
                msg_send![NSArray::<NSObject>::class(), arrayWithObjects: obj_arr.as_ptr(), count: 2]
            };
            let keys: Retained<NSArray<NSObject>> = unsafe {
                msg_send![NSArray::<NSObject>::class(), arrayWithObjects: key_arr.as_ptr(), count: 2]
            };
            // SAFETY: dictionaryWithObjects:forKeys: 两数组等长且键实现 NSCopying。
            let dict: Retained<NSDictionary> = unsafe {
                msg_send![
                    NSDictionary::<AnyObject, AnyObject>::class(),
                    dictionaryWithObjects: &*objs,
                    forKeys: &*keys
                ]
            };
            // SAFETY: 属性字典由本函数构造，键值类型正确。
            unsafe { c.setAttributes(Some(&dict)) };
            *ui = Some(c);
        }
        // 关键：把面板句柄 clone 出来后**立即释放 RefCell 借用**。下面的
        // host_call（updateCandidates / show）会**同步泵运行循环**——show 要向
        // 客户端查 markedRange；泵期间到达的按键会重入 input_text，而它开头就取
        // `candidates_ui.borrow()`。借用若跨过那次泵，就是
        // `already mutably borrowed` panic；该 panic 穿出 objc2 的 ObjC 异常
        // shim 会二次 panic（rust_drop_panic）→ **abort**。真机 2026-09-17 12:22
        // 的 SIGABRT 正是这个形状：打完第 1 个字母候选窗弹出、第 2 个字母必崩。
        let ui_ref = ui.as_ref().expect("刚创建").clone();
        drop(ui);
        // SAFETY: updateCandidates/show 为无前置条件的 UI 方法；候选数据源
        // 由本控制器实现（candidates / candidates:）。
        self.host_call("refresh.updateCandidates", || unsafe {
            ui_ref.updateCandidates()
        });
        // show 会向客户端同步查询面板定位（attributesForCharacterIndex，
        // XPC 泵运行循环）：已在展示则跳过——此前每击键 show 两次，嵌套
        // XPC 往返既拖慢长句刷新又放大重入窗口（见 host_call）。
        // SAFETY: isVisible 为 NSPanel/NSWindow 公开方法（同上，主线程访问）。
        if !unsafe { ui_ref.isVisible() } {
            self.host_call("refresh.show", || unsafe {
                ui_ref.show(kIMKLocateCandidatesBelowHint as NSUInteger)
            });
            // 面板窗口圆角校正：首个展示周期探针一次（IMKUIPanel 不在
            // IMKCandidates API 暴露范围，只能经 [NSApp windows] 找到）。
            if !self.ivars().panel_probe_done.replace(true) {
                style_candidate_panel(mtm);
            }
        }
    }

    /// 隐藏候选窗（提交/清空组合/会话切换时调用；无窗则空操作）。
    fn hide_candidate_window(&self) {
        // 同 refresh：先把句柄 clone 出来释放借用，再调会泵运行循环的 hide。
        let ui = self.ivars().candidates_ui.borrow().clone();
        if let Some(ui) = ui.as_ref() {
            // SAFETY: hide 为无前置条件的 UI 方法。
            self.host_call("hide", || unsafe { ui.hide() });
            // SAFETY: isVisible 为 NSPanel/NSWindow 公开方法（主线程）。
            let still = unsafe { ui.isVisible() };
            dbg_log(&format!(
                "hide_candidate_window after-hide isVisible={}",
                still
            ));
        }
    }

    // ---- LLM 流式 ----

    /// `//看图` 的截屏区域：优先光标上方（与 Windows eye 语义一致）；
    /// 取不到光标或 eye_enabled=false 时返回 None，worker 回退主屏全屏。
    fn vision_rect_for_client(&self) -> Option<(i32, i32, i32, i32)> {
        let cfg = verba_config::ConfigManager::new(verba_config::VerbaDirs::locate().ok()?)
            .load()
            .ok()?;
        if !cfg.eye_enabled {
            return None;
        }
        // 先把 Retained 从 RefCell clone 出来再进 host_call：client_caret_point
        // 会同步 XPC 并可能泵 runloop，重入 set_client 的 borrow_mut 不能与
        // 这里未释放的 borrow() 冲突（同 commit() 的既有模式）。
        let client = self.ivars().client.borrow().clone()?;
        let mut caret = None;
        self.host_call("vision.eye_rect", || {
            caret = client_caret_rect(&client);
        });
        let (x, y, w, h) = caret?;
        let dx = if w.is_finite() { w.max(0.0) } else { 0.0 };
        let main_bounds = unsafe { CGDisplayBounds(CGMainDisplayID()) };
        let main_h = main_bounds.size.height;
        // Cocoa 底左 → CG 顶左：x 不变，y 取 top/bottom 两条边。
        let caret_x = x + dx;
        let caret_top = main_h - (y + h);
        let caret_bottom = main_h - y;
        let work = display_work_area_containing((caret_x, caret_top), main_h)
            .unwrap_or_else(|| cg_bounds_work(main_bounds));
        Some(vision_rect_from_caret(
            caret_x.round() as i32,
            caret_top.round() as i32,
            caret_bottom.round() as i32,
            work,
            cfg.eye_width,
            cfg.eye_height,
            cfg.eye_offset_y,
        ))
    }

    fn start_llm(
        &self,
        prompt: String,
        system: Option<String>,
        vision: bool,
        vision_rect: Option<(i32, i32, i32, i32)>,
    ) {
        // 发送即反馈：标记文本立刻换成机器的短状态串（Streaming 态 =
        // 「✨ 生成中…」）——发送 → 首块的首 token 延迟内此前完全无反馈，
        // 用户以为没发出而习惯性再敲 Enter，空提交把提示词一并抹掉
        // （Windows 真机 2026-09-04「AI 没回复」根因；两端口径一致）。
        // set_marked 内部按内容去重，首块到达时的同串刷新零宿主往返。
        let status = self.ivars().machine.borrow().preedit();
        self.set_marked(&status);
        // 发送即占位：候选面板在**同一次按键处理**里同步出场——首个 token 到达
        // 前用户视线落在面板上，此前那里什么都没有（感知「什么都没发生」）。
        // 正文取 core 的 `PLACEHOLDER_RESULT_BODY`（与 Windows 同一常量，杜绝
        // 两端措辞漂移），状态行由 `ai_result_display_items` 取
        // `result_hint(Streaming)`。
        // 零延迟硬约束：本路径只做数据源赋值 + 一次面板刷新（与首块到达走同一
        // 条 `show_ai_result` / `refresh_candidate_window`），无 daemon IPC、无线
        // 程 spawn、无定时器；也**不为占位新增 `set_marked`**（组合串上面刚刷过
        // 一次，再刷多一次宿主往返且违背「占位不引入额外 composition update」）。
        // 顺序恒为 set_marked → 占位面板 → 发起流，同一栈内同步完成。
        // 首块覆盖占位：面板按显示串去重，占位串与首块串不同故不会被跳过；模型
        // 恰好只吐「…」时显示串相同而跳过刷新，面板内容本就正确，无副作用。
        dbg_log("AI 占位浮层（首 token 前）已同步显示");
        self.show_ai_result(PLACEHOLDER_RESULT_BODY, ResultPhase::Streaming);
        self.cancel_stream();
        let seq = LLM_SEQ.fetch_add(1, Ordering::SeqCst);
        self.ivars().active_stream.set(seq);
        let session_id = self.ivars().session_id.get();
        let session_key = self.ivars().session_key.borrow().clone();

        std::thread::spawn(move || {
            let mut client = match ipc::ensure_daemon() {
                Ok(c) => c,
                Err(e) => {
                    push_llm(seq, error_event(&format!("无法连接 daemon: {e}")));
                    // 启动失败也要清掉可能的取消登记，防 cancelled_seqs 滞留（复审 V8）
                    cancelled_seqs().lock().unwrap().remove(&seq);
                    return;
                }
            };
            let session = if session_key.is_empty() {
                LlmSession::legacy(session_id)
            } else {
                LlmSession::window(session_id, &session_key)
            };
            let image = if vision {
                match spawn_vision_shot(vision_rect) {
                    Ok(png) => Some(("image/png".to_owned(), png)),
                    Err(e) => {
                        // 截图失败必须是错误，不能静默降级成无图文本请求——
                        // 否则用户以为图已被分析。
                        push_llm(seq, error_event(&format!("//看图 截屏失败: {e}")));
                        cancelled_seqs().lock().unwrap().remove(&seq);
                        return;
                    }
                }
            } else {
                None
            };
            let image_ref = image.as_ref().map(|(m, d)| (m.as_str(), d.as_slice()));
            let id = match client.llm_start(
                &prompt,
                system.as_deref(),
                None,
                None,
                image_ref,
                session,
            ) {
                Ok(id) => id,
                Err(e) => {
                    push_llm(seq, error_event(&format!("LLM 启动失败: {e}")));
                    cancelled_seqs().lock().unwrap().remove(&seq);
                    return;
                }
            };
            // 图已交给 daemon（llm_start 返回）：及时释放 PNG，避免整个
            // 流式生成期驻留数 MB 截图缓冲。
            drop(image);
            daemon_ids().lock().unwrap().insert(seq, id);
            // 启动期间已被取消（cancel_stream 在 llm_start 返回前执行，daemon id
            // 尚不可知）：检查取消登记，立即取消并退出，避免空转。
            if cancelled_seqs().lock().unwrap().remove(&seq) {
                daemon_ids().lock().unwrap().remove(&seq);
                let _ = client.llm_cancel(id);
                return;
            }
            loop {
                match client.next_event(id) {
                    Ok(evt) => {
                        let done = matches!(
                            evt.kind,
                            Some(stream_event::Kind::Final(_)) | Some(stream_event::Kind::Error(_))
                        );
                        push_llm(seq, evt);
                        if done {
                            break;
                        }
                    }
                    Err(e) => {
                        push_llm(seq, error_event(&format!("LLM 连接中断: {e}")));
                        break;
                    }
                }
            }
            // 流结束：清理 daemon id 映射（取消路径已 remove；此处防正常结束残留累积）。
            // 取消登记一并清掉——worker 退出后不再有消费者，迟到登记会永久滞留（复审 V8）。
            daemon_ids().lock().unwrap().remove(&seq);
            cancelled_seqs().lock().unwrap().remove(&seq);
        });

        self.ensure_timer();
    }

    /// 候选请求（拼音变更后请求本地 Rime 整句候选）。
    ///
    /// 单引擎（Rime）：候选只走本地 Rime。LLM 只在「输入 → 结果」的 AI 直输
    /// （回车触发 `StartLlm`）时调用，**打字过程不调用远程 LLM 做候选融合**。
    fn start_candidates(&self, req: LlmCandidateRequest) {
        self.maybe_reload_rime_schema();
        let schema = self.ivars().candidate_rime_schema.borrow().clone();
        self.start_rime_candidates(req.pinyin, schema);
    }

    /// 按配置 mtime 热更新 Rime 方案（避免每键读盘解析；未变则用缓存）。
    fn maybe_reload_rime_schema(&self) {
        let mtime = verba_config::VerbaDirs::locate().ok().and_then(|d| {
            let mgr = verba_config::ConfigManager::new(d);
            std::fs::metadata(mgr.path())
                .and_then(|m| m.modified())
                .ok()
        });
        if mtime == self.ivars().candidate_config_mtime.get() {
            return;
        }
        self.ivars().candidate_config_mtime.set(mtime);
        let schema = load_rime_schema();
        *self.ivars().candidate_rime_schema.borrow_mut() = schema;
    }

    /// Rime 候选查询（单引擎）：一次性请求 Rime 整句候选并压入候选队列。
    ///
    /// Rime 为 daemon 内本地同步查询，拼音变更即触发（不防抖），保证「整句候选」即时呈现，
    /// 与其它平台 engine=rime 行为一致。请求结果经 `feed_candidates_event` 融合/去重。
    fn start_rime_candidates(&self, pinyin: String, schema: String) {
        // Rime 为本地同步查询，无 daemon 侧 token 注册（无需取消）；
        // 旧候选回流由 seq 过滤防住（feed_candidates_event 只消费本控制器
        // 当前 active_candidates 序号的事件）。查询由全局单例 worker 执行
        // （只查最新拼音，见 ensure_cand_worker）。
        let seq = LLM_SEQ.fetch_add(1, Ordering::SeqCst);
        self.ivars()
            .candidate_pinyin
            .borrow_mut()
            .replace(pinyin.clone());
        // 被取代的旧候选请求序号入废弃集：其迟到响应（首次部署可达数秒，慢于
        // 击键间隔）在 drain 丢弃，不再永久滞留全局队列（复审 V7-b）。
        let old_cand = self.ivars().active_candidates.replace(seq);
        record_dead(self.ivars(), old_cand);
        self.ensure_timer();

        let (lock, cv) = cand_slot();
        *lock.lock().unwrap() = Some(CandRequest {
            seq,
            pinyin,
            schema,
        });
        cv.notify_one();
        ensure_cand_worker();
    }

    fn feed_stream_event(&self, evt: StreamEvent) {
        let action = match evt.kind {
            Some(stream_event::Kind::Chunk(ch)) => {
                self.ivars().machine.borrow_mut().on_llm_chunk(&ch.text)
            }
            Some(stream_event::Kind::Final(_)) => {
                self.ivars().active_stream.set(0);
                self.ivars().machine.borrow_mut().on_llm_done()
            }
            Some(stream_event::Kind::Error(e)) => {
                self.ivars().active_stream.set(0);
                self.ivars().machine.borrow_mut().on_llm_error(&e.message)
            }
            _ => Action::None,
        };
        // 统一走 apply_action（单一派发点）：此前本函数自持一份
        // set_marked/clear_composition 处理，与 apply_action 的同名臂两处
        // 维护——「两处本该一致」的漂移温床（RewriteReady 曾因此漏接）。
        // ResultReady（结果浮层）/LlmFailed（失败保留，不清组合）的新语义
        // 只改 apply_action 一处。
        let _ = self.apply_action(action);
    }

    fn feed_candidates_event(&self, evt: StreamEvent) {
        let Some(kind) = evt.kind else {
            return;
        };
        dbg_log(&format!("feed_candidates_event {:?}", kind));
        match kind {
            stream_event::Kind::Candidates(c) => {
                // 优先用事件回显的拼音；为空时回退到本控制器记录的请求拼音。
                let pinyin = if c.pinyin.is_empty() {
                    let Some(py) = self.ivars().candidate_pinyin.borrow().clone() else {
                        return;
                    };
                    py
                } else {
                    c.pinyin.clone()
                };
                // 是否当前请求的拼音（决定 done+空候选时能否收起候选窗：迟到事件
                // 不得影响正在展示的新候选）。
                let is_current =
                    self.ivars().candidate_pinyin.borrow().as_deref() == Some(pinyin.as_str());
                let actions = self.ivars().machine.borrow_mut().on_llm_candidates(
                    &pinyin,
                    &c.candidates,
                    c.done,
                );
                // settle 可能按序重放整队暂缓意图，产出动作序列（盲窗队列化，
                // issue #87）——逐个按序执行。
                for action in actions {
                    match action {
                        Action::UpdatePinyin {
                            preedit,
                            candidates,
                            page,
                            ..
                        } => {
                            self.ivars().candidates.borrow_mut().clone_from(&candidates);
                            self.ivars().page.set(page);
                            self.set_marked(&preedit);
                            self.refresh_candidate_window();
                        }
                        // 在途期间暂缓的意图补执行：候选（或原文回退）提交上屏。
                        Action::CommitImmediate(text) => self.commit(&text),
                        Action::None => {}
                        other => log::debug!("[VerbaIMK] 候选事件产生其它动作: {other:?}"),
                    }
                }
                if c.done {
                    self.ivars().candidate_pinyin.borrow_mut().take();
                    self.ivars().active_candidates.set(0);
                    // 查询终结且「有效候选」为空时才收起窗口：状态机会在真实
                    // 候选全空时合成一条原文候选（英文原文本身即候选，手心/
                    // 搜狗惯例）——此时面板应展示它而非收起；只有连合成项都
                    // 没有的纯空态（如非组合输入错误回调）才收起防残影。
                    let effective_empty = self.ivars().candidates.borrow().is_empty();
                    if effective_empty && is_current {
                        self.hide_candidate_window();
                    }
                }
            }
            stream_event::Kind::Error(e) => {
                // Rime 候选错误：不再静默空白，落到日志便于排查（如 librime 未部署）。
                log::warn!("[VerbaIMK] Rime 候选错误: {}", e.message);
                let pinyin = self.ivars().candidate_pinyin.borrow_mut().take();
                self.ivars().active_candidates.set(0);
                // 错误也是查询终结：以空结果通知状态机，释放在途标记并补执行
                // 暂缓队列（无候选按原文回退），避免按键被吞（前端兜底是队列
                // 的唯一解药，静默 return 会让队列永不 settle）。
                if let Some(py) = pinyin {
                    let actions =
                        self.ivars()
                            .machine
                            .borrow_mut()
                            .on_llm_candidates(&py, &[], true);
                    for action in actions {
                        let _ = self.apply_action(action);
                    }
                }
            }
            _ => {}
        }
    }

    fn cancel_stream(&self) {
        let stream_seq = self.ivars().active_stream.get();
        let cand_seq = self.ivars().active_candidates.get();
        self.ivars().active_stream.set(0);
        self.ivars().active_candidates.set(0);
        self.ivars().candidate_pinyin.borrow_mut().take();
        // 废弃序号入集：取消的流补发的 Final、在途旧候选响应，之后才入队，
        // 届时 active_* 已归 0 无法匹配——按废弃集在 drain 丢弃（防全局队列
        // 无界滞留，复审 V7；原单槽 dead_stream 会被 start_llm 立即清 0 失效）。
        record_dead(self.ivars(), stream_seq);
        record_dead(self.ivars(), cand_seq);
        if stream_seq == 0 {
            return;
        }
        // 取本流的 daemon id：拿到则直接取消；拿不到（llm_start 未返回，daemon id
        // 尚不可知）才登记到取消集合（闭合启动竞态窗口）。避免对已完成流重复
        // 登记（防止 cancelled_seqs 泄漏）。
        let stream_id = daemon_ids().lock().unwrap().remove(&stream_seq);
        match stream_id {
            Some(stream_id) => {
                std::thread::spawn(move || {
                    if let Ok(mut c) = ipc::try_connect() {
                        let _ = c.llm_cancel(stream_id);
                    }
                });
            }
            None => {
                cancelled_seqs().lock().unwrap().insert(stream_seq);
            }
        }
    }

    fn reset(&self) {
        self.cancel_stream();
        self.ivars().machine.borrow_mut().feed_escape();
        self.ivars().candidates.borrow_mut().clear();
        self.ivars().page.set(0);
        *self.ivars().composed.borrow_mut() = String::new();
    }

    // ---- 主线程定时器 ----

    fn ensure_timer(&self) {
        if self.ivars().timer.borrow().is_some() {
            return;
        }
        // SAFETY: self 与 AnyObject 指向同一 ObjC 对象（NSObject 子类），
        // drainVerbaStream 选择器在本类上有效。
        let target: &AnyObject =
            unsafe { &*(self as *const VerbaIMKController as *const AnyObject) };
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                // 16ms（约 60Hz）：上一版 50ms 让候选刷新滞后整帧以上，
                // 感知为「候选框慢」。排空本身极轻（空队列快速路径）。
                0.016,
                target,
                sel!(drainVerbaStream),
                None,
                true,
            )
        };
        let run_loop = NSRunLoop::mainRunLoop();
        // SAFETY: timer 尚未被其它 run loop 持有。
        unsafe {
            run_loop.addTimer_forMode(&timer, NSDefaultRunLoopMode);
        }
        self.ivars().timer.borrow_mut().replace(timer);
    }

    fn invalidate_timer(&self) {
        if let Some(timer) = self.ivars().timer.borrow_mut().take() {
            // SAFETY: invalidate 可在任意线程/run loop 调用。
            let _: () = unsafe { msg_send![&timer, invalidate] };
        }
    }
}

/// 引导加载 IMK 局：注册控制器类并进入 AppKit 主循环。
///
/// IMKServer 从主 bundle 的 Info.plist 读取 `InputMethodServerControllerClass`，
/// 因此调用前必须确保 `VerbaIMKController` 类已注册。
pub fn run_server() -> ! {
    use objc2::MainThreadMarker;

    init_file_logger();
    log::info!("[Verba] IMK 服务启动");

    // SAFETY: run_server 只能从 main()（主线程）调用。
    let mtm = unsafe { MainThreadMarker::new_unchecked() };

    // 强制注册控制器类（define_class! 懒注册）。
    let _ = VerbaIMKController::class();

    let app = NSApplication::sharedApplication(mtm);
    let bundle = NSBundle::mainBundle();
    let bundle_id = bundle
        .bundleIdentifier()
        .unwrap_or_else(|| NSString::from_str("dev.verba.inputmethod.Verba"));
    let conn = NSString::from_str(&connection_name_for_bundle_id(&bundle_id.to_string()));

    // SAFETY: initWithName:bundleIdentifier: 从主 bundle Info.plist 解析控制器类。
    let _server = unsafe {
        IMKServer::initWithName_bundleIdentifier(IMKServer::alloc(), Some(&conn), Some(&bundle_id))
    }
    .expect("IMKServer 初始化失败");

    // SAFETY: IMK 输入法为 LSUIElement 应用，run() 阻塞直到进程退出。
    app.run();
    unreachable!("NSApplication run 不会返回")
}

/// 确保 IMKInputController 子类已注册（供外部引导复用）。
pub fn register() {
    let _ = VerbaIMKController::class();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_name_derives_from_bundle_id() {
        assert_eq!(
            connection_name_for_bundle_id("dev.verba.inputmethod.Verba"),
            "dev.verba.inputmethod.Verba_Connection",
            "IMK 连接名必须是 <CFBundleIdentifier>_Connection"
        );
    }

    #[test]
    fn selection_range_uses_utf16_units() {
        let text = "ni你🦀";
        assert_eq!(text.len(), 9, "UTF-8 字节数不是 selectionRange 的单位");
        assert_eq!(
            text.chars().count(),
            4,
            "Unicode scalar 数不是 selectionRange 的单位"
        );
        let range = selection_range_for(text);
        assert_eq!(range.location, 5, "selectionRange 使用 UTF-16 code unit");
        assert_eq!(range.length, 0);
    }

    #[test]
    fn ocr_commits_when_result_arrives_during_input() {
        // 输入回调期间到达必须直接上屏；空闲时保留预览确认。
        assert!(should_commit_ocr_during_input(true));
        assert!(!should_commit_ocr_during_input(false));
    }

    #[test]
    fn ocr_preview_confirmation_keys_include_space() {
        assert!(ocr_preview_confirm_key(Some(ImkKey::Char('1'))));
        assert!(ocr_preview_confirm_key(Some(ImkKey::Char(' '))));
        assert!(ocr_preview_confirm_key(Some(ImkKey::Enter)));
        assert!(!ocr_preview_confirm_key(Some(ImkKey::Char('2'))));
    }

    #[test]
    fn ocr_preview_other_printable_key_commits_first() {
        assert!(ocr_preview_other_commits(Some(ImkKey::Char('o'))));
        assert!(!ocr_preview_other_commits(Some(ImkKey::Char('2'))));
        assert!(!ocr_preview_other_commits(Some(ImkKey::Backspace)));
        assert!(!ocr_preview_other_commits(None));
    }

    #[test]
    fn pending_key_maps_to_shared_ocr_preview_route() {
        assert_eq!(
            pending_key_to_imk_key(&PendingKey::Char('o')),
            Some(ImkKey::Char('o'))
        );
        assert_eq!(
            pending_key_to_imk_key(&PendingKey::Enter),
            Some(ImkKey::Enter)
        );
        assert_eq!(
            pending_key_to_imk_key(&PendingKey::Backspace),
            Some(ImkKey::Backspace)
        );
        assert_eq!(
            pending_key_to_imk_key(&PendingKey::Paste("x".to_owned())),
            None
        );
    }

    #[test]
    fn classify_printable_char() {
        let s = NSString::from_str("a");
        assert_eq!(classify_key(Some(&s), 0), Some(ImkKey::Char('a')));
        let sp = NSString::from_str(" ");
        assert_eq!(classify_key(Some(&sp), 49), Some(ImkKey::Char(' ')));
    }

    #[test]
    fn classify_special_keys_by_keycode() {
        assert_eq!(classify_key(None, 51), Some(ImkKey::Backspace));
        assert_eq!(classify_key(None, 36), Some(ImkKey::Enter));
        assert_eq!(classify_key(None, 76), Some(ImkKey::Enter));
        assert_eq!(classify_key(None, 53), Some(ImkKey::Escape));
        assert_eq!(classify_key(None, 123), Some(ImkKey::PageUp));
        assert_eq!(classify_key(None, 124), Some(ImkKey::PageDown));
    }

    #[test]
    fn classify_unknown_keycode_is_none() {
        assert_eq!(classify_key(None, 96), None);
    }

    #[test]
    fn page_slice_paginates() {
        let all: Vec<String> = (1..=20).map(|i| format!("c{i}")).collect();
        assert_eq!(
            page_slice(&all, 0, 9),
            (1..=9).map(|i| format!("c{i}")).collect::<Vec<_>>()
        );
        assert_eq!(
            page_slice(&all, 2, 9),
            (19..=20).map(|i| format!("c{i}")).collect::<Vec<_>>()
        );
        assert!(page_slice(&all, 9, 9).is_empty());
    }

    #[test]
    fn selection_digit_maps_within_page() {
        assert_eq!(selection_digit(0, 0, 9), Some('1'));
        assert_eq!(selection_digit(8, 0, 9), Some('9'));
        assert_eq!(selection_digit(9, 1, 9), Some('1'));
        assert_eq!(selection_digit(17, 1, 9), Some('9'));
        assert_eq!(selection_digit(9, 0, 9), None);
        assert_eq!(selection_digit(0, 1, 9), None);
    }

    #[test]
    fn classify_filters_function_key_unicode() {
        // NSLeftArrowFunctionKey 等 0xF700..0xF8FF 不应作为可打印字符
        let fk = NSString::from_str("\u{F702}");
        assert_eq!(classify_key(Some(&fk), 123), Some(ImkKey::PageUp));
        // 控制字符不落字符分支
        let ctrl = NSString::from_str("\u{3}");
        assert_eq!(classify_key(Some(&ctrl), 0), None);
    }

    #[test]
    fn pinyin_machine_drives_actions() {
        let mut m = CompositionMachine::new();
        assert!(matches!(m.feed_char('n'), Action::UpdatePinyin { .. }));
        m.feed_char('i');
        // 单引擎 Rime：拼音态候选经 on_llm_candidates 异步注入。
        let _ = m.on_llm_candidates("ni", &["你".to_string()], true);
        let a = m.feed_char(' ');
        assert!(matches!(&a, Action::CommitImmediate(text) if text == "你"));
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn ai_trigger_enter_starts_llm() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        for c in "你好".chars() {
            m.feed_char(c);
        }
        assert!(matches!(m.feed_enter(), Action::StartLlm { .. }));
    }

    #[test]
    fn load_rime_schema_returns_schema() {
        // 读取失败/成功均返回非空 scheme；不 panic。
        let schema = load_rime_schema();
        assert!(!schema.is_empty());
    }

    #[test]
    fn pasteable_char_matches_classify_filter() {
        // 与 classify_key 字符过滤一致：控制字符与 NS*FunctionKey 私有区被拒
        assert!(is_pasteable_char('a'));
        assert!(is_pasteable_char('你'));
        assert!(is_pasteable_char(' '));
        assert!(!is_pasteable_char('\u{3}'));
        assert!(!is_pasteable_char('\u{F702}'));
        assert!(!is_pasteable_char('\u{F8FF}'));
    }

    #[test]
    fn vision_rect_from_caret_matches_windows_fit_semantics() {
        let main = (0, 0, 1440, 900);
        // caret line: top=490, bottom=510（高度 20）；上方 490 足够放 480。
        assert_eq!(
            vision_rect_from_caret(500, 490, 510, main, 640, 480, 0),
            (500, 10, 640, 480)
        );
        // 上方放不下 → 翻到光标行下方（bottom=120）。
        assert_eq!(
            vision_rect_from_caret(100, 100, 120, main, 640, 480, 0),
            (100, 120, 640, 480)
        );
        // 上下都放不下且上方空间更大 → 贴工作区顶部。
        assert_eq!(
            vision_rect_from_caret(500, 490, 510, main, 640, 480, 40),
            (500, 0, 640, 480)
        );
        // 副屏（右侧）按光标所在屏 clamp。
        assert_eq!(
            vision_rect_from_caret(2200, 490, 510, (1440, 0, 2880, 900), 640, 480, 0),
            (2200, 10, 640, 480)
        );
        // 副屏（左侧，负坐标）同样成立。
        assert_eq!(
            vision_rect_from_caret(-1000, 490, 510, (-1920, 0, 0, 1080), 640, 480, 0),
            (-1000, 10, 640, 480)
        );
    }

    #[test]
    fn caret_point_accepts_zero_width_and_rejects_invalid() {
        // NSTextView 零长度插入点：width=0 是正常的，x 取 origin.x。
        assert_eq!(
            caret_point_from_rect(2, 10.0, 20.0, 0.0, 14.0),
            Some((10.0, 34.0))
        );
        // 负 width（range 向左延伸）插入点取 origin.x，不拒绝。
        assert_eq!(
            caret_point_from_rect(2, 10.0, 20.0, -3.0, 14.0),
            Some((10.0, 34.0))
        );
        // 正 width：插入点在 range 末端。
        assert_eq!(
            caret_point_from_rect(2, 10.0, 20.0, 3.0, 14.0),
            Some((13.0, 34.0))
        );
        assert_eq!(
            caret_point_from_rect(NSNotFound as NSUInteger, 10.0, 20.0, 0.0, 14.0),
            None
        );
        assert_eq!(caret_point_from_rect(2, 10.0, 20.0, 0.0, 0.0), None);
    }

    #[test]
    fn macos_session_key_separates_windows_and_falls_back_per_session() {
        let a = compose_macos_window_key(123, 1);
        let b = compose_macos_window_key(123, 2);
        let c = compose_macos_window_key(456, 1);
        assert_ne!(a, b, "同进程不同窗口必须不同 key");
        assert_ne!(a, c, "不同进程窗口必须不同 key");
        assert!(a.starts_with("macos:"));
        assert_ne!(
            compose_macos_fallback_key(1),
            compose_macos_fallback_key(2),
            "窗口身份不可得时至少按 controller 拆会话，不跨窗口复用"
        );
    }

    #[test]
    fn cocoa_to_cg_point_flips_y_around_main_display() {
        assert_eq!(cocoa_to_cg_point((10.0, 20.0), 900.0), (10.0, 880.0));
        assert_eq!(cocoa_to_cg_point((0.0, 900.0), 900.0), (0.0, 0.0));
    }

    #[test]
    fn cg_bubble_to_cocoa_frame_inverts_y_with_size() {
        // 复审 F1：CG 顶左锚点（200,308,44×44，屏高 900）→ Cocoa frame
        // 原点（200, 900-308-44=548）；矩形上下沿与 CG 互逆。
        assert_eq!(
            cg_bubble_to_cocoa_frame((200, 308), 900.0, 44.0),
            (200.0, 548.0)
        );
        // 原点换算回 CG 顶左 = 自身（cocoa_to_cg 按点互逆）。
        assert_eq!(cocoa_to_cg_point((200.0, 592.0), 900.0), (200.0, 308.0));
        // 贴屏底（CG y 最大）：frame 原点 y 最小不越界。
        assert_eq!(
            cg_bubble_to_cocoa_frame((0, 900 - 44), 900.0, 44.0),
            (0.0, 0.0)
        );
    }

    #[test]
    fn session_id_unique_and_carries_process_salt() {
        // 高 32 位为进程盐（本次进程内恒定），低 32 位单调递增：同进程内
        // session_id 唯一，跨进程（IME 重启）盐不同 → 不与 daemon 侧旧历史槽碰撞
        // （复审 V4）。
        let a = alloc_session_id();
        let b = alloc_session_id();
        assert_ne!(a, b);
        assert_eq!(a >> 32, b >> 32, "同进程盐应一致");
        assert_eq!((a >> 32) as u32, process_salt());
        assert!((b as u32) > (a as u32), "低 32 位应递增");
    }

    /// #89 结果浮层面板条目：显示截断（上限+省略号）、空结果只剩提示、
    /// 短文本原样。「提交取全文」的一半由 core 的 feed_enter →
    /// CommitResult(全文) 测试钉住——显示截断只属于面板。
    #[test]
    fn ai_result_display_items_keep_failure_actionable_hint() {
        // 与 daemon vision_error_hint 同构：动作前置，40 字安全宽度内可见。
        let hint = "图片未识别：请换支持视觉的模型，或改用 `//截图` 走内置 OCR。当前模型 `deepseek-flash` 拒绝了图片输入（HTTP 400）。\n服务端返回：HTTP 400: model does not support image input";
        let items = ai_result_display_items(hint, ResultPhase::Failed);
        assert!(items[0].contains("//截图"), "失败提示必须保留可执行动作");
        assert_eq!(items[1], result_hint(ResultPhase::Failed));
    }

    #[test]
    fn ai_result_display_items_truncate_and_hint() {
        let items = ai_result_display_items(&"字".repeat(60), ResultPhase::Ready);
        assert_eq!(items.len(), 2, "截断结果 + 阶段提示两条");
        assert_eq!(
            items[0].chars().count(),
            AI_RESULT_DISPLAY_CHARS + 1,
            "截断到上限并补省略号"
        );
        assert!(items[0].ends_with('…'));
        assert_eq!(items[1], result_hint(ResultPhase::Ready));
        // 失败于首块前（空结果）：只剩提示条目
        assert_eq!(
            ai_result_display_items("", ResultPhase::Failed),
            vec![result_hint(ResultPhase::Failed).to_owned()]
        );
        // 短文本不截断
        assert_eq!(
            ai_result_display_items("你好", ResultPhase::Streaming)[0],
            "你好"
        );
    }

    /// 占位面板条目（发送瞬间、首 token 前）：占位正文 + Streaming 状态行两条，
    /// 且与首块条目**不同**——`show_ai_result` 按显示串去重，相同即跳过面板刷新
    /// （占位若与首块同串，首块到达时面板不刷新；不同才保证覆盖生效）。
    #[test]
    fn placeholder_items_differ_from_first_chunk() {
        let ph = ai_result_display_items(PLACEHOLDER_RESULT_BODY, ResultPhase::Streaming);
        assert_eq!(ph.len(), 2, "占位 = 正文 + 状态行");
        assert_eq!(ph[0], PLACEHOLDER_RESULT_BODY);
        assert!(!ph[0].is_empty(), "占位正文不得为空串");
        assert_eq!(ph[1], result_hint(ResultPhase::Streaming));
        assert_ne!(
            ph,
            ai_result_display_items("你好", ResultPhase::Streaming),
            "占位条目与首块条目必须不同（否则首块被去重跳过）"
        );
    }

    /// 回归（真机 2026-09-17 12:22 SIGABRT 家族）：host_call 闭包里的 Rust panic
    /// **绝不允许穿出**到 objc2 的 ObjC 异常 shim —— 那会在 `__cxa_end_catch` 里
    /// 二次 panic（`rust_drop_panic`）并把可恢复的 panic 升级成进程 abort。
    /// 这里直接钉住 `catch_void` 的吞咽语义。
    #[test]
    fn catch_void_swallows_rust_panic() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // 静音本次预期 panic 的输出
        let escaped = std::panic::catch_unwind(|| {
            catch_void("test.swallow", || panic!("boom"));
        });
        std::panic::set_hook(prev);
        assert!(
            escaped.is_ok(),
            "catch_void 必须吞掉 Rust panic，否则真机上会穿出 ObjC 异常帧 abort"
        );
    }
}
