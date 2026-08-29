//! TSF 文本服务：按键处理、组合管理、LLM 流式（经 daemon）。
//!
//! M1 实现说明（windows 0.62 绑定限制）：
//! - `ITfThreadMgr` 未导出 `AdviseSink`，故不挂 `ITfThreadMgrEventSink`；
//!   改为 Activate 时直接 `AdviseKeyEventSink`（TSF 绑定不要求 context），
//!   `OnKeyDown` 每次带回 `ITfContext`，写入共享状态供组合/定时器使用。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::os::windows::process::CommandExt;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use verba_core::machine::{
    is_fullwidth_mapped_punct, Action, CompositionMachine, LlmCandidateRequest, MachineState,
};
use verba_protos::{stream_event, StreamEvent};
use windows::core::{implement, w, Interface, Ref, Result, PCWSTR};
use windows::Win32::Foundation::{FALSE, HINSTANCE, HWND, LPARAM, LRESULT, TRUE, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, GetKeyboardState, ToUnicodeEx, VK_BACK, VK_CONTROL, VK_DOWN,
    VK_ESCAPE, VK_M, VK_MENU, VK_NEXT, VK_O, VK_PRIOR, VK_RETURN, VK_S, VK_UP,
};

use crate::capture::capture_primary_screen;
use crate::play::play_audio;
use crate::record::record_seconds;
use windows::Win32::UI::TextServices::{
    IEnumTfDisplayAttributeInfo, ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl,
    ITfContext, ITfContextView, ITfDisplayAttributeInfo, ITfDisplayAttributeProvider,
    ITfDisplayAttributeProvider_Impl, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr,
    ITfTextInputProcessor, ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl,
    ITfTextInputProcessor_Impl, ITfThreadMgr,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, KillTimer, PostMessageW,
    RegisterClassW, SetTimer, SetWindowLongPtrW, CREATESTRUCTW, GWLP_USERDATA, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_DESTROY, WM_NCCREATE, WM_TIMER, WNDCLASSW,
};

use crate::dll;
use crate::edit_session;
use crate::ipc;

const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 80;
const TIMER_WINDOW_CLASS: &str = "VerbaTimerWindow";
const CANDIDATE_POS_RETRY_TICKS: u32 = 15; // 80ms×15≈1.2s：GetTextExt 布局未就绪时锚点重试上限
const CANDIDATE_REQ_DEBOUNCE_TICKS: u32 = 1; // 80ms：击键后短暂停顿即查 Rime（本地快）；过早的 320ms 防抖为远程 LLM 融合设计，单引擎 Rime 下导致候选框滞后于输入（不跟手）
/// 听写 / ASR 热键录音时长（秒）。
const ASR_RECORD_SECONDS: f32 = 3.0;

/// Win32 CREATE_NO_WINDOW 进程创建标志（0x08000000）：由 IME/TSF 上下文拉起
/// 子进程（daemon / verba-trigger）时绝不允许出现控制台窗口。三处 spawn 共用，
/// 勿再散落魔术数。
pub const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 全局自增会话序号（从 1 起）：进程内每个输入上下文独占一个 AI 多轮上下文会话，
/// daemon 按 session_id 分组隔离历史（架构审查会话维度 B4b）。
static SESSION_ID_SEQ: AtomicU64 = AtomicU64::new(1);

/// 每进程随机盐（惰性生成一次）。本 IME 是 in-proc COM DLL（InprocServer32），被
/// 加载进**每个应用进程**，而 daemon 是按用户单例、按 session_id 分组历史。用
/// 随机盐而非裸 pid：避免 pid 复用后新进程撞回旧进程的历史槽（复审 LOW——裸
/// pid 下撞槽会**继承**陈旧上下文而非覆盖）。盐含时间纳秒+pid 熵，跨进程（含
/// pid 复用）实际不碰撞。无 rand 依赖，同 name.rs 的本地熵方案（无需密码学强度，
/// 同用户同机本地隔离即可）。
fn process_salt() -> u32 {
    static SALT: OnceLock<u32> = OnceLock::new();
    // 本地熵实现统一收敛到 verba-ipc name::local_entropy_u64（复用评审：
    // 原三处内联 xorshift 实现合一，便于审计与保持一致）。
    *SALT.get_or_init(|| (verba_ipc::name::local_entropy_u64() >> 32) as u32)
}

/// 分配全局唯一的 AI 会话 id：高 32 位为每进程随机盐、低 32 位为进程内自增序号，
/// 跨进程（含 in-proc DLL 各应用进程）不串 daemon 历史槽。
fn alloc_session_id() -> u64 {
    let seq = SESSION_ID_SEQ.fetch_add(1, Ordering::SeqCst);
    ((process_salt() as u64) << 32) | (seq & 0xffff_ffff)
}

/// 待重试的候选窗锚点（组合布局就绪后由定时器精确定位）。
struct CandidatePosRetry {
    context: ITfContext,
    attempts_left: u32,
}

/// 待触发的 Rime 候选请求（防抖中，pinyin 变更时重置计时）。
struct PendingCandidateReq {
    pinyin: String,
    ticks: u32,
}

/// 触发任务（截图 OCR / 录音 ASR）结果。
pub enum TriggerResult {
    /// 识别文本（上屏）。
    Text(String),
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
    pub chunks: Arc<Mutex<VecDeque<(u64, StreamEvent)>>>,
    /// 触发任务（截图 OCR / 录音 ASR）结果，定时器消费并上屏。
    pub trigger_results: Arc<Mutex<VecDeque<TriggerResult>>>,
    pub candidate_window: RefCell<Option<crate::candidate_window::CandidateWindow>>,
    /// 候选窗锚点重试（GetTextExt 返回 TS_E_NOLAYOUT 时，由定时器稍后重试精确定位）。
    candidate_pending_pos: RefCell<Option<CandidatePosRetry>>,
    /// 候选窗主题（随配置热更新）。
    candidate_theme: RefCell<verba_candidate::Theme>,
    /// Rime 方案（单引擎，如 luna_pinyin_simp / wubi86）。
    candidate_rime_schema: RefCell<String>,
    /// 配置文件上次 mtime（用于热更新检测）。
    theme_config_mtime: Cell<Option<std::time::SystemTime>>,
    /// 当前流注册槽：打包 token `(epoch << 32) | 请求id`（见 pack_stream_token）。
    /// 打包携带所属代际是为了让清理可「只认自己的票」——裸 id 会数值撞车
    /// （每连接自增恒为 2），store(0)/store(new) 都可能误伤他流；0 = 空槽。
    pub stream_request_id: Arc<AtomicU64>,
    /// 流代际（epoch）：每次发起新 LLM 流 +1；chunks 队列事件携带 epoch，
    /// 过滤只消费当前代际——请求 id 每连接从 1 自增（恒为 2），不能作跨流依据。
    pub stream_epoch: Arc<AtomicU64>,
    /// 在途候选融合请求 id（0 = 无）。
    pub candidate_request_id: Arc<AtomicU64>,
    /// 本输入上下文的 AI 多轮上下文会话 id（创建时分配，全局唯一）。daemon 按
    /// session_id 分组隔离历史：多应用文本域各自独立多轮，互不串上下文（B4b）。
    pub session_id: Cell<u64>,
    /// 待触发的候选融合请求（防抖中）。
    candidate_req_pending: RefCell<Option<PendingCandidateReq>>,
    stream_thread: RefCell<Option<JoinHandle<()>>>,
    /// 候选融合请求线程句柄。
    candidate_thread: RefCell<Option<JoinHandle<()>>>,
    /// 是否有候选/LLM 请求线程在途（防止 daemon 卡住时线程/连接堆积）。
    candidate_request_busy: Arc<AtomicBool>,
    /// daemon 是否已在本进程预拉起（激活时预热，避免首次输入冷启动延迟）。
    daemon_prewarmed: AtomicBool,
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
            trigger_results: Arc::new(Mutex::new(VecDeque::new())),
            candidate_window: RefCell::new(None),
            candidate_pending_pos: RefCell::new(None),
            candidate_theme: RefCell::new(verba_candidate::Theme::default()),
            candidate_rime_schema: RefCell::new("luna_pinyin_simp".into()),
            theme_config_mtime: Cell::new(None),
            stream_request_id: Arc::new(AtomicU64::new(0)),
            stream_epoch: Arc::new(AtomicU64::new(0)),
            candidate_request_id: Arc::new(AtomicU64::new(0)),
            session_id: Cell::new(alloc_session_id()),
            candidate_req_pending: RefCell::new(None),
            stream_thread: RefCell::new(None),
            candidate_thread: RefCell::new(None),
            candidate_request_busy: Arc::new(AtomicBool::new(false)),
            daemon_prewarmed: AtomicBool::new(false),
            control: RefCell::new(None),
        }
    }
}

