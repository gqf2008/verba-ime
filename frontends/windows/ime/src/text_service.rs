//! TSF 文本服务：按键处理、组合管理、LLM 流式（经 daemon）。
//!
//! M1 实现说明（windows 0.62 绑定限制）：
//! - `ITfThreadMgr` 未导出 `AdviseSink`，故不挂 `ITfThreadMgrEventSink`；
//!   改为 Activate 时直接 `AdviseKeyEventSink`（TSF 绑定不要求 context），
//!   `OnKeyDown` 每次带回 `ITfContext`，写入共享状态供组合/定时器使用。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use verba_core::machine::{Action, CompositionMachine, MachineState};
use verba_protos::{stream_event, StreamEvent};
use windows::core::{implement, w, Interface, Ref, Result, PCWSTR};
use windows::Win32::Foundation::{FALSE, HINSTANCE, HWND, LPARAM, LRESULT, TRUE, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, GetKeyboardState, ToUnicodeEx, VK_BACK, VK_ESCAPE, VK_RETURN,
};
use windows::Win32::UI::TextServices::{
    ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfContext, ITfContextView,
    ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfTextInputProcessor,
    ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl,
    ITfThreadMgr,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, KillTimer, RegisterClassW,
    SetTimer, SetWindowLongPtrW, CREATESTRUCTW, GWLP_USERDATA, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_DESTROY, WM_NCCREATE, WM_TIMER, WNDCLASSW,
};

use crate::dll;
use crate::edit_session;
use crate::ipc;

const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 80;
const TIMER_WINDOW_CLASS: &str = "VerbaTimerWindow";
const CANDIDATE_POS_RETRY_TICKS: u32 = 15; // 80ms×15≈1.2s：GetTextExt 布局未就绪时锚点重试上限

/// 待重试的候选窗锚点（组合布局就绪后由定时器精确定位）。
struct CandidatePosRetry {
    context: ITfContext,
    attempts_left: u32,
}

/// 共享状态（TSF 线程独占；`chunks` 由流线程写入，用 Mutex 保护）。
pub struct TextServiceData {
    self_rc: RefCell<Option<Rc<TextServiceData>>>,
    pub threadmgr: RefCell<Option<ITfThreadMgr>>,
    pub clientid: Cell<u32>,
    pub context: RefCell<Option<ITfContext>>,
    pub composition: RefCell<Option<ITfComposition>>,
    pub machine: RefCell<CompositionMachine>,
    keysink: RefCell<Option<ITfKeyEventSink>>,
    keysink_advised: Cell<bool>,
    timer_hwnd: Cell<Option<HWND>>,
    pub chunks: Arc<Mutex<VecDeque<StreamEvent>>>,
    pub candidate_window: RefCell<Option<crate::candidate_window::CandidateWindow>>,
    /// 候选窗锚点重试（GetTextExt 返回 TS_E_NOLAYOUT 时，由定时器稍后重试精确定位）。
    candidate_pending_pos: RefCell<Option<CandidatePosRetry>>,
    pub stream_request_id: Arc<AtomicU64>,
    stream_thread: RefCell<Option<JoinHandle<()>>>,
    control: RefCell<Option<verba_ipc::VerbaClient>>,
}

impl TextServiceData {
    fn new() -> Self {
        Self {
            self_rc: RefCell::new(None),
            threadmgr: RefCell::new(None),
            clientid: Cell::new(0),
            context: RefCell::new(None),
            composition: RefCell::new(None),
            machine: RefCell::new(CompositionMachine::new()),
            keysink: RefCell::new(None),
            keysink_advised: Cell::new(false),
            timer_hwnd: Cell::new(None),
            chunks: Arc::new(Mutex::new(VecDeque::new())),
            candidate_window: RefCell::new(None),
            candidate_pending_pos: RefCell::new(None),
            stream_request_id: Arc::new(AtomicU64::new(0)),
            stream_thread: RefCell::new(None),
            control: RefCell::new(None),
        }
    }
}

#[implement(ITfTextInputProcessorEx, ITfTextInputProcessor)]
pub struct TextService {
    pub data: Rc<TextServiceData>,
}

impl TextService {
    pub fn new() -> Self {
        Self {
            data: Rc::new(TextServiceData::new()),
        }
    }
}

impl Default for TextService {
    fn default() -> Self {
        Self::new()
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, ptim: Ref<ITfThreadMgr>, tid: u32) -> Result<()> {
        let ptim = ptim.ok()?;
        tsf_activate(&self.data, ptim, tid)
    }

