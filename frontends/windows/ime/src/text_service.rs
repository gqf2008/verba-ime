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
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, GetKeyboardState, ToUnicodeEx, VK_BACK, VK_ESCAPE, VK_RETURN,
};
use windows::Win32::UI::TextServices::{
    ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfContext, ITfKeyEventSink,
    ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfTextInputProcessor, ITfTextInputProcessorEx,
    ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl, ITfThreadMgr,
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

/// 共享状态（TSF 线程独占；`chunks` 由流线程写入，用 Mutex 保护）。
pub struct TextServiceData {
    self_rc: RefCell<Option<Rc<TextServiceData>>>,
    pub threadmgr: RefCell<Option<ITfThreadMgr>>,
    pub clientid: Cell<u32>,
    pub context: RefCell<Option<ITfContext>>,
    pub composition: RefCell<Option<ITfComposition>>,
    pub machine: RefCell<CompositionMachine>,
    keysink: RefCell<Option<ITfKeyEventSink>>,
    timer_hwnd: Cell<Option<HWND>>,
    pub chunks: Arc<Mutex<VecDeque<StreamEvent>>>,
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
            timer_hwnd: Cell::new(None),
            chunks: Arc::new(Mutex::new(VecDeque::new())),
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

fn tsf_activate(data: &Rc<TextServiceData>, ptim: &ITfThreadMgr, tid: u32) -> Result<()> {
    *data.threadmgr.borrow_mut() = Some(ptim.clone());
    data.clientid.set(tid);

    let km: ITfKeystrokeMgr = ptim.cast()?;
    let sink: ITfKeyEventSink = KeyEventSink::new(data.clone()).into();
    match unsafe { km.AdviseKeyEventSink(tid, &sink, true) } {
        Ok(()) => log::info!("AdviseKeyEventSink 成功"),
        Err(e) => log::warn!("AdviseKeyEventSink 失败: {e}"),
    }
    *data.keysink.borrow_mut() = Some(sink);

    if let Ok(ctx) = unsafe { ptim.GetFocus() }.and_then(|d| unsafe { d.GetBase() }) {
        *data.context.borrow_mut() = Some(ctx);
    }

    create_timer_window(data)?;
    log::info!("Verba TSF 激活, clientid={tid}");
    Ok(())
}

fn tsf_deactivate(data: &Rc<TextServiceData>) -> Result<()> {
    if let Some(hwnd) = data.timer_hwnd.take() {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
    *data.self_rc.borrow_mut() = None;
    if let Some(tm) = data.threadmgr.borrow().as_ref().cloned() {
        if let Ok(km) = tm.cast::<ITfKeystrokeMgr>() {
            unsafe {
                let _ = km.UnadviseKeyEventSink(data.clientid.get());
            }
        }
    }
    *data.keysink.borrow_mut() = None;
    *data.threadmgr.borrow_mut() = None;
    *data.context.borrow_mut() = None;
    *data.composition.borrow_mut() = None;
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

impl ITfKeyEventSink_Impl for KeyEventSink_Impl {
    fn OnSetFocus(&self, _fforeground: windows::core::BOOL) -> Result<()> {
        Ok(())
    }
    fn OnTestKeyDown(
        &self,
        _pic: Ref<ITfContext>,
        _wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Result<windows::core::BOOL> {
        Ok(FALSE)
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
        log::info!("OnKeyDown vk=0x{:02X}", wparam.0 as u32);
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
        *self.data.composition.borrow_mut() = None;
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
            None
        }
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
        Action::StartLlm { prompt, system: _ } => {
            set_preedit(data, context, clientid, "")?;
            start_llm(data, prompt);
            Ok(())
        }
        Action::ResultReady => Ok(()),
        Action::CommitResult { text } => {
            if let Some(comp) = data.composition.borrow_mut().take() {
                edit_session::end_composition(context, clientid, &comp, &text)?;
            }
            data.stream_request_id.store(0, Ordering::SeqCst);
            Ok(())
        }
        Action::Cancel => {
            if let Some(comp) = data.composition.borrow_mut().take() {
                edit_session::end_composition(context, clientid, &comp, "")?;
            }
            cancel_stream(data);
            Ok(())
        }
        Action::LlmFailed { message } => {
            if let Some(comp) = data.composition.borrow_mut().take() {
                edit_session::end_composition(context, clientid, &comp, "")?;
            }
            log::warn!("LLM 失败: {message}");
            Ok(())
        }
    }
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
        edit_session::update_composition(context, clientid, &comp, text)
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
        let Some(rc) = self.self_rc.borrow().as_ref().cloned() else {
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