#[implement(
    ITfTextInputProcessorEx,
    ITfTextInputProcessor,
    ITfDisplayAttributeProvider
)]
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

impl ITfDisplayAttributeProvider_Impl for TextService_Impl {
    fn EnumDisplayAttributeInfo(&self) -> windows::core::Result<IEnumTfDisplayAttributeInfo> {
        let p: ITfDisplayAttributeProvider =
            crate::display_attribute::DisplayAttributeProvider.into();
        unsafe { p.EnumDisplayAttributeInfo() }
    }

    fn GetDisplayAttributeInfo(
        &self,
        guid: *const windows::core::GUID,
    ) -> windows::core::Result<ITfDisplayAttributeInfo> {
        let p: ITfDisplayAttributeProvider =
            crate::display_attribute::DisplayAttributeProvider.into();
        unsafe { p.GetDisplayAttributeInfo(guid) }
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
    // 预拉起 daemon（daemon 启动即预热 Rime），避免首次输入等冷启动。
    prewarm_daemon(data);
    // 注册显示属性提供者（组合下划线；幂等，失败仅告警）。
    crate::display_attribute::register_provider();
    // 候选窗主题/引擎：从配置文件加载（失败保留默认，不影响激活）
    reload_candidate_config(data);
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
/// - `Idle`：认领 `/` 触发键、字母（进入拼音组合）与状态机标点（全角输出，
///   与 macOS 契约对齐——此前不认领时宿主直插半角，跨平台审查发现的不一致），
///   其余按键直通应用（不吞键、不进 IME）。
/// - `PendingSlash` / `Prompt` / `Streaming` / `ResultReady`：认领全部可打印字符
///   与控制键（Enter/Backspace/Esc），避免 `/` 或提示词被吞/丢字符。
/// - 修饰键/导航键/功能键（无字符）一律不认领，保持应用正常导航。
pub fn should_claim_key(state: MachineState, vk: u32, lparam: u32) -> bool {
    // 空闲态触发热键（Ctrl+Alt+O 截图 OCR / Ctrl+Alt+M 录音 ASR）一律认领。
    if state == MachineState::Idle && is_trigger_hotkey(vk) {
        return true;
    }
    let is_control = vk == VK_RETURN.0 as u32 || vk == VK_BACK.0 as u32 || vk == VK_ESCAPE.0 as u32;
    let is_page = vk == VK_PRIOR.0 as u32 || vk == VK_NEXT.0 as u32;
    let is_arrow = vk == VK_UP.0 as u32 || vk == VK_DOWN.0 as u32;
    // Ctrl/Alt 按下时不做字符认领：字母键在 Ctrl 下 ToUnicodeEx 返回控制字符
    // 本就不命中认领，但 OEM 标点（. , 等）仍返回普通字符——Idle/Pinyin 新
    // 认领的标点会把应用快捷键吞掉（如 VS Code 的 Ctrl+. 快速修复）。热键与
    // 控制/翻页键分支不受影响（沿用既有语义；实机验收项在 issue #44）。
    if ctrl_or_alt_held() {
        return false;
    }
    match state {
        MachineState::Idle => match get_char_for_vk(vk, lparam) {
            // 认领 `/`（AI 触发）、字母（进入拼音组合）、状态机标点（全角映射
            // 与成对引号——与 verba-core::is_fullwidth_mapped_punct 同源）
            Some(c) => idle_claim_char(c),
            None => false,
        },
        MachineState::Pinyin => {
            if is_control || is_page || is_arrow {
                // Enter/Backspace/Esc、PageUp/PageDown 与 Up/Down：
                // 拼音态由状态机处理（翻页/方向键选字）
                return true;
            }
            match get_char_for_vk(vk, lparam) {
                // 拼音态认领：字母（缓冲）、数字（选候选）、空格（选首选）、`/`（提交+AI）、
                // `-`/`=`（翻页）及其它状态机标点（候选+全角 flush，同 macOS）
                Some(c) => pinyin_claim_char(c),
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

/// Ctrl 或 Alt 当前按下（GetKeyState 高位；与既有热键判定同一套位约定）。
fn ctrl_or_alt_held() -> bool {
    unsafe {
        (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0
            || (GetKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0
    }
}

/// Idle 态认领的字符判定（VK 解耦，单测直测；调用点见 `should_claim_key`）。
fn idle_claim_char(c: char) -> bool {
    c == '/' || c.is_ascii_alphabetic() || is_fullwidth_mapped_punct(c)
}

/// 拼音态认领的字符判定（VK 解耦，单测直测）。
fn pinyin_claim_char(c: char) -> bool {
    c == '/'
        || c.is_ascii_alphabetic()
        || c.is_ascii_digit()
        || c == ' '
        || c == '-'
        || c == '='
        || is_fullwidth_mapped_punct(c)
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
    // 触发热键（Ctrl+Alt+O 截图 OCR / Ctrl+Alt+M 录音 ASR）：异步采集识别，结果经定时器上屏。
    if let Some(kind) = trigger_kind_for_vk(wparam) {
        log::info!("触发热键: {kind:?}");
        trigger_async(data, kind);
        return Ok(TRUE);
    }
    let vk = wparam;
    let is_control = vk == VK_RETURN.0 as u32 || vk == VK_BACK.0 as u32 || vk == VK_ESCAPE.0 as u32;
    let is_page = vk == VK_PRIOR.0 as u32 || vk == VK_NEXT.0 as u32;
    let is_arrow = vk == VK_UP.0 as u32 || vk == VK_DOWN.0 as u32;
    let ch = if is_control || is_page || is_arrow {
        None
    } else {
        get_char_for_vk(vk, lparam)
    };

    let mut machine = data.machine.borrow_mut();
    let state = machine.state();
    let action = if is_page {
        if state == MachineState::Pinyin {
            Some(if vk == VK_PRIOR.0 as u32 {
                machine.feed_page_up()
            } else {
                machine.feed_page_down()
            })
        } else {
            None
        }
    } else if is_arrow {
        if state == MachineState::Pinyin {
            Some(if vk == VK_UP.0 as u32 {
                machine.feed_arrow_up()
            } else {
                machine.feed_arrow_down()
            })
        } else {
            None
        }
    } else if let Some(c) = ch {
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
            cancel_candidate_request(data);
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
            page,
            selected,
            llm_request,
        } => {
            set_preedit(data, context, clientid, &preedit)?;
            update_candidate_window(data, context, &preedit, &candidates, page, selected);
            schedule_candidate_request(data, llm_request);
            Ok(())
        }
        Action::StartLlm { prompt, system: _ } => {
            // 多模态命令路由：
            // - `//朗读 <文本>` → TTS 合成并播放（不落盘文本）
            // - `//截图` → 全屏截图 OCR，识别文本上屏
            // - `//听写` → 录音 ASR，识别文本上屏
            // 均以「结束当前组合 + 重置状态机」收尾；采集/合成/播放异步完成。
            let cmd = prompt.trim();
            if cmd.starts_with("朗读") {
                let text = tts_text_of(cmd);
                log::info!("朗读命令: text={text}");
                if let Some(comp) = data.composition.borrow_mut().take() {
                    let _ = edit_session::end_composition(context, clientid, &comp, "");
                }
                *data.machine.borrow_mut() = CompositionMachine::new();
                start_tts_play(text);
                return Ok(());
            }
            // `//短语 <名称>`：快捷插入用户定义的文本模板。
            if let Some(name) = cmd.strip_prefix("短语") {
                let name = name.trim();
                if !name.is_empty() {
                    if let Ok(dirs) = verba_config::VerbaDirs::locate() {
                        if let Ok(Some(text)) = verba_config::phrases::get(&dirs, name) {
                            log::info!("插入快捷短语: {name}");
                            if let Some(comp) = data.composition.borrow_mut().take() {
                                let _ = edit_session::end_composition(context, clientid, &comp, "");
                            }
                            *data.machine.borrow_mut() = CompositionMachine::new();
                            let _ = edit_session::commit_text(context, clientid, &text);
                            crate::clipboard::set_text_quiet(&text);
                            return Ok(());
                        }
                    }
                }
            }
            // `//看图`：多模态 vision，直接捕捉眼睛区域（或全屏回退）发图给 LLM。
            // 与普通 `//` LLM 命令一致：不结束组合、不重置状态机，保持流式输出。
            if cmd == "看图" {
                log::info!("看图命令（vision）");
                start_llm(data, prompt, eye_rect_for(data, context), true);
                return Ok(());
            }
            let kind = if cmd == "截图" {
                Some(TriggerKind::OcrFullScreen)
            } else if cmd == "听写" {
                Some(TriggerKind::Asr)
            } else {
                None
            };
            if let Some(kind) = kind {
                log::info!("触发命令: {kind:?}");
                if let Some(comp) = data.composition.borrow_mut().take() {
                    let _ = edit_session::end_composition(context, clientid, &comp, "");
                }
                *data.machine.borrow_mut() = CompositionMachine::new();
                trigger_async(data, kind);
                return Ok(());
            }
            // 不要 set_preedit("")：把组合文本置空会触发应用终止组合
            // （OnCompositionTerminated → cancel_stream → 流式输出全丢，实测 Notepad--）。
            // 保持提示词组合，首个流式块到达时由 on_timer 的 UpdateResult 替换文本。
            let eye_rect = eye_rect_for(data, context);
            let (eye_enabled, eye_mode) =
                load_eye_runtime_cfg().unwrap_or((true, "ocr".to_owned()));
            let use_vision = eye_enabled && eye_mode == "vision";
            start_llm(data, prompt, eye_rect, use_vision);
            Ok(())
        }
        Action::ResultReady => Ok(()),
        Action::CommitResult { text } => {
            hide_candidate_window(data);
            cancel_candidate_request(data);
            if let Some(comp) = data.composition.borrow_mut().take() {
                edit_session::end_composition(context, clientid, &comp, &text)?;
            }
            // 提交时若流仍在途（用户中途 Enter）：取消流 + bump 代际，防僵尸
            // chunk/Final 以当前代际混入下个会话、防 daemon 把未见到的尾巴
            // 写进会话历史（原实现仅 store(0)，既不取消也不 bump，复审发现）。
            cancel_stream(data);
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
    preedit: &str,
    candidates: &[String],
    page: usize,
    selected: usize,
) {
    let mut borrow = data.candidate_window.borrow_mut();
    let Some(cw) = borrow.as_mut() else {
        return;
    };
    if candidates.is_empty() {
        // 与 macOS「空数据保持原内容」策略对齐（imk.rs refresh_candidate_window
        // 同款）：组合中候选在途时每次击键都携带空列表，若此处收起会造成候选窗
        // 逐键闪烁。真实收起由显式 hide 调用负责（提交/取消/会话终止路径）；
        // 查询终结为空的场景状态机会合成原文条目（非空），不会走到这里。
        return;
    }
    let theme = data.candidate_theme.borrow().clone();
    let mut ctrl = verba_candidate::CandidateWindowController::new(theme);
    ctrl.set_candidates(candidates.to_vec());
    ctrl.set_preedit(preedit);
    // 先 set_page（其内部会把选中重置回首项），最后 select_relative 应用
    // 状态机传来的选中——顺序反了会被 set_page 重置，方向键选字无视觉反馈。
    ctrl.set_page(page);
    ctrl.select_relative(selected.min(candidates.len().saturating_sub(1)));
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

/// 从配置文件加载候选相关配置（主题 + 引擎 + Rime 方案）；失败保留当前值。
fn reload_candidate_config(data: &Rc<TextServiceData>) {
    match load_candidate_config() {
        Ok((theme, schema)) => {
            *data.candidate_theme.borrow_mut() = theme;
            *data.candidate_rime_schema.borrow_mut() = schema.clone();
            log::info!("候选配置已加载（schema={schema}）");
        }
        Err(e) => log::warn!("候选配置加载失败: {e}"),
    }
    data.theme_config_mtime.set(config_mtime());
}

/// 定时器热更新：配置文件 mtime 变化时重载主题与引擎。
fn maybe_reload_candidate_config(data: &Rc<TextServiceData>) {
    if config_mtime() != data.theme_config_mtime.get() {
        reload_candidate_config(data);
    }
}

fn load_candidate_config() -> std::result::Result<(verba_candidate::Theme, String), String> {
    let dirs = verba_config::VerbaDirs::locate().map_err(|e| e.to_string())?;
    let mgr = verba_config::ConfigManager::new(dirs);
    let cfg = mgr.load().map_err(|e| e.to_string())?;
    Ok((cfg.theme.to_candidate_theme(), cfg.rime_schema))
}

fn config_mtime() -> Option<std::time::SystemTime> {
    let dirs = verba_config::VerbaDirs::locate().ok()?;
    let path = verba_config::ConfigManager::new(dirs).path().clone();
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
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

/// 与 daemon 默认一致的 AI 系统提示词（前端注入眼睛上下文时拼接）。
const AI_SYSTEM_BASE: &str =
    "你是一个输入法里的 AI 助手。回答应简洁、直接，以可上屏的文本输出，不要使用 Markdown。";
fn start_llm(
    data: &Rc<TextServiceData>,
    prompt: String,
    eye_rect: Option<(i32, i32, i32, i32)>,
    use_vision: bool,
) {
    let chunks = Arc::clone(&data.chunks);
    let request_id = Arc::clone(&data.stream_request_id);
    let stream_epoch = Arc::clone(&data.stream_epoch);
    let session_id = data.session_id.get();
    // 新流代际必须在发起线程（spawn 之前）领取：在 worker 内领取时，两个快速
    // 连续的 start_llm 的 epoch 顺序由 OS 线程调度决定，可能新旧颠倒——
    // on_timer 过滤会丢弃当前流、放行已作废流（复审 V6，P2-2 回归）。
    let epoch = stream_epoch.fetch_add(1, Ordering::SeqCst) + 1;
    let handle = std::thread::spawn(move || {
        let mut client = match ipc::ensure_daemon() {
            Ok(c) => c,
            Err(e) => {
                push_chunk(
                    &chunks,
                    epoch,
                    error_event(&format!("无法连接 daemon: {e}")),
                );
                return;
            }
        };
        // 眼睛：指令前捕捉光标上方屏幕。use_vision=true 时（`//看图` / eye_mode=vision）
        // 在工作线程内截图→PNG 直接交给 LLM；否则 OCR 转文字注入 system。
        let mut system: Option<String> = None;
        let mut image: Option<(String, Vec<u8>)> = None;
        if use_vision {
            if let Some(img) = eye_vision_image(eye_rect) {
                log::info!("眼睛区域 vision 捕捉成功, bytes={}", img.1.len());
                image = Some(img);
            } else {
                log::warn!("眼睛区域 vision 捕捉失败");
            }
        } else if let Some((rx, ry, rw, rh)) = eye_rect {
            match run_region_ocr_rect(rx, ry, rw, rh) {
                Ok(Some(text)) if !text.is_empty() => {
                    log::info!("眼睛区域已捕捉, ocr_len={}", text.chars().count());
                    system = Some(format!(
                        "{AI_SYSTEM_BASE}\n\n【眼睛内容：光标上方屏幕】\n{text}"
                    ));
                }
                Ok(_) => log::debug!("眼睛区域无识别文本"),
                Err(e) => log::warn!("眼睛区域 OCR 失败: {e}"),
            }
        }

        let image_ref = image.as_ref().map(|(m, d)| (m.as_str(), d.as_slice()));
        let id = match client.llm_start(
            &prompt,
            system.as_deref(),
            None,
            None,
            image_ref,
            session_id,
        ) {
            Ok(id) => id,
            Err(e) => {
                push_chunk(&chunks, epoch, error_event(&format!("LLM 启动失败: {e}")));
                return;
            }
        };
        // 注册取消目标：CAS 安装打包 token（守卫语义见 install_stream_token）。
        let my_token = pack_stream_token(epoch, id);
        let _ = install_stream_token(&request_id, my_token);
        // 已把图交给 daemon（llm_start 已返回）：线程还要跑完整个流式读
        // 循环，及时释放 PNG 缓冲——多屏截图可达数 MB，整个生成期驻留纯属浪费。
        drop(image);
        // 启动窗口内被取消/被新流取代（cancel_stream 在 id 落盘前已 bump
        // 代际）：立即在本连接补发取消——精确 (conn_id, id) 命中、无跨连接
        // fallback 歧义，防僵尸流继续烧 token 并把用户未见到的尾巴写进
        // daemon 会话历史。取消后直接退出：llm_cancel 的读循环可能已吞掉
        // 流的收尾事件，且本流事件代际已过期（on_timer 按 epoch 过滤），
        // 无需再消费。槽内若是自己的票则一并清空。
        if stream_epoch.load(Ordering::SeqCst) != epoch {
            let _ = client.llm_cancel(id);
            let _ = request_id.compare_exchange(my_token, 0, Ordering::SeqCst, Ordering::SeqCst);
            return;
        }
        loop {
            match client.next_event(id) {
                Ok(evt) => {
                    let done = matches!(
                        evt.kind,
                        Some(stream_event::Kind::Final(_)) | Some(stream_event::Kind::Error(_))
                    );
                    if let Ok(mut q) = chunks.lock() {
                        q.push_back((epoch, evt));
                    }
                    if done {
                        break;
                    }
                }
                Err(e) => {
                    push_chunk(&chunks, epoch, error_event(&format!("LLM 连接中断: {e}")));
                    break;
                }
            }
        }
        // 流已结束（正常完成/错误/连接断开）：只清自己的票。若改用裸
        // store(0)，迟到的旧流会把新流刚注册的取消凭据一并抹掉。
        let _ = request_id.compare_exchange(my_token, 0, Ordering::SeqCst, Ordering::SeqCst);
    });
    *data.stream_thread.borrow_mut() = Some(handle);
}

/// 推送合成错误事件：epoch 为唯一流判别依据（请求 id 每连接自增，不可跨流
/// 使用，on_timer 也只按 kind 消费），事件 id 恒 0。
fn push_chunk(
    chunks: &Arc<Mutex<VecDeque<(u64, StreamEvent)>>>,
    epoch: u64,
    kind: stream_event::Kind,
) {
    if let Ok(mut q) = chunks.lock() {
        q.push_back((
            epoch,
            StreamEvent {
                id: 0,
                kind: Some(kind),
            },
        ));
    }
}

/// 流注册槽打包格式：`(epoch << 32) | (daemon请求id & 0xFFFF_FFFF)`。
/// epoch 占高位使 token 自带代际：清理与取消都能核对「这张票是否仍属于
/// 发起时的流」，杜绝裸 id 数值撞车（每连接自增，恒为 2）导致的误清/误杀。
fn pack_stream_token(epoch: u64, daemon_id: u64) -> u64 {
    (epoch << 32) | (daemon_id & 0xFFFF_FFFF)
}

fn stream_token_epoch(token: u64) -> u64 {
    token >> 32
}

fn stream_token_id(token: u64) -> u64 {
    token & 0xFFFF_FFFF
}

/// 流注册槽安装：CAS 安装打包 token，**绝不覆盖同代或更新代的票**——
/// 慢启动的旧流迟到安装时新流可能已接管槽位，此时返回 false 放弃安装
/// （本流注定被代际检测回收）；若反向覆盖，会顶掉唯一能取消新流的凭据。
/// 返回 true = 本 token 已落盘；false = 未安装（槽内同代票保留先到者，
/// 或槽内已是更新代——当前调用方不区分这两者）。
fn install_stream_token(slot: &AtomicU64, token: u64) -> bool {
    loop {
        let cur = slot.load(Ordering::SeqCst);
        if cur != 0 && stream_token_epoch(cur) >= stream_token_epoch(token) {
            return false; // 槽内已是同/更新代，保留它
        }
        if slot
            .compare_exchange(cur, token, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return true;
        }
    }
}

fn cancel_stream(data: &Rc<TextServiceData>) {
    // 候选融合请求一并取消
    cancel_candidate_request(data);
    // 先取票后作废：token 自带代际，bump 前的 epoch 才是与票比对的有效
    // 基准（先 bump 再读会把自己刚作废的新票误判为陈旧）。
    let pre_bump_epoch = data.stream_epoch.load(Ordering::SeqCst);
    let token = data.stream_request_id.load(Ordering::SeqCst);
    // 作废旧流代际：无论是否已取得请求 id 都必须 bump——取消发生在
    // llm_start/OCR 在途（worker 尚未安装 token）的窗口内时，提前
    // 返回会漏掉 bump，旧流残留事件仍以「当前代际」通过 on_timer 的
    // epoch 过滤（复审发现）；bump 后由 start_llm worker 在安装 token 时
    // 检测代际并补发 daemon 取消（见 start_llm）。
    data.stream_epoch.fetch_add(1, Ordering::SeqCst);
    if token == 0 || stream_token_epoch(token) != pre_bump_epoch {
        // 槽内为空或陈旧代际残留（正常情况下 worker 的 CAS 清理已兜住，
        // 此处防御性清掉，且绝不发送跨代取消——裸 id 数值撞车会误杀别的流）。
        let _ =
            data.stream_request_id
                .compare_exchange(token, 0, Ordering::SeqCst, Ordering::SeqCst);
        return;
    }
    cancel_with_retry(&data.control, stream_token_id(token));
    // 取消已发出：只清自己的票（CAS 校验代际）。否则后续 Cancel/Commit 会
    // 拿陈旧 id 走跨连接 fallback，唯一命中时误杀另一条连接上同 id 的并发
    // 流（复审发现，id 每连接自增恒为 2，撞 id 是常态而非巧合）。
    let _ = data
        .stream_request_id
        .compare_exchange(token, 0, Ordering::SeqCst, Ordering::SeqCst);
}

/// 经控制连接取消指定请求；连接已死（服务端 idle 超时回收等）时重建并重试
/// 一次，保证本次取消生效（架构审查 P2-3 回归防护）。
fn cancel_with_retry(control: &RefCell<Option<verba_ipc::VerbaClient>>, id: u64) {
    let mut client = control.borrow_mut();
    if client.is_none() {
        *client = ipc::try_connect().ok();
    }
    if let Some(c) = client.as_mut() {
        if c.llm_cancel(id).is_err() {
            *client = ipc::try_connect().ok();
            if let Some(c2) = client.as_mut() {
                let _ = c2.llm_cancel(id);
            }
        }
    }
}

/// 取消在途候选融合请求（发起新请求 / 提交 / 取消组合时调用）。
fn cancel_candidate_request(data: &Rc<TextServiceData>) {
    data.candidate_req_pending.borrow_mut().take();
    let id = data.candidate_request_id.swap(0, Ordering::SeqCst);
    if id == 0 {
        return;
    }
    cancel_with_retry(&data.control, id);
}

/// 调度候选融合请求：不在途时**立即**发起（Rime 本地查询经 daemon IPC，
/// 毫秒级，无需防抖等待——原 320ms 防抖为远程 LLM 融合设计，单引擎下
/// 造成候选框滞后于输入、不跟手）；在途时保留 pending，由定时器
/// maybe_fire_candidate_request 在查询结束后补发（防线程堆积）。
fn schedule_candidate_request(data: &Rc<TextServiceData>, req: Option<LlmCandidateRequest>) {
    if let Some(r) = req {
        // 单引擎（Rime）：打字过程只请求本地 Rime 候选，不请求远程 LLM 候选融合
        // （LLM 仅用于回车触发的 AI 直输）。
        if !data.candidate_request_busy.load(Ordering::SeqCst) {
            let schema = data.candidate_rime_schema.borrow().clone();
            start_rime_candidates(data, r.pinyin, schema);
        } else {
            *data.candidate_req_pending.borrow_mut() = Some(PendingCandidateReq {
                pinyin: r.pinyin,
                ticks: 0,
            });
        }
    }
}

/// 候选融合防抖触发：输入停顿 DEBOUNCE_TICKS 个周期后才发起候选请求；
/// 已有请求在途时暂不触发（保留 pending，下个 tick 重试），防止线程/连接堆积。
fn maybe_fire_candidate_request(data: &Rc<TextServiceData>) {
    if data.candidate_request_busy.load(Ordering::SeqCst) {
        return;
    }
    let fire = {
        let mut pending = data.candidate_req_pending.borrow_mut();
        match pending.as_mut() {
            None => false,
            Some(req) => {
                req.ticks += 1;
                req.ticks >= CANDIDATE_REQ_DEBOUNCE_TICKS
            }
        }
    };
    if fire {
        let req = data
            .candidate_req_pending
            .borrow_mut()
            .take()
            .expect("存在");
        let schema = data.candidate_rime_schema.borrow().clone();
        start_rime_candidates(data, req.pinyin, schema);
    }
}

/// Rime 候选查询失败也必须回一个 done=true 空结果事件：状态机靠
/// Candidates(done=true) 释放 candidates_in_flight 并结算 deferred_intent
/// （与 macOS imk.rs 的错误即空结果 done 结算对齐，issue #44），静默
/// return 会把组合永远卡在「在途」——空格/选字全被暂缓（复审发现）。
fn push_rime_fail(chunks: &Arc<Mutex<VecDeque<(u64, StreamEvent)>>>, pinyin: &str, msg: &str) {
    log::warn!("{msg}");
    if let Ok(mut q) = chunks.lock() {
        q.push_back((
            0,
            StreamEvent {
                id: 0,
                kind: Some(stream_event::Kind::Candidates(verba_protos::Candidates {
                    pinyin: pinyin.to_owned(),
                    candidates: vec![],
                    done: true,
                })),
            },
        ));
    }
}

/// 发起 Rime 候选查询（单引擎；一次性返回候选，经 chunks 队列回流合并展示）。
/// Rime 查询为本地同步调用，未使用候选请求 id（cancel_candidate_request 仅清理 pending）。
fn start_rime_candidates(data: &Rc<TextServiceData>, pinyin: String, schema: String) {
    log::info!("Rime 候选请求: pinyin={pinyin} schema={schema}");
    cancel_candidate_request(data);
    let chunks = Arc::clone(&data.chunks);
    let busy = Arc::clone(&data.candidate_request_busy);
    // 结果回流后立即唤醒定时器窗口消费（不等下一个 80ms tick，跟手性）。
    // HWND 不实现 Send，跨线程传递原始指针值再还原。
    let timer_hwnd = data.timer_hwnd.get().map(|h| h.0 as usize);
    let handle = std::thread::spawn(move || {
        let _busy = BusyGuard::new(&busy);
        let mut client = match ipc::ensure_daemon() {
            Ok(c) => c,
            Err(e) => {
                push_rime_fail(&chunks, &pinyin, &format!("Rime 候选无法连接 daemon: {e}"));
                return;
            }
        };
        // 一次取足量候选（27 = daemon 上限）供前端本地分页（每页 9 条、
        // 最多 3 页）。此前每查询只取 9 条导致候选窗永远单页、翻页无效。
        let cands = match client.rime_candidates(&pinyin, &schema, 27) {
            Ok(c) => c,
            Err(e) => {
                push_rime_fail(&chunks, &pinyin, &format!("Rime 候选查询失败: {e}"));
                return;
            }
        };
        if let Ok(mut q) = chunks.lock() {
            // epoch=0：Rime 候选恒保留（不归属任何 LLM 流代际）
            q.push_back((
                0,
                StreamEvent {
                    id: 0,
                    kind: Some(stream_event::Kind::Candidates(verba_protos::Candidates {
                        pinyin,
                        candidates: cands,
                        done: true,
                    })),
                },
            ));
        }
        // 立即唤醒 on_timer 消费（PostMessage 同 WM_TIMER 投递，定时器
        // 窗口过程在 TSF 线程处理——线程安全）。
        if let Some(raw) = timer_hwnd {
            unsafe {
                let hwnd = HWND(raw as *mut core::ffi::c_void);
                let _ = PostMessageW(Some(hwnd), WM_TIMER, WPARAM(TIMER_ID as usize), LPARAM(0));
            }
        }
    });
    *data.candidate_thread.borrow_mut() = Some(handle);
}

/// 请求线程在途标志（RAII：任何退出路径都会复位，防止卡住时线程堆积）。
struct BusyGuard(Arc<AtomicBool>);
impl BusyGuard {
    fn new(flag: &Arc<AtomicBool>) -> Self {
        flag.store(true, Ordering::SeqCst);
        Self(Arc::clone(flag))
    }
}
impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// 激活时预拉起 daemon（daemon 启动即预热 Rime），
/// 避免用户首次输入时等待 daemon 冷启动（候选延迟 1-2 秒）。每进程只预热一次。
fn prewarm_daemon(data: &Rc<TextServiceData>) {
    if data.daemon_prewarmed.swap(true, Ordering::SeqCst) {
        return;
    }
    log::info!("预拉起 daemon…");
    std::thread::spawn(|| match ipc::ensure_daemon() {
        Ok(mut client) => {
            let _ = client.ping();
            log::info!("daemon 预热完成");
        }
        Err(e) => log::warn!("daemon 预热失败（首次请求时会重试）: {e}"),
    });
}

// ---- 多模态触发（截图 OCR / 录音 ASR / 朗读 TTS） ----

/// 触发热键判定（需 Ctrl+Alt 修饰）：Ctrl+Alt+O = 截图 OCR，Ctrl+Alt+M = 录音 ASR。
fn is_trigger_hotkey(vk: u32) -> bool {
    // Ctrl+Alt+O = 截图 OCR；Ctrl+Alt+S = 打开设置面板。
    // Ctrl+Alt+M（听写 ASR）随 ASR 冻结为实验性移除（#78 范围决策）。
    if vk != VK_O.0 as u32 && vk != VK_S.0 as u32 {
        return false;
    }
    unsafe {
        (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0
            && (GetKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0
    }
}

/// 触发热键 → 任务类型。
fn trigger_kind_for_vk(vk: u32) -> Option<TriggerKind> {
    if !is_trigger_hotkey(vk) {
        return None;
    }
    trigger_kind_for_hotkey_vk(vk)
}

/// 热键键集合内的按键 → 任务类型（纯函数，供单测钉住；
/// Ctrl+Alt 修饰在位性由 is_trigger_hotkey 判定）。
fn trigger_kind_for_hotkey_vk(vk: u32) -> Option<TriggerKind> {
    if vk == VK_O.0 as u32 {
        Some(TriggerKind::Ocr)
    } else if vk == VK_S.0 as u32 {
        Some(TriggerKind::OpenSettings)
    } else {
        None
    }
}

/// 触发任务类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TriggerKind {
    /// 选区截图 OCR（Ctrl+Alt+O，交互拖选）。
    Ocr,
    /// 全屏截图 OCR（`//截图`，无遴罩）。
    OcrFullScreen,
    /// 打开设置面板（Ctrl+Alt+S）。
    OpenSettings,
    /// 录音 ASR（`//听写` 命令保留；热键随 ASR 冻结移除）。
    Asr,
}

/// 启动设置面板（verba-settings.exe）：DLL 同目录 → 安装目录 → 稳定目录
/// （%LOCALAPPDATA%\Verba\ime）。GUI 应用，spawn 后分离（不阻塞 TSF 线程）。
fn open_settings() {
    use std::path::PathBuf;
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dll) = crate::reg::dll_path() {
        candidates.push(dll.with_file_name("verba-settings.exe"));
    }
    candidates.push(PathBuf::from(r"C:\Program Files\Verba\verba-settings.exe"));
    if let Ok(appdata) = std::env::var("LOCALAPPDATA") {
        candidates.push(PathBuf::from(appdata).join("Verba").join("ime").join("verba-settings.exe"));
    }
    for p in &candidates {
        if p.exists() {
            match std::process::Command::new(p).spawn() {
                Ok(_) => {
                    log::info!("打开设置面板: {}", p.display());
                    return;
                }
                Err(e) => log::warn!("启动设置面板失败 {}: {e}", p.display()),
            }
        }
    }
    log::warn!("未找到 verba-settings.exe（候选: {candidates:?}）");
}

/// 后台执行触发任务（采集 + daemon 识别），结果入队由定时器上屏。
fn trigger_async(data: &Rc<TextServiceData>, kind: TriggerKind) {
    let results = Arc::clone(&data.trigger_results);
    let _ = std::thread::spawn(move || {
        let outcome = match kind {
            TriggerKind::Ocr => match run_region_ocr() {
                Ok(Some(text)) => Ok(text),
                Ok(None) => return, // 用户取消选区
                Err(e) => {
                    log::warn!("选区 OCR 失败，回退全屏: {e}");
                    ocr_full_screen()
                }
            },
            TriggerKind::OcrFullScreen => ocr_full_screen(),
            TriggerKind::OpenSettings => {
                open_settings();
                return;
            }
            TriggerKind::Asr => {
                let wav = match record_seconds(ASR_RECORD_SECONDS) {
                    Ok(w) => w,
                    Err(e) => {
                        log::warn!("录音失败: {e}");
                        return;
                    }
                };
                match ipc::ensure_daemon() {
                    Ok(mut client) => client.asr_transcribe(&wav).map_err(|e| e.to_string()),
                    Err(e) => {
                        log::warn!("连接 daemon 失败: {e}");
                        return;
                    }
                }
            }
        };
        match outcome {
            Ok(text) => {
                if !text.trim().is_empty() {
                    results.lock().unwrap().push_back(TriggerResult::Text(text));
                }
            }
            Err(e) => log::warn!("{kind:?} 失败: {e}"),
        }
    });
}

/// 子进程调用 verba-trigger region-ocr：选区拖选 → OCR，stdout 为识别文本。
/// Ok(Some(text)) 识别成功；Ok(None) 用户取消；Err 失败。
fn run_region_ocr() -> std::result::Result<Option<String>, String> {
    let exe = match crate::reg::dll_path() {
        Ok(p) => {
            let candidate = p.with_file_name("verba-trigger.exe");
            if candidate.exists() {
                candidate
            } else {
                return Err(format!(
                    "未找到 verba-trigger.exe（{}）",
                    candidate.display()
                ));
            }
        }
        Err(e) => return Err(format!("定位 DLL 目录失败: {e}")),
    };
    let out = std::process::Command::new(&exe)
        .arg("region-ocr")
        // CREATE_NO_WINDOW：隐藏控制台窗口，遮罩 GUI 照常显示。
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("启动 verba-trigger 失败: {e}"))?;
    if !out.status.success() {
        return Err(format!("region-ocr 退出码: {:?}", out.status.code()));
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(text))
    }
}

/// 子进程调用 verba-trigger region-ocr --rect：脚本化区域 OCR（眼睛区域），stdout 为识别文本。
/// Ok(Some(text)) 识别成功；Ok(None) 无文本；Err 失败。
fn run_region_ocr_rect(
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> std::result::Result<Option<String>, String> {
    let exe = match crate::reg::dll_path() {
        Ok(p) => {
            let candidate = p.with_file_name("verba-trigger.exe");
            if candidate.exists() {
                candidate
            } else {
                return Err(format!(
                    "未找到 verba-trigger.exe（{}）",
                    candidate.display()
                ));
            }
        }
        Err(e) => return Err(format!("定位 DLL 目录失败: {e}")),
    };
    let rect_arg = format!("{x},{y},{w},{h}");
    let out = std::process::Command::new(&exe)
        .arg("region-ocr")
        .arg("--rect")
        .arg(rect_arg)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("启动 verba-trigger 失败: {e}"))?;
    if !out.status.success() {
        return Err(format!("region-ocr --rect 退出码: {:?}", out.status.code()));
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(text))
    }
}

/// 眼睛区域：`//` 指令时自动捕捉「光标上方」矩形（可配 eye.*），供 LLM 上下文。
/// BMP（32bpp top-down，capture 产物）→ PNG 字节，用于多模态 LLM vision。
fn bmp_to_png(bmp: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let img = image::load_from_memory(bmp).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// 捕捉眼睛区域（或全屏回退）为 PNG 图像，供多模态 LLM。
/// `eye_rect` 为 None 时回退到主屏全屏。
fn eye_vision_image(eye_rect: Option<(i32, i32, i32, i32)>) -> Option<(String, Vec<u8>)> {
    let shot = match eye_rect {
        Some((rx, ry, rw, rh)) => crate::capture::capture_region(rx, ry, rw, rh).ok()?,
        None => crate::capture::capture_primary_screen().ok()?,
    };
    let png = bmp_to_png(&shot.bmp).ok()?;
    Some(("image/png".to_owned(), png))
}

/// 读取当前眼睛运行配置：是否启用 + 喂给 LLM 的方式（ocr|vision）。
fn load_eye_runtime_cfg() -> Option<(bool, String)> {
    let dirs = verba_config::VerbaDirs::locate().ok()?;
    let cfg = verba_config::ConfigManager::new(dirs).load().ok()?;
    Some((cfg.eye_enabled, cfg.eye_mode.clone()))
}

fn eye_rect_for(data: &Rc<TextServiceData>, context: &ITfContext) -> Option<(i32, i32, i32, i32)> {
    let dirs = verba_config::VerbaDirs::locate().ok()?;
    let cfg = verba_config::ConfigManager::new(dirs).load().ok()?;
    if !cfg.eye_enabled {
        return None;
    }
    let (ax, top, bot) = caret_screen_pos(data, context)?;
    let w = cfg.eye_width.max(64);
    let h = cfg.eye_height.max(64);
    let off = cfg.eye_offset_y.max(0);
    // 与候选窗一致：以光标所在显示器「工作区」为边界，智能避让（默认上方）。
    let work = crate::candidate_window::monitor_work_area(ax, bot);
    let (px, py) = crate::candidate_window::fit_eye_rect((ax, top, bot), w, h, off, work);
    Some((px, py, w, h))
}

/// 全屏截图 → daemon OCR（选区失败时的回退路径）。
fn ocr_full_screen() -> std::result::Result<String, String> {
    let shot = capture_primary_screen().map_err(|e| e.to_string())?;
    let mut client = ipc::ensure_daemon().map_err(|e| format!("连接 daemon 失败: {e}"))?;
    client.ocr_recognize(&shot.bmp).map_err(|e| e.to_string())
}

/// 后台 TTS 合成（daemon）+ 播放（rodio），完成后仅记日志。
fn start_tts_play(text: String) {
    let _ = std::thread::spawn(move || {
        let mut client = match ipc::ensure_daemon() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("朗读：连接 daemon 失败: {e}");
                return;
            }
        };
        match client.tts_synthesize(&text, None) {
            Ok((format, bytes)) => match play_audio(&bytes) {
                Ok(()) => log::info!(
                    "朗读完成: text={text} format={format} bytes={}",
                    bytes.len()
                ),
                Err(e) => log::warn!("朗读播放失败: {e}"),
            },
            Err(e) => log::warn!("朗读合成失败: {e}"),
        }
    });
}

/// `//朗读 xxx` → 提取朗读文本（去前缀与分隔符）。
fn tts_text_of(prompt: &str) -> String {
    prompt
        .trim_start_matches("朗读")
        .trim_start_matches(|c| ":： \t".contains(c))
        .trim()
        .to_owned()
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

/// 两段式派发的中间步骤：第一遍（持状态机借锁）只收集动作，
/// 第二遍释放借锁后统一执行——apply_action 的部分分支会再次
/// borrow_mut(machine)（如 StartLlm 的重置），持锁直派必 panic。
#[derive(Debug)]
enum Step {
    Preedit(String),
    Candidates {
        preedit: String,
        candidates: Vec<String>,
        page: usize,
        selected: usize,
    },
    Act(Action),
    EndCompositionQuiet,
}

/// 把队列中捞出的流事件逐个喂给状态机，收集两段式派发步骤（纯函数，
/// 供单测钉住「done=true 空结果事件结算在途暂缓」「非刷新动作不丢弃」
/// 等布线语义，issue #44）。
fn collect_steps(machine: &mut CompositionMachine, events: Vec<StreamEvent>) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    // 合并 chunk 预编辑：每个胶子只调一次 set_preedit，降低 TSF 回调压力。
    let mut pending_preedit: Option<String> = None;
    for evt in events {
        match evt.kind {
            Some(stream_event::Kind::Chunk(ch)) => {
                if let Action::UpdateResult { preedit } = machine.on_llm_chunk(&ch.text) {
                    pending_preedit = Some(preedit);
                }
            }
            Some(stream_event::Kind::Final(_)) => {
                machine.on_llm_done();
                pending_preedit = Some(machine.result().to_owned());
            }
            Some(stream_event::Kind::Candidates(c)) => {
                // 先刷干容尽的 chunk 预编辑，再显示候选。
                if let Some(p) = pending_preedit.take() {
                    steps.push(Step::Preedit(p));
                }
                steps.push(
                    match machine.on_llm_candidates(&c.pinyin, &c.candidates, c.done) {
                        Action::UpdatePinyin {
                            preedit,
                            candidates,
                            page,
                            selected,
                            ..
                        } => Step::Candidates {
                            preedit,
                            candidates,
                            page,
                            selected,
                        },
                        // 在途暂缓后的知情回退（重复空格按原文提交）等
                        // 非刷新动作不能丢弃：与 macOS feed_candidates_event
                        // 的 CommitImmediate 处理对齐，走通用派发上屏
                        // （复审发现：此前被静默吞掉，空格无效）。
                        other => Step::Act(other),
                    },
                );
            }
            Some(stream_event::Kind::Error(e)) => {
                if matches!(machine.on_llm_error(&e.message), Action::LlmFailed { .. }) {
                    steps.push(Step::EndCompositionQuiet);
                }
            }
            None => {}
        }
    }
    if let Some(p) = pending_preedit {
        steps.push(Step::Preedit(p));
    }
    steps
}

impl TextServiceData {
    pub fn on_timer(&self) {
        let Some(rc) = self.self_rc.borrow().as_ref().cloned() else {
            return;
        };
        // 持续重试挂载键盘 sink（Activate 时可能失败）
        try_advise_keysink(&rc);
        // 候选窗主题/引擎：配置文件变更时热更新
        maybe_reload_candidate_config(&rc);
        // 候选窗：组合布局就绪后重试精确定位
        retry_candidate_pos(&rc);
        // 候选融合：输入停顿后发起 LLM 候选请求
        maybe_fire_candidate_request(&rc);
        // 触发任务（截图 OCR / 录音 ASR）结果上屏
        self.drain_trigger_results();

        // 流代际过滤（架构审查 P2-2）：只消费当前代际事件，跳过旧流残留
        // （提交后立即新流的窗口内，旧流在途 chunk 会混入队列——请求 id 每
        // 连接从 1 自增恒为 2，不能作跨流依据；epoch 单调递增可靠隔离）。
        // epoch=0（Rime 候选/无代际事件）恒保留。
        let current_epoch = self.stream_epoch.load(Ordering::SeqCst);
        let events: Vec<StreamEvent> = {
            let mut q = self.chunks.lock().unwrap();
            q.drain(..)
                .filter(|(epoch, _)| *epoch == 0 || *epoch == current_epoch)
                .map(|(_, evt)| evt)
                .collect()
        };
        if events.is_empty() {
            return;
        }
        let Some(context) = self.context.borrow().as_ref().cloned() else {
            return;
        };
        let clientid = self.clientid.get();

        let mut machine = self.machine.borrow_mut();
        // 两段式派发：第一遍（持状态机借锁）只收集动作，第二遍释放借锁后
        // 统一执行——收集逻辑见模块级纯函数 collect_steps（供单测钉住）。
        let steps = collect_steps(&mut machine, events);
        drop(machine);
        for step in steps {
            match step {
                Step::Preedit(p) => {
                    let _ = set_preedit(&rc, &context, clientid, &p);
                }
                Step::Candidates {
                    preedit,
                    candidates,
                    page,
                    selected,
                } => {
                    let _ = set_preedit(&rc, &context, clientid, &preedit);
                    update_candidate_window(&rc, &context, &preedit, &candidates, page, selected);
                }
                Step::Act(action) => {
                    let _ = apply_action(&rc, &context, action);
                }
                Step::EndCompositionQuiet => {
                    if let Some(comp) = self.composition.borrow_mut().take() {
                        let _ = edit_session::end_composition(&context, clientid, &comp, "");
                    }
                }
            }
        }
    }

    /// 消费触发任务（截图 OCR / 录音 ASR）结果并上屏。
    fn drain_trigger_results(&self) {
        let results: Vec<String> = {
            let mut q = self.trigger_results.lock().unwrap();
            q.drain(..)
                .map(|r| match r {
                    TriggerResult::Text(t) => t,
                })
                .collect()
        };
        if results.is_empty() {
            return;
        }
        let Some(context) = self.context.borrow().as_ref().cloned() else {
            return;
        };
        let clientid = self.clientid.get();
        for text in results {
            match edit_session::commit_text(&context, clientid, &text) {
                Ok(()) => log::info!("触发结果上屏: chars={}", text.chars().count()),
                Err(e) => log::warn!("触发结果上屏失败: {e}"),
            }
            crate::clipboard::set_text_quiet(&text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tts_text_of_strips_command_prefix() {
        assert_eq!(tts_text_of("朗读你好"), "你好");
        assert_eq!(tts_text_of("朗读 你好世界"), "你好世界");
        assert_eq!(tts_text_of("朗读：你好"), "你好");
        assert_eq!(tts_text_of("朗读: 你好"), "你好");
        assert_eq!(tts_text_of("朗读"), "");
    }

    #[test]
    fn trigger_kind_requires_modifier_but_maps_vk() {
        // 无修饰键（测试环境 GetKeyState 为 0）时不应认作热键。
        assert_eq!(trigger_kind_for_vk(VK_O.0 as u32), None);
        // Ctrl+Alt+M（听写）随 ASR 冻结移除（#78）；S = 打开设置。
        assert_eq!(trigger_kind_for_vk(VK_M.0 as u32), None);
        assert_eq!(trigger_kind_for_vk(VK_S.0 as u32), None);
    }

    #[test]
    fn settings_hotkey_maps_open_settings_kind() {
        // 热键映射纯函数：O → Ocr，S → OpenSettings，M → 已移除（None）。
        assert_eq!(
            trigger_kind_for_hotkey_vk(VK_O.0 as u32),
            Some(TriggerKind::Ocr)
        );
        assert_eq!(
            trigger_kind_for_hotkey_vk(VK_S.0 as u32),
            Some(TriggerKind::OpenSettings)
        );
        assert_eq!(trigger_kind_for_hotkey_vk(VK_M.0 as u32), None, "M 热键已移除");
        // 其他键不在热键集合
        assert_eq!(trigger_kind_for_hotkey_vk(VK_UP.0 as u32), None);
    }

    #[test]
    fn prompt_routing_classifies_commands() {
        assert!("朗读 你好".trim().starts_with("朗读"));
        assert_eq!("截图".trim(), "截图");
        assert_eq!("听写".trim(), "听写");
        assert_ne!("翻译：你好".trim(), "截图");
    }

    #[test]
    fn session_id_is_unique_and_carries_process_salt() {
        // B4b 回归：会话 id 低 32 位进程内自增（互不相同），高 32 位为每进程
        // 随机盐——in-proc DLL 加载进各应用进程，跨进程不串 daemon 历史槽。
        let a = alloc_session_id();
        let b = alloc_session_id();
        assert_ne!(a, b);
        assert_eq!(a >> 32, process_salt() as u64);
        assert_eq!(b >> 32, process_salt() as u64);
        assert_ne!(a & 0xffff_ffff, b & 0xffff_ffff);
    }

    /// 键位认领的字符级判定：状态机标点在 Idle/Pinyin 两态都认领（全角输出，
    /// 与 macOS 契约对齐）；未映射字符不认领。
    #[test]
    fn claim_chars_cover_machine_punct() {
        for c in [
            ',', '.', ';', ':', '?', '!', '(', ')', '[', ']', '"', '\'', '-', '~',
        ] {
            assert!(idle_claim_char(c), "Idle 应认领 {c:?}");
            assert!(pinyin_claim_char(c), "Pinyin 应认领 {c:?}");
        }
        assert!(idle_claim_char('/'));
        // '=' 不在映射表内（仅拼音态翻页键）：两态差异钉住
        assert!(pinyin_claim_char('='));
        assert!(!idle_claim_char('='), "'=' 非映射标点，Idle 不认领");
        assert!(!idle_claim_char('`'), "未映射符号不认领");
        assert!(!idle_claim_char(' '), "空格 Idle 不认领（保持原语义）");
        assert!(pinyin_claim_char('2') && pinyin_claim_char(' '));
        assert!(pinyin_claim_char('0'), "数字行整体走拼音态通用通道");
    }

    /// Rime 失败通道与 macOS 对齐（issue #44）：查询失败也回 done=true
    /// 空结果 Candidates 事件，状态机据此释放在途标记并结算暂缓意图。
    #[test]
    fn rime_fail_event_is_done_empty_candidates() {
        let chunks = Arc::new(Mutex::new(VecDeque::new()));
        push_rime_fail(&chunks, "ni", "模拟失败");
        let q = chunks.lock().unwrap();
        let (epoch, evt) = q.front().unwrap();
        assert_eq!(*epoch, 0, "Rime 候选事件不归属任何流代际");
        match evt.kind.as_ref().unwrap() {
            stream_event::Kind::Candidates(c) => {
                assert_eq!(c.pinyin, "ni");
                assert!(c.candidates.is_empty());
                assert!(c.done, "done=true 才能释放 in-flight 并结算暂缓");
            }
            other => panic!("应回 Candidates 事件，实际 {other:?}"),
        }
    }

    /// 布线语义钉住（issue #44）：done=true 空结果候选事件经 collect_steps
    /// 走到状态机，暂缓空格被结算为「上板原文条目」的刷新（而非被静默吞掉
    /// 或盲提原文）——与 macOS feed_candidates_event 的错误结算对齐。
    #[test]
    fn collect_steps_settles_inflight_deferred_space() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        assert_eq!(m.feed_char(' '), Action::None, "在途空格应暂缓");
        let steps = collect_steps(
            &mut m,
            vec![StreamEvent {
                id: 0,
                kind: Some(stream_event::Kind::Candidates(verba_protos::Candidates {
                    pinyin: "ni".into(),
                    candidates: vec![],
                    done: true,
                })),
            }],
        );
        assert!(
            matches!(&steps[..], [Step::Candidates { preedit, candidates, .. }]
                if preedit == "ni" && candidates == &vec!["ni".to_string()]),
            "空结果结算应上板合成原文条目（与 macOS 一致），实际 {steps:?}"
        );
        // 用户此刻看得见候选窗：再按空格才是知情回退
        assert_eq!(m.feed_char(' '), Action::CommitImmediate("ni".into()));
    }

    /// 布线语义钉住（issue #44，复审建议 S1）：暂缓空格后真实候选结算 →
    /// 首候选提交动作走 Step::Act 派发、不得被静默丢弃（此前被吞掉、
    /// 空格无效的复审发现——非刷新动作与 macOS 对齐走通用派发上屏）。
    #[test]
    fn collect_steps_real_candidates_emit_commit_act() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        assert_eq!(m.feed_char(' '), Action::None, "在途空格应暂缓");
        let steps = collect_steps(
            &mut m,
            vec![StreamEvent {
                id: 0,
                kind: Some(stream_event::Kind::Candidates(verba_protos::Candidates {
                    pinyin: "ni".into(),
                    candidates: vec!["你".into()],
                    done: true,
                })),
            }],
        );
        assert!(
            matches!(&steps[..], [Step::Act(Action::CommitImmediate(t))] if t == "你"),
            "真实候选结算应产出首候选提交步骤（不被丢弃），实际 {steps:?}"
        );
    }

    /// 流 token 打包/安装属性（issue #44 真机项的可自动化部分）：
    /// 代际嵌入高位、id 嵌入低位；「同代保留先到者、旧代绝不覆盖新代、
    /// 空槽正常安装」在交错序列下成立——真机验收仅剩 TSF 路由副作用。
    #[test]
    fn install_stream_token_never_overwrites_newer_epoch() {
        let tok = pack_stream_token(7, 0x1234_5678);
        assert_eq!(stream_token_epoch(tok), 7);
        assert_eq!(stream_token_id(tok), 0x1234_5678);
        assert_eq!(stream_token_epoch(0), 0, "0 = 空槽语义");

        let slot = AtomicU64::new(0);
        assert!(install_stream_token(&slot, pack_stream_token(1, 2)));
        // 同代旧 id：保留先到者（不覆盖）
        assert!(!install_stream_token(&slot, pack_stream_token(1, 99)));
        assert_eq!(slot.load(Ordering::SeqCst), pack_stream_token(1, 2));
        // 新代覆盖
        assert!(install_stream_token(&slot, pack_stream_token(2, 3)));
        assert_eq!(stream_token_epoch(slot.load(Ordering::SeqCst)), 2);
        // 旧代迟到安装：必须放弃（槽位代际只前进不后退）
        assert!(!install_stream_token(&slot, pack_stream_token(1, 5)));
        assert_eq!(slot.load(Ordering::SeqCst), pack_stream_token(2, 3));
        // 清空后正常安装
        slot.store(0, Ordering::SeqCst);
        assert!(install_stream_token(&slot, pack_stream_token(3, 7)));
        assert_eq!(stream_token_id(slot.load(Ordering::SeqCst)), 7);
    }
}