    fn Deactivate(&self) -> Result<()> {
        tsf_deactivate(&self.data)
    }
}

impl ITfTextInputProcessorEx_Impl for TextService_Impl {
    fn ActivateEx(&self, ptim: Ref<ITfThreadMgr>, tid: u32, _dwflags: u32) -> Result<()> {
        let ptim = ptim.ok()?;
        tsf_activate(&self.data, ptim, tid)
    }
}

/// 尝试挂载键盘 sink；成功则置位。Activate 时可能尚无前台上下文，定时器会持续重试。
fn try_advise_keysink(data: &Rc<TextServiceData>) -> bool {
    if data.keysink_advised.get() {
        return true;
    }
    let Some(tm) = data.threadmgr.borrow().as_ref().cloned() else {
        return false;
    };
    let Ok(km) = tm.cast::<ITfKeystrokeMgr>() else {
        return false;
    };
    let sink: ITfKeyEventSink = KeyEventSink::new(data.clone()).into();
    match unsafe { km.AdviseKeyEventSink(data.clientid.get(), &sink, true) } {
        Ok(()) => {
            *data.keysink.borrow_mut() = Some(sink);
            data.keysink_advised.set(true);
            log::info!("键盘 sink 已挂载");
            true
        }
        Err(e) => {
            log::warn!("键盘 sink 挂载失败: {e}");
            false
        }
    }
}

fn tsf_activate(data: &Rc<TextServiceData>, ptim: &ITfThreadMgr, tid: u32) -> Result<()> {
    *data.threadmgr.borrow_mut() = Some(ptim.clone());
    data.clientid.set(tid);

    // 挂键盘 sink：Activate 时可能尚无前台上下文导致失败，定时器会重试。
    let sink_ok = try_advise_keysink(data);

    if let Ok(ctx) = unsafe { ptim.GetFocus() }.and_then(|d| unsafe { d.GetBase() }) {
        *data.context.borrow_mut() = Some(ctx);
    }

    create_timer_window(data)?;
    // 候选窗（懒创建：失败不影响激活）
    if data.candidate_window.borrow().is_none() {
        if let Ok(cw) = crate::candidate_window::CandidateWindow::new() {
            *data.candidate_window.borrow_mut() = Some(cw);
        }
    }
    unsafe {
        log::info!(
            "Verba TSF 激活, clientid={tid} tid={} sink_immediate={}",
            GetCurrentThreadId(),
            sink_ok
        );
    }
    Ok(())
}

