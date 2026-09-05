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
    is_fullwidth_mapped_punct, result_hint, Action, CompositionMachine, LlmCandidateRequest,
    MachineState, PreviewKey, ResultPhase, REWRITE_SYSTEM_PROMPT,
};
use verba_core::{parse_ai_command, AiCommand};
use verba_protos::{stream_event, StreamEvent};
use windows::core::{implement, w, Interface, Ref, Result, PCWSTR};
use windows::Win32::Foundation::{FALSE, HINSTANCE, HWND, LPARAM, LRESULT, TRUE, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, GetKeyboardState, ToUnicodeEx, VK_BACK, VK_CONTROL, VK_DOWN,
    VK_ESCAPE, VK_MENU, VK_NEXT, VK_O, VK_PRIOR, VK_RETURN, VK_S, VK_SHIFT, VK_UP,
};

use verba_trigger::capture::capture_primary_screen;
use verba_trigger::play::play_audio;
use verba_trigger::record::record_seconds;
use windows::Win32::UI::TextServices::{
    IEnumTfDisplayAttributeInfo, ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl,
    ITfContext, ITfContextView, ITfDisplayAttributeInfo, ITfDisplayAttributeProvider,
    ITfDisplayAttributeProvider_Impl, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr,
    ITfTextInputProcessor, ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl,
    ITfTextInputProcessor_Impl, ITfThreadMgr, GUID_COMPARTMENT_KEYBOARD_INPUTMODE,
    TF_CONVERSIONMODE_ALPHANUMERIC, TF_CONVERSIONMODE_NATIVE,
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
    /// 中英模式（Shift 孤立按切换）：true = 中文（拼音），false = 英文直输。
    ime_chinese: Cell<bool>,
    /// Shift 按下中（孤立 Shift 抬起时切换中英）。
    shift_down: Cell<bool>,
    /// Shift 按下期间是否与其他键组合（组合则不切换）。
    shift_combined: Cell<bool>,
    /// 中英切换状态提示的到期时刻（候选窗闪「中/英」后自动隐藏）。
    ime_status_until: Cell<Option<std::time::Instant>>,
    /// OCR/触发结果浮窗锚点：触发命令结束组合**前**记下的组合光标位置。
    /// 预览几秒后异步显示时组合已不在（caret_screen_pos 依赖组合引用），
    /// 无此槽则只剩「视图左上角」兜底——离用户视线（拖选区/光标）可能
    /// 相距甚远（真机 2026-09-05 `///` 后「啥也没看到」的定位嫌疑）。
    ocr_anchor: Cell<Option<(i32, i32, i32)>>,
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
            ime_chinese: Cell::new(true),
            shift_down: Cell::new(false),
            shift_combined: Cell::new(false),
            ime_status_until: Cell::new(None),
            ocr_anchor: Cell::new(None),
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

    #[allow(clippy::not_unsafe_ptr_arg_deref)]
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
    // 初始中英状态：激活一律中文起步（拼音输入法的自然默认）。
    // 不读系统 compartment 的遗留值：Shift 切英文时写入的
    // ALPHANUMERIC(0) 会全局留存，把之后的每次激活都误判成英文起步
    // （真机 2026-09-04：升级后新开窗口全程英文、`//`/`///` 因按键
    // 不被认领而失效，按一次 Shift 才恢复；DLL 日志连续 claim=false
    // 为证）。英文模式只在会话内经 Shift 显式进入，不跨激活继承。
    // 同时把 compartment 写回 NATIVE，让系统语言栏指示器与实际
    // 模式一致（遗留的英文值不清会让指示器显示「英」）。
    data.ime_chinese.set(true);
    unsafe {
        if let Ok(mgr) = ptim.GetGlobalCompartment() {
            if let Ok(comp) = mgr.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_INPUTMODE) {
                let _ = comp.SetValue(
                    0,
                    &crate::display_attribute::variant_i4(TF_CONVERSIONMODE_NATIVE as i32),
                );
            }
        }
    }
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
/// - `PendingSlash` / `Prompt` / `Streaming` / `ResultReady` / `Failed`：认领
///   全部可打印字符与控制键（Enter/Backspace/Esc），避免 `/` 或提示词被
///   吞/丢字符。
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
        | MachineState::ResultReady
        | MachineState::Failed => {
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
        // Shift 始终认领（仅用于接收 OnKeyUp 检测孤立按；OnKeyDown 返回 FALSE
        // 不吞键，Shift 照常交宿主）——TSF 对不认领的键不回调 OnKeyUp，切换
        // 检测会永远不触发（真机复现）。
        // 英文模式（Shift 切换后）：除 Shift/热键外全部交宿主直插（字母/标点
        // 不过 IME）；中文模式走 should_claim_key 正常路由。
        let claim = if vk == VK_SHIFT.0 as u32 {
            true
        } else if !self.data.ime_chinese.get() && !is_trigger_hotkey(vk) {
            false
        } else {
            should_claim_key(state, vk, lparam.0 as u32)
        };
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
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Result<windows::core::BOOL> {
        // Shift Up 也须认领（与 Down 对称——TSF 对未认领的键不回调 OnKeyUp）。
        if wparam.0 as u32 == VK_SHIFT.0 as u32 {
            return Ok(TRUE);
        }
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
        let vk = wparam.0 as u32;
        if vk == VK_SHIFT.0 as u32 {
            // 首次按下（lparam bit30=0）记录：孤立 Shift 抬起时切换中英。
            if (lparam.0 as u32 >> 30) & 1 == 0 {
                self.data.shift_down.set(true);
                self.data.shift_combined.set(false);
            }
            return Ok(FALSE); // Shift 不吞（交宿主）
        }
        if self.data.shift_down.get() {
            self.data.shift_combined.set(true);
        }
        if let Ok(ctx) = pic.ok() {
            *self.data.context.borrow_mut() = Some(ctx.clone());
        }
        handle_key_down(&self.data, wparam.0 as u32, lparam.0 as u32)
    }
    fn OnKeyUp(
        &self,
        _pic: Ref<ITfContext>,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Result<windows::core::BOOL> {
        unsafe {
            log::info!(
                "OnKeyUp vk=0x{:02X} scan=0x{:02X} tid={}",
                wparam.0 as u32,
                (lparam.0 as u32 >> 16) & 0xff,
                GetCurrentThreadId()
            );
        }
        if wparam.0 as u32 == VK_SHIFT.0 as u32 {
            // 孤立 Shift（按下期间无其他键组合）→ 切换中英。
            if self.data.shift_down.get() && !self.data.shift_combined.get() {
                toggle_ime(&self.data);
            }
            self.data.shift_down.set(false);
        }
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

/// 构建覆盖层候选窗并按给定锚点显示。OCR 预览 / 改写对照预览 / 中英状态卡
/// 共用这套「主题克隆 → 控制器 → 显示 → 锚点」管线——三处各抄一份时兜底
/// 定位与借锁顺序已经开始漂移，收口到这里后改一处即三处生效。
fn show_overlay_window(
    data: &Rc<TextServiceData>,
    anchor: (i32, i32, i32),
    populate: impl FnOnce(&mut verba_candidate::CandidateWindowController),
) {
    let theme = data.candidate_theme.borrow().clone();
    let mut ctrl = verba_candidate::CandidateWindowController::new(theme);
    populate(&mut ctrl);
    ctrl.show();
    if let Some(cw) = data.candidate_window.borrow_mut().as_mut() {
        cw.update(&ctrl, anchor);
    }
}

/// 视图粗定位兜底锚点（无组合 / 布局未就绪时的最佳 effort）。
fn view_fallback_anchor(data: &Rc<TextServiceData>) -> (i32, i32, i32) {
    data.context
        .borrow()
        .as_ref()
        .and_then(view_screen_pos)
        .unwrap_or((0, 0, 0))
}

/// 发送即反馈：LLM 流（自由生成 / 看图 / 改写管道）发起时把组合文本立刻
/// 换成机器的短状态串（Streaming 态 =「✨ 生成中…」）。发送 → 首块的
/// 首 token 延迟内此前完全无反馈，用户以为没发出而习惯性再敲 Enter——
/// 空提交把提示词一并抹掉（真机 2026-09-04「AI 没回复」）。非空短串不
/// 触发 Notepad-- 的「空组合文本 → 应用终止组合」陷阱（该陷阱专指空串）；
/// 后续 chunk 仍由 on_timer 的 UpdateResult 走同一条 set_preedit 路径。
fn set_preedit_streaming_status(data: &Rc<TextServiceData>, context: &ITfContext, clientid: u32) {
    let status = data.machine.borrow().preedit();
    let _ = set_preedit(data, context, clientid, &status);
}

/// 触发命令（`///` 选区截图 / `//截图` 全屏 / `//听写`）结束组合**前**记下
/// 光标锚点：预览几秒后异步回来时组合已结束，caret_screen_pos 依赖组合
/// 引用将不可用。只按视图左上角兜底会把预览甩到应用文本区左上角，离
/// 用户视线（刚拖选的区域 / 光标）可能相距数屏——真机 2026-09-05 `///`
/// 全链路正常、窗口可见 9s+，用户却感知「啥也没看到」的定位主嫌疑。
fn stash_ocr_anchor(data: &Rc<TextServiceData>, context: &ITfContext) {
    let anchor = caret_screen_pos(data, context).or_else(|| view_screen_pos(context));
    if let Some(a) = anchor {
        data.ocr_anchor.set(Some(a));
    }
}

/// OCR 预览浮窗：识别文本进多行结果块（与 AI 结果浮层同一条渲染路径，
/// 长文本换行可读、宽度撑满浮层）+ 标题行自解释 + 状态行操作提示。
/// 此前识别文本塞单条候选行：360px 宽内单行截断 + 「1.」前缀，36 字
/// 只剩半行——真机上不像任何「识别结果」。上屏文本取 machine 的
/// ocr_preview 槽（完整原文），与本处显示串（带标题前缀）无关。
/// 锚点优先触发时记下的光标位置（见 stash_ocr_anchor），兜底视图粗定位。
fn show_ocr_preview(data: &Rc<TextServiceData>, text: &str) {
    // take() 一次性消费：热键路径（Ctrl+Alt+O，无组合不 stash）不得复用
    // 上一次触发留下的陈旧锚点——「卡片甩到远处」不能从这扇门回来
    // （独立审查 P2）。无 stash 的触发自然落回视图兜底。
    let anchor = data
        .ocr_anchor
        .take()
        .unwrap_or_else(|| view_fallback_anchor(data));
    show_overlay_window(data, anchor, |ctrl| {
        // 标题用 CJK 括号标记而非 emoji：cosmic-text/swash 渲染路径只验证过
        // CJK+ASCII 字形（✨ 等都在 preedit 文本里由宿主应用渲染），彩色
        // emoji 字形经 swash 栅格化的表现未验证，不拿可发现性冒险。
        ctrl.set_result_block(&format!("【OCR 识别结果】\n{text}"));
        ctrl.set_status(Some("Enter/空格/1 上屏 · Esc 取消".to_owned()));
    });
}

/// 改写对照预览候选窗：1=改写结果（选中态）2=原文，状态行提示操作。
/// 此刻组合仍活跃（preedit=改写结果），优先光标锚点——只按视图粗定位会把
/// 窗口甩到文本区左上角，离正在改写的那一行可能相距数屏。
fn show_rewrite_preview(
    data: &Rc<TextServiceData>,
    context: &ITfContext,
    rewritten: &str,
    source: &str,
) {
    let anchor = caret_screen_pos(data, context)
        .or_else(|| view_screen_pos(context))
        .unwrap_or((0, 0, 0));
    show_overlay_window(data, anchor, |ctrl| {
        ctrl.set_candidates(vec![rewritten.to_owned(), source.to_owned()]);
        ctrl.set_status(Some("1/Enter 改写 · 2 原文 · Esc 取消".to_owned()));
    });
}

/// AI 结果浮层候选窗：多行结果全文（显示层按 MAX_RESULT_CHARS 截断）+
/// 阶段状态行（提示文案取 core 的 result_hint，两端一致）。流式/就绪/
/// 失败三态共用；组合此时活跃（preedit 为短状态串），优先组合光标锚点。
/// 行数实测回填不在前端做——由 CandidateWindow::update 在**缩放后**坐标
/// 系实测（见 candidate_window.rs 注），建窗/渲染/换行同一控制器。
fn show_result_overlay(
    data: &Rc<TextServiceData>,
    context: &ITfContext,
    body: &str,
    phase: ResultPhase,
) {
    let anchor = caret_screen_pos(data, context)
        .or_else(|| view_screen_pos(context))
        .unwrap_or((0, 0, 0));
    show_overlay_window(data, anchor, |ctrl| {
        ctrl.set_result_block(body);
        ctrl.set_status(Some(result_hint(phase).to_owned()));
    });
}

/// 查用户定义短语（`//短语 名称`）；无配置/无此名称返回 None
/// （调用方按普通生成兜底）。
fn lookup_phrase(name: &str) -> Option<String> {
    let dirs = verba_config::VerbaDirs::locate().ok()?;
    verba_config::phrases::get(&dirs, name).ok().flatten()
}

/// Shift 孤立按切换中英：翻转模式 + 同步 GUID_COMPARTMENT_KEYBOARD_INPUTMODE
/// （系统语言栏/输入法指示器显示中/英状态）+ 组合中则取消（干净进入英文）。
fn toggle_ime(data: &Rc<TextServiceData>) {
    let chinese = !data.ime_chinese.get();
    data.ime_chinese.set(chinese);
    if let Some(tm) = data.threadmgr.borrow().as_ref().cloned() {
        unsafe {
            if let Ok(mgr) = tm.GetGlobalCompartment() {
                if let Ok(comp) = mgr.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_INPUTMODE) {
                    let mode = if chinese {
                        TF_CONVERSIONMODE_NATIVE as i32
                    } else {
                        TF_CONVERSIONMODE_ALPHANUMERIC as i32
                    };
                    let _ = comp.SetValue(0, &crate::display_attribute::variant_i4(mode));
                }
            }
        }
    }
    // 组合中切换：取消当前组合（干净进入英文，后续字母直插）。
    if data.machine.borrow().state() != MachineState::Idle {
        if let Some(context) = data.context.borrow().as_ref().cloned() {
            if let Some(comp) = data.composition.borrow_mut().take() {
                let _ = edit_session::end_composition(&context, data.clientid.get(), &comp, "");
            }
            *data.machine.borrow_mut() = CompositionMachine::new();
        }
    }
    // 视觉反馈：候选窗位置闪「中/英」状态卡（2 秒后由 on_timer 隐藏）。
    show_overlay_window(data, view_fallback_anchor(data), |ctrl| {
        ctrl.set_status(Some(
            if chinese {
                "中文模式"
            } else {
                "英文模式"
            }
            .to_owned(),
        ));
    });
    data.ime_status_until.set(Some(
        std::time::Instant::now() + std::time::Duration::from_secs(2),
    ));
    log::info!("中英模式切换: {}", if chinese { "中文" } else { "英文" });
}

/// 预览态按键分类（OCR 预览与改写对照预览共用一份清单，防止两条路由
/// 各自漂移）。Return/Esc 按虚拟键；数字/空格按成字符。
fn classify_preview_key(vk: u32, ch: Option<char>) -> Option<PreviewKey> {
    if vk == VK_RETURN.0 as u32 {
        Some(PreviewKey::Enter)
    } else if ch == Some(' ') {
        Some(PreviewKey::Space)
    } else if ch == Some('1') {
        Some(PreviewKey::Digit1)
    } else if ch == Some('2') {
        Some(PreviewKey::Digit2)
    } else if vk == VK_ESCAPE.0 as u32 {
        Some(PreviewKey::Escape)
    } else {
        None
    }
}

/// 数字列的 VK 回退：AZERTY 等布局数字键不经 Shift 不产生数字字符（法语
/// 布局原样产出 é 等），只看成字符会让「1/2 选定」在这些布局永远触发不了。
/// 仅改写对照预览使用——OCR 预览发生在 Idle 态，数字键本就不会被认领送达。
fn preview_digit_by_vk(vk: u32) -> Option<PreviewKey> {
    match vk {
        0x31 => Some(PreviewKey::Digit1), // VK_1
        0x32 => Some(PreviewKey::Digit2), // VK_2
        _ => None,
    }
}

/// 处理一次 key down。pub 供 tsf_smoke 集成测试直驱按键路由（曾在
/// 重构中被收回私有，测试目标随之编译失败）。
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
    // 改写对照预览态按键拦截（优先于 OCR 预览——两态互斥）：1/Enter/空格=
    // 改写上屏，2=原文上屏，Esc 取消；其他键不动预览，落回下方正常路由
    // （此刻仍 ResultReady：可打印键已被认领送达，feed_char 返回 None 后
    // 照常吞掉，不会漏进宿主文档——预览期间保护结果）。
    if machine.rewrite_previewing() {
        let key = classify_preview_key(vk, ch).or(preview_digit_by_vk(vk));
        if let Some(k) = key {
            if let Some(action) = machine.feed_rewrite_preview(k) {
                log::info!("改写预览: {action:?}");
                drop(machine);
                hide_candidate_window(data);
                let Some(context) = data.context.borrow().as_ref().cloned() else {
                    return Ok(FALSE);
                };
                let _ = apply_action(data, &context, action);
            }
            return Ok(TRUE);
        }
    }
    // OCR 预览态按键拦截：Enter/空格/1 上屏，Esc 取消，其他键退出预览
    // 后照常走下方路由（不打断打字流）。
    if machine.ocr_previewing() {
        match classify_preview_key(vk, ch) {
            // 2 在 OCR 预览无语义（仅识别文本一项）：按未命中处理——退出
            // 预览，该键落回下方正常路由。
            Some(PreviewKey::Digit2) | None => {
                // 未命中预览键：退出预览，隐藏候选窗，落回下方正常路由。
                machine.end_ocr_preview();
                hide_candidate_window(data);
            }
            Some(k) => {
                let action = machine.feed_ocr_preview(k).unwrap_or(Action::None);
                log::info!("OCR 预览: {action:?}");
                drop(machine);
                hide_candidate_window(data);
                if matches!(action, Action::CommitImmediate(_) | Action::Cancel) {
                    let Some(context) = data.context.borrow().as_ref().cloned() else {
                        return Ok(FALSE);
                    };
                    let _ = apply_action(data, &context, action);
                }
                return Ok(TRUE);
            }
        }
    }
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
        Action::EnterPrompt { preedit } | Action::UpdatePrompt { preedit } => {
            // 离开结果浮层回提示词编辑（e/退格）或 // 进入提示词模式时收起
            // 结果浮层：不收起则旧结果全文与「r 重试 e 改提示词」状态行继续
            // 悬浮，而组合已变提示词、按键语义已变（独立复审 P1；镜像 macOS
            // 同场景的收起——两端同语义是 #89 的目标）。浮层态不产本动作，
            // ai_previewing 守卫恒过，仅作显式声明。
            if !data.machine.borrow().ai_previewing() {
                hide_candidate_window(data);
            }
            set_preedit(data, context, clientid, &preedit)
        }
        Action::UpdateResult { preedit, body } => {
            // 流式增量：preedit 只放**短状态串**——组合不再随流变长（此前
            // 整串 setMarkedText 把长结果挤进窄 preedit，且每 chunk 一次宿主
            // 往返）；全文进结果浮层（verba-candidate 多行结果块）。
            set_preedit(data, context, clientid, &preedit)?;
            let phase = data
                .machine
                .borrow()
                .result_phase()
                .unwrap_or(ResultPhase::Streaming);
            show_result_overlay(data, context, &body, phase);
            Ok(())
        }
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
        Action::StartRewrite { content } => {
            // `//<内容>` + Tab：改写管道。系统提示词固定为「忠实改写」——
            // 纠错补全 + 结构化成文，不自由发挥；流式结果沿用
            // Streaming/ResultReady 通道（Enter 上屏 / 继续打字编辑）。
            // 提示词与 macOS 前端共用 verba-core 的常量，杜绝两端措辞漂移。
            log::info!("改写管道: content_len={}", content.chars().count());
            let system = Some(REWRITE_SYSTEM_PROMPT.to_owned());
            // 发送即反馈：组合文本立刻换成短状态串——发送 → 首块的 1-3s
            // 此前完全无反馈，用户以为没发出而习惯性再敲 Enter，空提交把
            // 被改写的原文一并抹掉。非空短串不触发 Notepad-- 的「空组合
            // 文本 → 应用终止组合」陷阱（该陷阱专指空串）。经 helper 两
            // 语句写法：行内 &machine.borrow().preedit() 会让 Ref 存活
            // 跨越整个 set_preedit 调用，若 update_composition 同步触发
            // OnCompositionTerminated（内部 machine.borrow_mut()）即
            // BorrowMutError panic（独立审查 P3）。
            set_preedit_streaming_status(data, context, clientid);
            start_llm_with_system(data, &content, system, None, false);
            Ok(())
        }
        Action::StartLlm { prompt, system: _ } => {
            // 多模态命令路由统一走 core commands::parse_ai_command（判定次序
            // 与措辞两端一致；结果浮层的重试 feed_ai_preview 也经此还原命令
            // 语义——重试 `//看图` 会重走 vision 截屏）。`//重置`/`//会话` 等
            // daemon 命令解析为 Llm 原样透传，前端不得拦截。
            // 进入生成前先收掉可能残留的拼音候选窗（提示词内拼音组合的候选）。
            hide_candidate_window(data);
            // 短语未命中（无配置/无此名称）按普通生成兜底，与原内联实现一致
            // （config 依赖留在前端，core 不引配置）。
            let cmd = parse_ai_command(prompt.trim());
            if let AiCommand::Phrase { name } = &cmd {
                if let Some(text) = lookup_phrase(name) {
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
            match cmd {
                // `//朗读 <文本>` → TTS 合成并播放（不落盘文本）。
                AiCommand::Tts { text } => {
                    log::info!("朗读命令: text={text}");
                    if let Some(comp) = data.composition.borrow_mut().take() {
                        let _ = edit_session::end_composition(context, clientid, &comp, "");
                    }
                    *data.machine.borrow_mut() = CompositionMachine::new();
                    start_tts_play(text);
                }
                // `//看图`：多模态 vision，直接捕捉眼睛区域（或全屏回退）发图给
                // LLM。与普通 `//` LLM 命令一致：不结束组合、不重置状态机，
                // 保持流式输出通道。
                AiCommand::Vision => {
                    log::info!("看图命令（vision）");
                    set_preedit_streaming_status(data, context, clientid);
                    start_llm(data, prompt, eye_rect_for(data, context), true);
                }
                // `//截图` / `//听写`：结束当前组合 + 重置状态机，异步采集识别。
                AiCommand::FullScreenOcr | AiCommand::Asr => {
                    let kind = if matches!(cmd, AiCommand::FullScreenOcr) {
                        TriggerKind::OcrFullScreen
                    } else {
                        TriggerKind::Asr
                    };
                    log::info!("触发命令: {kind:?}");
                    stash_ocr_anchor(data, context);
                    if let Some(comp) = data.composition.borrow_mut().take() {
                        let _ = edit_session::end_composition(context, clientid, &comp, "");
                    }
                    *data.machine.borrow_mut() = CompositionMachine::new();
                    trigger_async(data, kind);
                }
                // `//短语 名称` 未命中（已查表）与普通生成同路；daemon 命令
                // （`//重置` 等）解析为 Llm 原样透传。
                AiCommand::Phrase { .. } | AiCommand::Llm => {
                    // 发送即反馈：组合文本立刻换成「✨ 生成中…」——发送 → 首块
                    // 的 1-3s 此前完全无反馈（旧实现刻意不碰 preedit），用户
                    // 以为没发出而习惯性再敲一次 Enter，空提交把提示词一并
                    // 抹掉（真机 2026-09-04「AI 没回复」的直接根因）。非空
                    // 短串不触发 Notepad-- 的「空组合文本 → 应用终止组合」
                    // 陷阱（该陷阱专指空串，见 set_preedit_streaming_status 注）。
                    set_preedit_streaming_status(data, context, clientid);
                    let eye_rect = eye_rect_for(data, context);
                    let (eye_enabled, eye_mode) =
                        load_eye_runtime_cfg().unwrap_or((true, "ocr".to_owned()));
                    let use_vision = eye_enabled && eye_mode == "vision";
                    start_llm(data, prompt, eye_rect, use_vision);
                }
            }
            Ok(())
        }
        Action::ResultReady { text } => {
            // 生成完成 → 结果浮层就绪态（状态行切换为上屏/重试/改提示词提示）。
            // preedit 的短状态串刷新由同批 Final 的 Step::Preedit 负责（单一
            // preedit 写点），此处不碰组合。
            show_result_overlay(data, context, &text, ResultPhase::Ready);
            log::info!("AI 结果就绪: chars={}", text.chars().count());
            Ok(())
        }
        Action::RewriteReady { rewritten, source } => {
            // 改写完成 → 进入对照预览态 + 弹对照预览候选窗（1=改写结果
            // 2=原文）。机器必须同步 begin_rewrite_preview——此后 1/Enter/
            // 空格=改写、2=原文、Esc=取消才由 handle_key_down 的改写预览
            // 分支路由；漏调时拦截分支成死代码，窗口文案承诺的按键全部
            // 失效（审查发现）。组合串此时显示着流式结果（rewritten），
            // 保持不隐藏——预览候选窗叠加显示双候选；用户选定后组合由
            // CommitImmediate/Cancel 清理。
            data.machine
                .borrow_mut()
                .begin_rewrite_preview(rewritten.clone(), source.clone());
            show_rewrite_preview(data, context, &rewritten, &source);
            log::info!(
                "改写对照预览: rewritten_len={} source_len={}",
                rewritten.chars().count(),
                source.chars().count()
            );
            Ok(())
        }
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
        Action::OcrPreview { text } => {
            // 预览动作不经 apply_action（handle_key_down 的预览分支直处理）；
            // 走到这里属异常路径，回退直接上屏保不丢文本。
            log::warn!("OcrPreview 走到 apply_action（异常路径），直接上屏");
            edit_session::commit_text(context, clientid, &text)
        }
        Action::TriggerOcr => {
            // `///`：结束当前组合，触发选区截图 OCR（Ctrl+Alt+O 的键盘化替代）。
            hide_candidate_window(data);
            stash_ocr_anchor(data, context);
            if let Some(comp) = data.composition.borrow_mut().take() {
                let _ = edit_session::end_composition(context, clientid, &comp, "");
            }
            *data.machine.borrow_mut() = CompositionMachine::new();
            trigger_async(data, TriggerKind::Ocr);
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
            // 失败浮层保留：core 已入 Failed 态并保留 last_request（Enter/`r`
            // 重试、`e` 改提示词）。**不得结束组合**——组合里是短状态串，
            // end_composition("") 既踩空串陷阱又把刚弹的失败浮层立刻收掉
            // （表现为「错误一闪而过、按 r 无反应」，见 Action::LlmFailed 注）。
            log::warn!("LLM 失败: {message}");
            let (body, phase) = {
                let m = data.machine.borrow();
                (m.result().to_owned(), m.result_phase())
            };
            if phase == Some(ResultPhase::Failed) {
                // body 为已生成的部分结果（失败于首块前则为空——浮层仍有
                // 状态行的重试提示）。
                show_result_overlay(data, context, &body, ResultPhase::Failed);
            }
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
    start_llm_with_system(data, &prompt, None, eye_rect, use_vision)
}

/// start_llm 的系统提示词可注入变体（改写管道用）。
fn start_llm_with_system(
    data: &Rc<TextServiceData>,
    prompt: &str,
    system: Option<String>,
    eye_rect: Option<(i32, i32, i32, i32)>,
    use_vision: bool,
) {
    let prompt = prompt.to_owned();
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
        // 改写管道等调用方注入的系统提示词；None 时按用例（眼睛等）再定。
        let mut system: Option<String> = system;
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
/// Candidates(done=true) 释放 candidates_in_flight 并结算盲窗暂缓队列
/// deferred_intents（与 macOS imk.rs 的错误即空结果 done 结算对齐，
/// issue #44/#87），静默 return 会把组合永远卡在「在途」——空格/选字
/// 全被暂缓（复审发现；前端兜底是队列的唯一解药）。
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
                let _ = PostMessageW(Some(hwnd), WM_TIMER, WPARAM(TIMER_ID), LPARAM(0));
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
        candidates.push(
            PathBuf::from(appdata)
                .join("Verba")
                .join("ime")
                .join("verba-settings.exe"),
        );
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
        Some((rx, ry, rw, rh)) => verba_trigger::capture::capture_region(rx, ry, rw, rh).ok()?,
        None => verba_trigger::capture::capture_primary_screen().ok()?,
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
}

/// 把队列中捞出的流事件逐个喂给状态机，收集两段式派发步骤（纯函数，
/// 供单测钉住「done=true 空结果事件结算在途暂缓」「非刷新动作不丢弃」
/// 等布线语义，issue #44）。
fn collect_steps(machine: &mut CompositionMachine, events: Vec<StreamEvent>) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    // 合并结果更新：一个 tick 内多条 chunk 只派发**最后一条** UpdateResult
    // （body 是累积全文，中间态无显示价值）——每 chunk 一次 set_preedit +
    // 结果浮层全窗重绘 + GDI blit 代价高。preedit 的刷新在 apply_action 的
    // UpdateResult 臂内做（恒定短状态串），不再单独积累。
    let mut pending_result: Option<Action> = None;
    let mut pending_preedit: Option<String> = None;
    for evt in events {
        match evt.kind {
            Some(stream_event::Kind::Chunk(ch)) => {
                if let action @ Action::UpdateResult { .. } = machine.on_llm_chunk(&ch.text) {
                    pending_result = Some(action);
                }
            }
            Some(stream_event::Kind::Final(_)) => {
                // 先刷干容尽的结果更新，再派发完成动作（浮层先见末段流、
                // 再切就绪态）。
                if let Some(a) = pending_result.take() {
                    steps.push(Step::Act(a));
                }
                // 流完成 → 改写流产 RewriteReady（对照预览）、自由生成产
                // ResultReady（结果浮层就绪态），**两者都必须走 Act 派发**
                // ——此前 ResultReady 被静默丢弃且 apply_action 侧是空实现，
                // 两跳皆缺（与 RewriteReady 曾经的 bug 同型，issue #89）。
                if let action @ (Action::RewriteReady { .. } | Action::ResultReady { .. }) =
                    machine.on_llm_done()
                {
                    steps.push(Step::Act(action));
                }
                // preedit 换短状态串（不再塞全文——长结果进浮层）。空串会
                // 触发应用终止组合（空串陷阱），Idle 等场景丢弃不推。
                let p = machine.preedit();
                if !p.is_empty() {
                    pending_preedit = Some(p);
                }
            }
            Some(stream_event::Kind::Candidates(c)) => {
                // 先刷干容尽的结果更新与 chunk 预编辑，再显示候选。
                if let Some(a) = pending_result.take() {
                    steps.push(Step::Act(a));
                }
                if let Some(p) = pending_preedit.take() {
                    steps.push(Step::Preedit(p));
                }
                // settle 可能按序重放整队暂缓意图，产出动作序列（盲窗队列化，
                // issue #87）——逐个映射为步骤，两段式派发顺序不变。
                for action in machine.on_llm_candidates(&c.pinyin, &c.candidates, c.done) {
                    match action {
                        Action::UpdatePinyin {
                            preedit,
                            candidates,
                            page,
                            selected,
                            ..
                        } => steps.push(Step::Candidates {
                            preedit,
                            candidates,
                            page,
                            selected,
                        }),
                        // 在途暂缓后的知情回退（重复空格按原文提交）、settle
                        // 整队重放的提交等非刷新动作不能丢弃：与 macOS
                        // feed_candidates_event 的 CommitImmediate 处理对齐，
                        // 走通用派发上屏（复审发现：此前被静默吞掉，空格无效）。
                        other => steps.push(Step::Act(other)),
                    }
                }
            }
            Some(stream_event::Kind::Error(e)) => {
                // 失败浮层保留（core 已入 Failed 态，Enter/`r` 重试、`e` 改
                // 提示词）：LlmFailed 走 Act 派发，**不再 EndCompositionQuiet**
                // ——结束组合既踩空串陷阱，又会把刚弹的失败浮层与重试基础
                // 一并收掉（错误一闪而过、按 r 无反应）。
                if let Some(a) = pending_result.take() {
                    steps.push(Step::Act(a));
                }
                if let a @ Action::LlmFailed { .. } = machine.on_llm_error(&e.message) {
                    steps.push(Step::Act(a));
                }
                let p = machine.preedit();
                if !p.is_empty() {
                    pending_preedit = Some(p);
                }
            }
            None => {}
        }
    }
    if let Some(a) = pending_result.take() {
        steps.push(Step::Act(a));
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
        // 中英切换状态提示超时：Idle（无候选）时隐藏候选窗。
        if let Some(until) = self.ime_status_until.get() {
            if std::time::Instant::now() >= until {
                self.ime_status_until.set(None);
                if self.machine.borrow().state() == MachineState::Idle {
                    hide_candidate_window(&rc);
                }
            }
        }
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
        let Some(rc) = self.self_rc.borrow().as_ref().cloned() else {
            return;
        };
        for text in results {
            // 剪贴板照常（识别文本随手可粘贴）；上屏改为候选窗预览——
            // 用户看到再决定（Enter/空格/数字 1 上屏，Esc 取消）。
            crate::clipboard::set_text_quiet(&text);
            match self.machine.borrow_mut().begin_ocr_preview(text.clone()) {
                Some(Action::OcrPreview { text: t }) => {
                    show_ocr_preview(&rc, &t);
                    log::info!("OCR 结果进预览: chars={}", t.chars().count());
                }
                _ => {
                    // 非 Idle（组合中触发等罕见场景）回退直接上屏。
                    if let Some(context) = self.context.borrow().as_ref().cloned() {
                        let _ = edit_session::commit_text(&context, self.clientid.get(), &text);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_kind_requires_modifier_but_maps_vk() {
        // 无修饰键（测试环境 GetKeyState 为 0）时不应认作热键。
        assert_eq!(trigger_kind_for_vk(VK_O.0 as u32), None);
        // Ctrl+Alt+M（听写）随 ASR 冻结移除（#78）；S = 打开设置。
        assert_eq!(trigger_kind_for_vk(0x4D), None);
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
        assert_eq!(trigger_kind_for_hotkey_vk(0x4D), None, "M 热键已移除");
        // 其他键不在热键集合
        assert_eq!(trigger_kind_for_hotkey_vk(VK_UP.0 as u32), None);
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

    /// 布线语义钉住（issue #87 盲窗队列化）：暂缓队列 [空格, 标点] 在 settle
    /// 时按序重放为**一次合并提交**（首候选+全角标点），经 collect_steps 走
    /// Step::Act 派发——既不许只重放队首（旧单槽语义），也不许把序列拆成多
    /// 次上屏。
    #[test]
    fn collect_steps_queue_settle_replays_merged_commit_act() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        assert_eq!(m.feed_char(' '), Action::None, "空格先暂缓");
        assert!(matches!(m.feed_char(','), Action::UpdatePinyin { .. }));
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
            matches!(&steps[..], [Step::Act(Action::CommitImmediate(t))] if t == "你，"),
            "队列 settle 应合并为一次提交步骤，实际 {steps:?}"
        );
        assert_eq!(m.state(), MachineState::Idle);
    }

    /// 改写流完成布线钉住：Final 事件 → Step::Act(RewriteReady) 派发（不得
    /// 静默丢弃）。apply_action 侧须接住该动作调 begin_rewrite_preview——
    /// 两跳缺一，对照预览按键路由整条失效（审查发现：后一跳曾漏接，拦截
    /// 分支成死代码而单测全绿）。preedit 语义随 #89 改为**短状态串**（改写
    /// 结果全文进对照预览候选，不再塞 preedit）。
    #[test]
    fn collect_steps_rewrite_final_emits_rewrite_ready_act() {
        let mut m = CompositionMachine::new();
        for c in "//请假条".chars() {
            let _ = m.feed_char(c);
        }
        assert!(matches!(m.feed_char('\t'), Action::StartRewrite { .. }));
        let steps = collect_steps(
            &mut m,
            vec![
                StreamEvent {
                    id: 0,
                    kind: Some(stream_event::Kind::Chunk(verba_protos::Chunk {
                        text: "尊敬的经理：".into(),
                    })),
                },
                StreamEvent {
                    id: 0,
                    kind: Some(stream_event::Kind::Final(verba_protos::Final {
                        text: "尊敬的经理：".into(),
                    })),
                },
            ],
        );
        assert!(
            matches!(
                &steps[..],
                [Step::Act(Action::UpdateResult { body, .. }),
                 Step::Act(Action::RewriteReady { rewritten, source }),
                 Step::Preedit(p)]
                    if body == "尊敬的经理："
                        && rewritten == "尊敬的经理："
                        && source == "请假条"
                        && p == "✨ 已就绪"
            ),
            "改写 Final 应先派发末段流更新，再派发 RewriteReady，preedit 为短状态串，实际 {steps:?}"
        );
        assert_eq!(m.state(), MachineState::ResultReady);
        assert!(
            !m.rewrite_previewing(),
            "预览态由 apply_action 接住 RewriteReady 后才进入（前端职责）"
        );
    }

    /// 布线钉住（#89 两处丢弃的修复）：自由生成 Final → ResultReady **必须
    /// 产出 Step::Act**（此前被静默丢弃）且 preedit 为短状态串（不再塞全文）。
    /// apply_action 侧由 ResultReady 臂接住弹就绪浮层（原空实现，两跳皆缺
    /// 的第三个实例）。
    #[test]
    fn collect_steps_result_ready_final_emits_act_and_short_preedit() {
        let mut m = CompositionMachine::new();
        for c in "//123".chars() {
            let _ = m.feed_char(c);
        }
        assert!(matches!(m.feed_enter(), Action::StartLlm { .. }));
        let steps = collect_steps(
            &mut m,
            vec![
                StreamEvent {
                    id: 0,
                    kind: Some(stream_event::Kind::Chunk(verba_protos::Chunk {
                        text: "Hel".into(),
                    })),
                },
                StreamEvent {
                    id: 0,
                    kind: Some(stream_event::Kind::Chunk(verba_protos::Chunk {
                        text: "lo".into(),
                    })),
                },
                StreamEvent {
                    id: 0,
                    kind: Some(stream_event::Kind::Final(verba_protos::Final {
                        text: "Hello".into(),
                    })),
                },
            ],
        );
        assert!(
            matches!(
                &steps[..],
                [Step::Act(Action::UpdateResult { body, .. }),
                 Step::Act(Action::ResultReady { text }),
                 Step::Preedit(p)]
                    if body == "Hello" && text == "Hello" && p == "✨ 已就绪"
            ),
            "自由生成 Final 应派发末段流更新 + ResultReady，preedit 为短状态串，实际 {steps:?}"
        );
        assert_eq!(m.state(), MachineState::ResultReady);
        assert_eq!(m.result(), "Hello");
    }

    /// 失败浮层保留（#89 风险 5——后果最不对称的一处）：流错误经
    /// collect_steps 派发 LlmFailed 走 Act（apply_action 只弹失败浮层），
    /// **不得再产出结束组合步骤**；状态机停在 Failed 且重试基础保留
    /// （Enter 应产出 StartLlm 而非无动作）。
    #[test]
    fn collect_steps_error_keeps_failed_state_for_retry() {
        let mut m = CompositionMachine::new();
        for c in "//123".chars() {
            let _ = m.feed_char(c);
        }
        assert!(matches!(m.feed_enter(), Action::StartLlm { .. }));
        let steps = collect_steps(
            &mut m,
            vec![
                StreamEvent {
                    id: 0,
                    kind: Some(stream_event::Kind::Chunk(verba_protos::Chunk {
                        text: "He".into(),
                    })),
                },
                StreamEvent {
                    id: 0,
                    kind: Some(stream_event::Kind::Error(verba_protos::Error {
                        code: 500,
                        message: "模拟失败".into(),
                    })),
                },
            ],
        );
        assert!(
            matches!(
                &steps[..],
                [Step::Act(Action::UpdateResult { .. }),
                 Step::Act(Action::LlmFailed { .. }),
                 Step::Preedit(p)]
                    if p == "✨ 生成失败"
            ),
            "流错误应派发 LlmFailed（浮层保留），无结束组合步骤，实际 {steps:?}"
        );
        assert_eq!(m.state(), MachineState::Failed);
        assert_eq!(m.result(), "He", "已生成的部分结果保留");
        assert!(
            matches!(m.feed_enter(), Action::StartLlm { .. }),
            "失败态 Enter = 重试（last_request 保留）"
        );
    }

    /// 节流钉住（#89）：一个 tick 内多条 chunk 只派发**最后一条**
    /// UpdateResult（body 为累积全文，中间态无显示价值）——每 chunk 一次
    /// 全窗重绘 + GDI blit 不可接受。
    #[test]
    fn collect_steps_merges_chunks_per_tick() {
        let mut m = CompositionMachine::new();
        for c in "//123".chars() {
            let _ = m.feed_char(c);
        }
        assert!(matches!(m.feed_enter(), Action::StartLlm { .. }));
        let chunks: Vec<StreamEvent> = ["Hel", "lo ", "AI"]
            .iter()
            .map(|t| StreamEvent {
                id: 0,
                kind: Some(stream_event::Kind::Chunk(verba_protos::Chunk {
                    text: (*t).into(),
                })),
            })
            .collect();
        let steps = collect_steps(&mut m, chunks);
        assert!(
            matches!(
                &steps[..],
                [Step::Act(Action::UpdateResult { body, .. })] if body == "Hello AI"
            ),
            "一个 tick 的多条 chunk 应合并为一条 UpdateResult，实际 {steps:?}"
        );
    }

    /// 空串陷阱防御（Notepad-- 教训）：机器已离开流态（如用户中途 Enter
    /// 提交）后迟到的 Final 不得推出空 preedit 步骤——组合文本置空会触发
    /// 应用终止组合，把下一条流式输出全吞掉。
    #[test]
    fn collect_steps_stale_final_pushes_no_empty_preedit() {
        let mut m = CompositionMachine::new();
        for c in "//123".chars() {
            let _ = m.feed_char(c);
        }
        assert!(matches!(m.feed_enter(), Action::StartLlm { .. }));
        let _ = collect_steps(
            &mut m,
            vec![StreamEvent {
                id: 0,
                kind: Some(stream_event::Kind::Chunk(verba_protos::Chunk {
                    text: "He".into(),
                })),
            }],
        );
        // 用户中途 Enter：提交已生成部分 → Idle。
        assert!(matches!(m.feed_enter(), Action::CommitResult { .. }));
        let steps = collect_steps(
            &mut m,
            vec![StreamEvent {
                id: 0,
                kind: Some(stream_event::Kind::Final(verba_protos::Final {
                    text: "He".into(),
                })),
            }],
        );
        assert!(
            steps.is_empty(),
            "迟到 Final（机器 Idle）不应产出任何步骤（尤其空 preedit），实际 {steps:?}"
        );
    }

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