fn tsf_deactivate(data: &Rc<TextServiceData>) -> Result<()> {
    unsafe {
        log::info!("Verba TSF 停用, tid={}", GetCurrentThreadId());
    }
    if let Some(hwnd) = data.timer_hwnd.take() {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
    *data.self_rc.borrow_mut() = None;
    if data.keysink_advised.get() {
        if let Some(tm) = data.threadmgr.borrow().as_ref().cloned() {
            if let Ok(km) = tm.cast::<ITfKeystrokeMgr>() {
                unsafe {
                    let _ = km.UnadviseKeyEventSink(data.clientid.get());
                }
            }
        }
    }
    *data.keysink.borrow_mut() = None;
    data.keysink_advised.set(false);
    *data.threadmgr.borrow_mut() = None;
    *data.context.borrow_mut() = None;
    *data.composition.borrow_mut() = None;
    *data.candidate_pending_pos.borrow_mut() = None;
    *data.machine.borrow_mut() = CompositionMachine::new();
    Ok(())
}

// ---- KeyEventSink ----

#[implement(ITfKeyEventSink)]
struct KeyEventSink {
    data: Rc<TextServiceData>,
}

impl KeyEventSink {
    fn new(data: Rc<TextServiceData>) -> Self {
        Self { data }
    }
}

/// OnTestKeyDown 是否认领按键。
///
/// TSF 只在测试阶段返回 TRUE 时才调用 `OnKeyDown`（实测 Notepad-- 等 TSF 应用：
/// 一直返回 FALSE 会导致 `OnKeyDown` 永远不被调用，`//` 触发与直输全部失效）。
/// - `Idle`：只认领 `/` 触发键，其余按键直通应用（不吞键、不进 IME）。
/// - `PendingSlash` / `Prompt` / `Streaming` / `ResultReady`：认领全部可打印字符
///   与控制键（Enter/Backspace/Esc），避免 `/` 或提示词被吞/丢字符。
/// - 修饰键/导航键/功能键（无字符）一律不认领，保持应用正常导航。
pub fn should_claim_key(state: MachineState, vk: u32, lparam: u32) -> bool {
    let is_control = vk == VK_RETURN.0 as u32 || vk == VK_BACK.0 as u32 || vk == VK_ESCAPE.0 as u32;
    match state {
        MachineState::Idle => match get_char_for_vk(vk, lparam) {
            // 认领 `/`（AI 触发）与字母（进入拼音组合）
            Some(c) => c == '/' || c.is_ascii_alphabetic(),
            None => false,
        },
        MachineState::Pinyin => {
            if is_control {
                // Enter/Backspace/Esc：拼音态由状态机处理
                return true;
            }
            match get_char_for_vk(vk, lparam) {
                // 拼音态认领：字母（缓冲）、数字（选候选）、空格（选首选）、`/`（提交+AI）
                Some(c) => c == '/' || c.is_ascii_alphabetic() || c.is_ascii_digit() || c == ' ',
                None => false,
            }
        }
        MachineState::PendingSlash
        | MachineState::Prompt
        | MachineState::Streaming
        | MachineState::ResultReady => {
            if is_control {
                return true;
            }
            get_char_for_vk(vk, lparam).is_some()
        }
    }
}

impl ITfKeyEventSink_Impl for KeyEventSink_Impl {
    fn OnSetFocus(&self, fforeground: windows::core::BOOL) -> Result<()> {
        unsafe {
            log::info!(
                "KeySink OnSetFocus fg={} tid={}",
                fforeground.as_bool(),
                GetCurrentThreadId()
            );
        }
        Ok(())
    }
    fn OnTestKeyDown(
        &self,
        _pic: Ref<ITfContext>,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Result<windows::core::BOOL> {
        let vk = wparam.0 as u32;
        let state = self.data.machine.borrow().state();
        let claim = should_claim_key(state, vk, lparam.0 as u32);
        unsafe {
            log::info!(
                "OnTestKeyDown vk=0x{vk:02X} scan=0x{:02X} state={state:?} claim={claim} tid={}",
                (lparam.0 as u32 >> 16) & 0xff,
                GetCurrentThreadId()
            );
        }
        Ok(claim.into())
    }
    fn OnTestKeyUp(
        &self,
        _pic: Ref<ITfContext>,
        _wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Result<windows::core::BOOL> {
        Ok(FALSE)
    }
    fn OnKeyDown(
        &self,
        pic: Ref<ITfContext>,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Result<windows::core::BOOL> {
        unsafe {
            log::info!(
                "OnKeyDown vk=0x{:02X} scan=0x{:02X} tid={}",
                wparam.0 as u32,
                (lparam.0 as u32 >> 16) & 0xff,
                GetCurrentThreadId()
            );
        }
        if let Ok(ctx) = pic.ok() {
            *self.data.context.borrow_mut() = Some(ctx.clone());
        }
        handle_key_down(&self.data, wparam.0 as u32, lparam.0 as u32)
    }
    fn OnKeyUp(
        &self,
        _pic: Ref<ITfContext>,
        _wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Result<windows::core::BOOL> {
        Ok(FALSE)
    }
    fn OnPreservedKey(
        &self,
        _pic: Ref<ITfContext>,
        _rguid: *const windows::core::GUID,
    ) -> Result<windows::core::BOOL> {
        Ok(FALSE)
    }
}

// ---- CompositionSink ----

#[implement(ITfCompositionSink)]
struct CompositionSink {
    data: Rc<TextServiceData>,
}

impl CompositionSink {
    fn new(data: Rc<TextServiceData>) -> Self {
        Self { data }
    }
}

impl ITfCompositionSink_Impl for CompositionSink_Impl {
    fn OnCompositionTerminated(
        &self,
        _ecwrite: u32,
        _pcomposition: Ref<ITfComposition>,
    ) -> Result<()> {
        log::info!("OnCompositionTerminated —— 组合被应用终止，状态机重置为 Idle");
        hide_candidate_window(&self.data);
        *self.data.composition.borrow_mut() = None;
        *self.data.candidate_pending_pos.borrow_mut() = None;
        *self.data.machine.borrow_mut() = CompositionMachine::new();
        cancel_stream(&self.data);
        Ok(())
    }
}

// ---- 按键处理 ----

pub fn handle_key_down(
    data: &Rc<TextServiceData>,
    wparam: u32,
    lparam: u32,
) -> Result<windows::core::BOOL> {
    if data.context.borrow().is_none() {
        return Ok(FALSE);
    }
    let vk = wparam;
    let is_control = vk == VK_RETURN.0 as u32 || vk == VK_BACK.0 as u32 || vk == VK_ESCAPE.0 as u32;
    let ch = if is_control {
        None
    } else {
        get_char_for_vk(vk, lparam)
    };

    let mut machine = data.machine.borrow_mut();
    let state = machine.state();
    let action = if let Some(c) = ch {
        Some(machine.feed_char(c))
    } else if vk == VK_BACK.0 as u32 {
        Some(machine.feed_backspace())
    } else if vk == VK_RETURN.0 as u32 {
        Some(machine.feed_enter())
    } else if vk == VK_ESCAPE.0 as u32 {
        Some(machine.feed_escape())
    } else {
        None
    };
    // 空闲状态下 Enter/Backspace/Esc 透传给应用（不吞键）。
    if state == MachineState::Idle
        && (vk == VK_RETURN.0 as u32 || vk == VK_BACK.0 as u32 || vk == VK_ESCAPE.0 as u32)
    {
        return Ok(FALSE);
    }
    let Some(action) = action else {
        log::info!("key 未处理 vk=0x{vk:02X} ch={ch:?} state={state:?}");
        return Ok(FALSE);
    };
    log::info!("action={action:?} (state={state:?})");
    drop(machine);

    let Some(context) = data.context.borrow().as_ref().cloned() else {
        return Ok(FALSE);
    };
    apply_action(data, &context, action)?;
    Ok(TRUE)
}

fn get_char_for_vk(vk: u32, lparam: u32) -> Option<char> {
    unsafe {
        let mut kbd = [0u8; 256];
        if GetKeyboardState(&mut kbd).is_err() {
            return None;
        }
        let scan = (lparam >> 16) & 0xff;
        let mut chars = [0u16; 8];
        let layout = GetKeyboardLayout(0);
        let n = ToUnicodeEx(vk, scan, &kbd, &mut chars, 0, Some(layout));
        if n > 0 {
            char::from_u32(chars[0] as u32)
        } else {
            // 兜底：常用符号的直接映射（无 Shift），确保 // 触发键在任何布局下可识别
            oem_fallback_char(vk)
        }
    }
}

/// ToUnicodeEx 失败时的常用符号兜底（美国/多数布局的未加 Shift 字符）。
fn oem_fallback_char(vk: u32) -> Option<char> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VK_OEM_1, VK_OEM_2, VK_OEM_3, VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA,
        VK_OEM_MINUS, VK_OEM_PERIOD, VK_OEM_PLUS, VK_SPACE, VK_TAB,
    };
    if vk == VK_OEM_2.0 as u32 {
        Some('/')
    } else if vk == VK_OEM_PERIOD.0 as u32 {
        Some('.')
    } else if vk == VK_OEM_COMMA.0 as u32 {
        Some(',')
    } else if vk == VK_OEM_MINUS.0 as u32 {
        Some('-')
    } else if vk == VK_OEM_PLUS.0 as u32 {
        Some('=')
    } else if vk == VK_OEM_1.0 as u32 {
        Some(';')
    } else if vk == VK_OEM_3.0 as u32 {
        Some('`')
    } else if vk == VK_OEM_4.0 as u32 {
        Some('[')
    } else if vk == VK_OEM_5.0 as u32 {
        Some('\\')
    } else if vk == VK_OEM_6.0 as u32 {
        Some(']')
    } else if vk == VK_OEM_7.0 as u32 {
        Some('\'')
    } else if vk == VK_SPACE.0 as u32 {
        Some(' ')
    } else if vk == VK_TAB.0 as u32 {
        Some('\t')
    } else {
        None
    }
}

pub fn apply_action(
    data: &Rc<TextServiceData>,
    context: &ITfContext,
    action: Action,
) -> Result<()> {
    let clientid = data.clientid.get();
    match action {
        Action::None => Ok(()),
        Action::CommitImmediate(text) => {
            hide_candidate_window(data);
            // 先取走组合引用并释放 borrow，避免分支内 borrow_mut 冲突。
            let existing = data.composition.borrow_mut().take();
            if let Some(comp) = existing {
                edit_session::end_composition(context, clientid, &comp, &text)
            } else {
                edit_session::commit_text(context, clientid, &text)
            }
        }
        Action::EnterPrompt { preedit }
        | Action::UpdatePrompt { preedit }
        | Action::UpdateResult { preedit } => set_preedit(data, context, clientid, &preedit),
        Action::UpdatePinyin {
            preedit,
            candidates,
        } => {
            set_preedit(data, context, clientid, &preedit)?;
            update_candidate_window(data, context, &candidates);
            Ok(())
        }
        Action::StartLlm { prompt, system: _ } => {
            // 不要 set_preedit("")：把组合文本置空会触发应用终止组合
            // （OnCompositionTerminated → cancel_stream → 流式输出全丢，实测 Notepad--）。
            // 保持提示词组合，首个流式块到达时由 on_timer 的 UpdateResult 替换文本。
            start_llm(data, prompt);
            Ok(())
        }
        Action::ResultReady => Ok(()),
        Action::CommitResult { text } => {
            hide_candidate_window(data);
            if let Some(comp) = data.composition.borrow_mut().take() {
                edit_session::end_composition(context, clientid, &comp, &text)?;
            }
            data.stream_request_id.store(0, Ordering::SeqCst);
            Ok(())
        }
        Action::Cancel => {
            hide_candidate_window(data);
            if let Some(comp) = data.composition.borrow_mut().take() {
                edit_session::end_composition(context, clientid, &comp, "")?;
            }
            cancel_stream(data);
            Ok(())
        }
        Action::LlmFailed { message } => {
            hide_candidate_window(data);
            if let Some(comp) = data.composition.borrow_mut().take() {
                edit_session::end_composition(context, clientid, &comp, "")?;
            }
            log::warn!("LLM 失败: {message}");
            Ok(())
        }
    }
}

/// 更新候选窗：有候选则显示在组合光标下方，否则隐藏。
fn update_candidate_window(
    data: &Rc<TextServiceData>,
    context: &ITfContext,
    candidates: &[String],
) {
    let mut borrow = data.candidate_window.borrow_mut();
    let Some(cw) = borrow.as_mut() else {
        return;
    };
    if candidates.is_empty() {
        cw.hide();
        data.candidate_pending_pos.borrow_mut().take();
        return;
    }
    let mut ctrl =
        verba_candidate::CandidateWindowController::new(verba_candidate::Theme::default());
    ctrl.set_candidates(candidates.to_vec());
    ctrl.show();
    match caret_screen_pos(data, context) {
        Some(anchor) => {
            log::info!("候选窗显示 锚点=({},{},{})", anchor.0, anchor.1, anchor.2);
            data.candidate_pending_pos.borrow_mut().take();
            cw.update(&ctrl, anchor);
        }
        None => {
            // 布局未就绪（TS_E_NOLAYOUT）：先用视图屏幕区域粗定位显示，
            // 同时安排定时器重试精确定位。
            let fallback = view_screen_pos(context).unwrap_or((0, 0, 0));
            log::info!(
                "候选窗粗定位 锚点=({},{},{})，等待组合布局就绪后重试",
                fallback.0,
                fallback.1,
                fallback.2
            );
            cw.update(&ctrl, fallback);
            *data.candidate_pending_pos.borrow_mut() = Some(CandidatePosRetry {
                context: context.clone(),
                attempts_left: CANDIDATE_POS_RETRY_TICKS,
            });
        }
    }
}

/// 隐藏候选窗（提交/取消时）。
fn hide_candidate_window(data: &Rc<TextServiceData>) {
    data.candidate_pending_pos.borrow_mut().take();
    let mut borrow = data.candidate_window.borrow_mut();
    if let Some(cw) = borrow.as_mut() {
        cw.hide();
    }
}

/// 组合范围在屏幕上的坐标（候选窗锚点：组合下方）。
///
/// 注意：`ITfContextView::GetTextExt` 的第一个参数必须是**编辑会话的 edit cookie
/// （ec）**，不能传 clientid（实测传 clientid 返回 E_INVALIDARG 0x80070057）。
/// 因此锚点查询必须放进只读同步编辑会话内执行。
fn caret_screen_pos(data: &Rc<TextServiceData>, context: &ITfContext) -> Option<(i32, i32, i32)> {
    let Some(comp) = data.composition.borrow().as_ref().cloned() else {
        log::warn!("候选锚点无组合引用");
        return None;
    };
    match edit_session::query_composition_anchor(context, data.clientid.get(), &comp) {
        Ok(Some(anchor)) => {
            log::info!("组合锚点 rect=({},{},{})", anchor.0, anchor.1, anchor.2);
            Some(anchor)
        }
        Ok(None) => {
            // TS_E_NOLAYOUT(0x80040205)：组合刚更新、应用尚未重算布局，稍后由定时器重试。
            log::warn!("候选锚点会话未返回结果（布局未就绪 TS_E_NOLAYOUT=0x80040205），稍后重试");
            None
        }
        Err(e) => {
            log::warn!("候选锚点只读会话失败: {e}");
            None
        }
    }
}

/// 视图屏幕区域（粗定位兜底：候选窗出现在应用文本区左上角下方）。
fn view_screen_pos(context: &ITfContext) -> Option<(i32, i32, i32)> {
    unsafe {
        let view: ITfContextView = context.GetActiveView().ok()?;
        let rc = view.GetScreenExt().ok()?;
        Some((rc.left, rc.top, rc.top + 8))
    }
}

/// 定时器重试：组合布局就绪后把候选窗精确移动到组合锚点。
fn retry_candidate_pos(data: &Rc<TextServiceData>) {
    // 先取出重试状态（避免持有 RefMut 时再 take 造成二次可变借用）。
    let Some(mut p) = data.candidate_pending_pos.borrow_mut().take() else {
        return;
    };
    if p.attempts_left == 0 {
        log::warn!("候选窗精确定位重试次数耗尽，保持视图区域粗定位");
        return;
    }
    p.attempts_left -= 1;
    let context = p.context.clone();
    if let Some(anchor) = caret_screen_pos(data, &context) {
        log::info!(
            "候选窗重试定位成功 锚点=({},{},{})",
            anchor.0,
            anchor.1,
            anchor.2
        );
        if let Some(cw) = data.candidate_window.borrow_mut().as_mut() {
            cw.move_to(anchor);
        }
        return;
    }
    // 布局仍未就绪：放回重试状态，等待下一拍。
    *data.candidate_pending_pos.borrow_mut() = Some(p);
}

fn set_preedit(
    data: &Rc<TextServiceData>,
    context: &ITfContext,
    clientid: u32,
    text: &str,
) -> Result<()> {
    // 先取走引用并释放 borrow，避免 else 分支内 borrow_mut 冲突。
    let existing = data.composition.borrow_mut().take();
    if let Some(comp) = existing {
        let r = edit_session::update_composition(context, clientid, &comp, text);
        // 更新后必须放回引用：否则下次更新会新建组合，
        // 导致应用终止旧组合 → OnCompositionTerminated → 状态机被重置回 Idle。
        *data.composition.borrow_mut() = Some(comp);
        r
    } else {
        let sink: ITfCompositionSink = CompositionSink::new(data.clone()).into();
        let comp = edit_session::start_composition(context, clientid, &sink, text)?;
        *data.composition.borrow_mut() = Some(comp);
        Ok(())
    }
}

// ---- LLM 流式 ----

fn error_event(message: &str) -> stream_event::Kind {
    stream_event::Kind::Error(verba_protos::Error {
        code: 500,
        message: message.to_owned(),
    })
}

fn start_llm(data: &Rc<TextServiceData>, prompt: String) {
    let chunks = Arc::clone(&data.chunks);
    let request_id = Arc::clone(&data.stream_request_id);
    let handle = std::thread::spawn(move || {
        let mut client = match ipc::ensure_daemon() {
            Ok(c) => c,
            Err(e) => {
                push_chunk(&chunks, 0, error_event(&format!("无法连接 daemon: {e}")));
                return;
            }
        };
        let id = match client.llm_start(&prompt, None, None, None) {
            Ok(id) => id,
            Err(e) => {
                push_chunk(&chunks, 0, error_event(&format!("LLM 启动失败: {e}")));
                return;
            }
        };
        request_id.store(id, Ordering::SeqCst);
        loop {
            match client.next_event(id) {
                Ok(evt) => {
                    let done = matches!(
                        evt.kind,
                        Some(stream_event::Kind::Final(_)) | Some(stream_event::Kind::Error(_))
                    );
                    if let Ok(mut q) = chunks.lock() {
                        q.push_back(evt);
                    }
                    if done {
                        break;
                    }
                }
                Err(e) => {
                    push_chunk(&chunks, id, error_event(&format!("LLM 连接中断: {e}")));
                    break;
                }
            }
        }
    });
    *data.stream_thread.borrow_mut() = Some(handle);
}

fn push_chunk(chunks: &Arc<Mutex<VecDeque<StreamEvent>>>, id: u64, kind: stream_event::Kind) {
    if let Ok(mut q) = chunks.lock() {
        q.push_back(StreamEvent {
            id,
            kind: Some(kind),
        });
    }
}

fn cancel_stream(data: &Rc<TextServiceData>) {
    let id = data.stream_request_id.load(Ordering::SeqCst);
    if id == 0 {
        return;
    }
    let mut client = data.control.borrow_mut();
    if client.is_none() {
        *client = ipc::try_connect().ok();
    }
    if let Some(c) = client.as_mut() {
        let _ = c.llm_cancel(id);
    }
}

// ---- 定时器窗口 ----

/// # Safety
/// 标准窗口过程；`data_ptr` 指向经 Rc::into_raw 泄漏的 TextServiceData，窗口销毁时归还。
unsafe extern "system" fn timer_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = lparam.0 as *const CREATESTRUCTW;
            let data_ptr = (*cs).lpCreateParams as *mut TextServiceData;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, data_ptr as isize);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_TIMER => {
            let data_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TextServiceData;
            if !data_ptr.is_null() {
                (*data_ptr).on_timer();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(Some(hwnd), TIMER_ID);
            let data_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TextServiceData;
            if !data_ptr.is_null() {
                drop(Rc::from_raw(data_ptr));
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn create_timer_window(data: &Rc<TextServiceData>) -> Result<()> {
    if data.timer_hwnd.get().is_some() {
        return Ok(());
    }
    unsafe {
        let hmodule = if dll::module_handle().0.is_null() {
            // 测试/独立进程场景（DllMain 未执行）
            GetModuleHandleW(None)?
        } else {
            dll::module_handle()
        };
        // SAFETY: HMODULE 与 HINSTANCE 同为模块基址句柄，位模式一致。
        let hinstance: HINSTANCE =
            std::mem::transmute::<windows::Win32::Foundation::HMODULE, HINSTANCE>(hmodule);
        let class: Vec<u16> = TIMER_WINDOW_CLASS
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(timer_wndproc),
            hInstance: hinstance,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        let data_ptr = Rc::into_raw(data.clone()) as *mut std::ffi::c_void;
        *data.self_rc.borrow_mut() = Some(data.clone());
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            w!("VerbaTimer"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            Some(data_ptr),
        )?;
        SetTimer(Some(hwnd), TIMER_ID, TIMER_MS, None);
        data.timer_hwnd.set(Some(hwnd));
        Ok(())
    }
}

impl TextServiceData {
    pub fn on_timer(&self) {
        let Some(rc) = self.self_rc.borrow().as_ref().cloned() else {
            return;
        };
        // 持续重试挂载键盘 sink（Activate 时可能失败）
        try_advise_keysink(&rc);
        // 候选窗：组合布局就绪后重试精确定位
        retry_candidate_pos(&rc);

        let events: Vec<StreamEvent> = {
            let mut q = self.chunks.lock().unwrap();
            q.drain(..).collect()
        };
        if events.is_empty() {
            return;
        }
        let Some(context) = self.context.borrow().as_ref().cloned() else {
            return;
        };
        let clientid = self.clientid.get();

        let mut machine = self.machine.borrow_mut();
        for evt in events {
            match evt.kind {
                Some(stream_event::Kind::Chunk(ch)) => {
                    if let Action::UpdateResult { preedit } = machine.on_llm_chunk(&ch.text) {
                        let _ = set_preedit(&rc, &context, clientid, &preedit);
                    }
                }
                Some(stream_event::Kind::Final(_)) => {
                    machine.on_llm_done();
                    let result = machine.result().to_owned();
                    let _ = set_preedit(&rc, &context, clientid, &result);
                }
                Some(stream_event::Kind::Error(e)) => {
                    if matches!(machine.on_llm_error(&e.message), Action::LlmFailed { .. }) {
                        if let Some(comp) = self.composition.borrow_mut().take() {
                            let _ = edit_session::end_composition(&context, clientid, &comp, "");
                        }
                    }
                }
                None => {}
            }
        }
    }
}
