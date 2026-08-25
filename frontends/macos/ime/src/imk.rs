//! macOS IMK 输入控制器（全 Rust：objc2 + objc2-input-method-kit）。
//!
//! 输入链路：`inputText:key:modifiers:client:` 收按键 → `verba-core` 组合状态机
//! （拼音组合 / `//` AI 模式）→ 上屏 / 标记文本 / 候选窗；LLM 流式经 daemon：
//! 工作线程把 `StreamEvent` 推入全局队列，主线程定时器排空喂给状态机。

#![cfg(target_os = "macos")]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{
    define_class, msg_send, sel, AnyThread, ClassType, DefinedClass, MainThreadMarker,
    MainThreadOnly,
};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{
    NSArray, NSAttributedString, NSBundle, NSDefaultRunLoopMode, NSInteger, NSNotFound,
    NSObjectProtocol, NSRange, NSRunLoop, NSString, NSTimer, NSUInteger,
};
use objc2_input_method_kit::{IMKInputController, IMKServer, IMKStateSetting};

use verba_core::machine::{Action, CompositionMachine, LlmCandidateRequest, MachineState};
use verba_protos::{stream_event, StreamEvent};

use crate::ipc;

/// IMKServer 连接名（与 app/Info.plist 的 `InputMethodConnectionName` 保持一致）。
pub const CONNECTION_NAME: &str = "Verba_1_Connection";
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

fn llm_queue() -> &'static Mutex<VecDeque<LlmItem>> {
    static Q: OnceLock<Mutex<VecDeque<LlmItem>>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// 全局自增序号（从 1 起，0 表示「无活跃请求」）。
static LLM_SEQ: AtomicU64 = AtomicU64::new(1);
/// 当前活跃的 LLM 流式请求序号。
static ACTIVE_STREAM: AtomicU64 = AtomicU64::new(0);
/// 当前活跃的候选融合请求序号。
static ACTIVE_CANDIDATES: AtomicU64 = AtomicU64::new(0);
/// 活跃流式请求的 daemon 侧 id（用于取消，0=无）。
static STREAM_DAEMON_ID: AtomicU64 = AtomicU64::new(0);
/// 活跃候选请求的 daemon 侧 id（用于取消，0=无）。
static CAND_DAEMON_ID: AtomicU64 = AtomicU64::new(0);

fn push_llm(seq: u64, event: StreamEvent) {
    if let Ok(mut q) = llm_queue().lock() {
        q.push_back(LlmItem { seq, event });
    }
}

/// 控制器实例变量（主线程独占；define_class 只暴露 `&Ivars`，故用内部可变性）。
struct Ivars {
    machine: RefCell<CompositionMachine>,
    /// 融合后的展示候选（当前页索引由 `page` 给出）。
    candidates: RefCell<Vec<String>>,
    page: Cell<usize>,
    /// 输入会话客户端（保留引用以跨回调上屏/标记）。
    client: RefCell<Option<Retained<AnyObject>>>,
    /// 流式排空定时器。
    timer: RefCell<Option<Retained<NSTimer>>>,
    /// 在途候选融合请求的拼音。
    candidate_pinyin: RefCell<Option<String>>,
    /// 当前组合文本（composedString: 数据源，供 updateComposition 使用）。
    composed: RefCell<String>,
    /// Rime 方案（单引擎，缓存；配置变更时热更新）。
    candidate_rime_schema: RefCell<String>,
    /// 配置 mtime（用于 Rime 方案热更新检测）。
    candidate_config_mtime: Cell<Option<std::time::SystemTime>>,
}

impl Default for Ivars {
    fn default() -> Self {
        Self {
            machine: RefCell::new(CompositionMachine::new()),
            candidates: RefCell::new(Vec::new()),
            page: Cell::new(0),
            client: RefCell::new(None),
            timer: RefCell::new(None),
            candidate_pinyin: RefCell::new(None),
            composed: RefCell::new(String::new()),
            candidate_rime_schema: RefCell::new("luna_pinyin_simp".to_owned()),
            candidate_config_mtime: Cell::new(None),
        }
    }
}

define_class!(
    // SAFETY: IMKInputController 的子类化无需额外约束。
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
            self.set_client(sender);
            self.reset();
            log::info!("[VerbaIMK] activateServer");
        }

        #[unsafe(method(deactivateServer:))]
        fn deactivate_server(&self, _sender: Option<&AnyObject>) {
            self.cancel_stream();
            self.invalidate_timer();
            self.reset();
            log::info!("[VerbaIMK] deactivateServer");
        }
    }

    // SAFETY: 覆盖父类（NSObjectIMKServerInput 类别）的输入方法。
    impl VerbaIMKController {
        /// 方式二：接收全部按键的 Unicode / keyCode / 修饰键。
        #[unsafe(method(inputText:key:modifiers:client:))]
        fn input_text(
            &self,
            string: Option<&NSString>,
            key_code: NSInteger,
            flags: NSUInteger,
            sender: Option<&AnyObject>,
        ) -> Bool {
            self.set_client(sender);

            // 带修饰键的组合键不吞（Cmd/Ctrl/Option 留给系统或宿主应用）。
            let mods = NSEventModifierFlags(flags);
            if mods.intersects(
                NSEventModifierFlags::Command
                    | NSEventModifierFlags::Control
                    | NSEventModifierFlags::Option,
            ) {
                return Bool::new(false);
            }

            let key = classify_key(string, key_code);
            // Shift+方向键等：交给宿主做文本选择，不当候选翻页。
            if mods.contains(NSEventModifierFlags::Shift)
                && matches!(key, Some(ImkKey::PageUp | ImkKey::PageDown))
            {
                return Bool::new(false);
            }

            // 多字符粘贴（keyCode=0 时整串到达）：逐字符喂入状态机并逐步应用动作。
            // 此前 classify_key 只取首字符，其余全部丢失（架构审查 P1-2）。
            let is_multi_paste = key_code == 0
                && string
                    .map(|s| s.to_string().chars().count() > 1)
                    .unwrap_or(false);
            if is_multi_paste {
                if let Some(s) = string {
                    let mut applied = false;
                    for ch in s.to_string().chars() {
                        // 与控制字符过滤一致（classify_key 同款规则）
                        if ch < ' ' || (0xF700..=0xF8FF).contains(&(ch as u32)) {
                            continue;
                        }
                        let action = self.ivars().machine.borrow_mut().feed_char(ch);
                        self.apply_action(action);
                        applied = true;
                    }
                    return Bool::new(applied);
                }
            }

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
                None => return Bool::new(false),
            };
            // 空闲态且状态机无动作（如 Enter/Backspace/Esc）：交给宿主处理。
            if was_idle && matches!(action, Action::None) {
                return Bool::new(false);
            }
            let _ = self.apply_action(action);
            Bool::new(true)
        }

        /// 组合文本数据源：updateComposition 调用它取当前 preedit 发给 client。
        #[unsafe(method_id(composedString:))]
        fn composed_string(&self, _sender: Option<&AnyObject>) -> Option<Retained<NSString>> {
            Some(NSString::from_str(&self.ivars().composed.borrow()))
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
            let ivars = self.ivars();
            let Some(global_idx) = ivars.candidates.borrow().iter().position(|c| c == &text)
            else {
                return;
            };
            let page = ivars.page.get();
            let Some(digit) = selection_digit(global_idx, page, CompositionMachine::PINYIN_PAGE_SIZE) else {
                return;
            };
            let action = self.ivars().machine.borrow_mut().feed_char(digit);
            let _ = self.apply_action(action);
        }

        /// 宿主要求结束组合（如焦点切换）：把当前组合内容提交。
        #[unsafe(method(commitComposition:))]
        fn commit_composition(&self, _sender: Option<&AnyObject>) {
            let text = {
                let mut m = self.ivars().machine.borrow_mut();
                let text = match m.state() {
                    MachineState::Pinyin | MachineState::Prompt => m.preedit(),
                    MachineState::Streaming | MachineState::ResultReady => m.result().to_owned(),
                    _ => String::new(),
                };
                m.feed_escape();
                text
            };
            if !text.is_empty() {
                self.commit(&text);
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
            unsafe { item.setTarget(Some(target)) };
            menu.addItem(&item);
            Some(menu)
        }

        /// 打开设置面板（菜单项 action）。
        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            match crate::ipc::settings_exe_path() {
                Some(p) => {
                    log::info!("[VerbaIMK] 打开设置面板: {}", p.display());
                    let _ = std::process::Command::new(&p).spawn();
                }
                None => log::warn!("[VerbaIMK] 未找到 verba-settings（VERBA_SETTINGS_PATH 或同目录）"),
            }
        }

        /// 主线程定时器：排空 daemon 流事件。
        #[unsafe(method(drainVerbaStream))]
        fn drain_stream(&self) {
            let items: Vec<LlmItem> = {
                let mut q = llm_queue().lock().unwrap();
                q.drain(..).collect()
            };
            if items.is_empty() {
                return;
            }
            let stream_seq = ACTIVE_STREAM.load(Ordering::SeqCst);
            let cand_seq = ACTIVE_CANDIDATES.load(Ordering::SeqCst);
            for item in items {
                if item.seq == stream_seq {
                    self.feed_stream_event(item.event);
                } else if item.seq == cand_seq {
                    self.feed_candidates_event(item.event);
                }
                // 其它序号的旧事件（已被新请求替代）直接丢弃。
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
        // 其余 keyCode 交给字符分支
        _ => string
            .and_then(|s| s.to_string().chars().next())
            .filter(|c| {
                // 过滤控制字符与 NS*FunctionKey（0xF700..0xF8FF）：这些走 keyCode 已处理，
                // 避免被误当作可打印字符提交。
                *c >= ' ' && !(0xF700..=0xF8FF).contains(&(*c as u32))
            })
            .map(ImkKey::Char),
    }
}

impl VerbaIMKController {
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

    fn set_client(&self, sender: Option<&AnyObject>) {
        if let Some(s) = sender {
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
                true
            }
            Action::CommitResult { text } => {
                // Streaming/ResultReady 提前 Enter：停流并停表，避免空转。
                self.cancel_stream();
                self.invalidate_timer();
                self.commit(&text);
                true
            }
            Action::EnterPrompt { preedit }
            | Action::UpdatePrompt { preedit }
            | Action::UpdateResult { preedit } => {
                self.set_marked(&preedit);
                true
            }
            Action::UpdatePinyin {
                preedit,
                candidates,
                page,
                llm_request,
            } => {
                self.ivars().candidates.borrow_mut().clone_from(&candidates);
                self.ivars().page.set(page);
                if let Some(req) = llm_request {
                    self.start_candidates(req);
                }
                self.set_marked(&preedit);
                true
            }
            Action::StartLlm { prompt, system } => {
                self.start_llm(prompt, system);
                true
            }
            Action::ResultReady => true,
            Action::Cancel => {
                self.cancel_stream();
                self.invalidate_timer();
                self.clear_composition();
                true
            }
            Action::LlmFailed { message } => {
                log::warn!("[VerbaIMK] LLM 失败: {message}");
                self.clear_composition();
                true
            }
        }
    }

    fn commit(&self, text: &str) {
        if let Some(client) = self.ivars().client.borrow().clone() {
            // 先清空标记文本，避免上屏后残留 preedit。
            let empty = NSString::from_str("");
            // SAFETY: client 是 IMK 输入会话客户端，setMarkedText/unmarkText/insertText
            // 均为 IMKInputText 非正式协议方法；NSNotFound 表示替换当前插入点。
            let _: () = unsafe {
                msg_send![
                    &client,
                    setMarkedText: &*empty,
                    selectionRange: NSRange::new(0, 0),
                    replacementRange: NSRange::new(NSNotFound as NSUInteger, 0),
                ]
            };
            let _: () = unsafe { msg_send![&client, unmarkText] };
            let ns = NSString::from_str(text);
            let _: () = unsafe {
                msg_send![
                    &client,
                    insertText: &*ns,
                    replacementRange: NSRange::new(NSNotFound as NSUInteger, 0),
                ]
            };
        }
        self.ivars().candidates.borrow_mut().clear();
        self.ivars().page.set(0);
        self.ivars().candidate_pinyin.borrow_mut().take();
        *self.ivars().composed.borrow_mut() = String::new();
    }

    fn set_marked(&self, text: &str) {
        *self.ivars().composed.borrow_mut() = text.to_owned();
        // SAFETY: updateComposition 为 IMKInputController 方法：取 composedString:
        // 并经 client setMarkedText: 发送，同时触发候选窗刷新。
        let _: () = unsafe { msg_send![self, updateComposition] };
    }

    fn clear_composition(&self) {
        *self.ivars().composed.borrow_mut() = String::new();
        // SAFETY: updateComposition 发空组合 → client 标记文本清空；再 unmark 兜底。
        let _: () = unsafe { msg_send![self, updateComposition] };
        if let Some(client) = self.ivars().client.borrow().clone() {
            let _: () = unsafe { msg_send![&client, unmarkText] };
        }
        self.ivars().candidates.borrow_mut().clear();
        self.ivars().page.set(0);
    }

    // ---- LLM 流式 ----

    fn start_llm(&self, prompt: String, system: Option<String>) {
        self.cancel_stream();
        let seq = LLM_SEQ.fetch_add(1, Ordering::SeqCst);
        ACTIVE_STREAM.store(seq, Ordering::SeqCst);

        std::thread::spawn(move || {
            let mut client = match ipc::ensure_daemon() {
                Ok(c) => c,
                Err(e) => {
                    push_llm(seq, error_event(&format!("无法连接 daemon: {e}")));
                    return;
                }
            };
            let id = match client.llm_start(&prompt, system.as_deref(), None, None, None) {
                Ok(id) => id,
                Err(e) => {
                    push_llm(seq, error_event(&format!("LLM 启动失败: {e}")));
                    return;
                }
            };
            STREAM_DAEMON_ID.store(id, Ordering::SeqCst);
            // 启动期间已被取消：立即向 daemon 取消并退出，避免空转。
            if ACTIVE_STREAM.load(Ordering::SeqCst) != seq {
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
        // 取消上一在途候选请求（含 LLM 融合），避免旧候选回流。
        let old_cand_id = CAND_DAEMON_ID.swap(0, Ordering::SeqCst);
        ACTIVE_CANDIDATES.store(0, Ordering::SeqCst);
        if old_cand_id != 0 {
            let old = old_cand_id;
            std::thread::spawn(move || {
                if let Ok(mut c) = ipc::try_connect() {
                    let _ = c.llm_cancel(old);
                }
            });
        }
        let seq = LLM_SEQ.fetch_add(1, Ordering::SeqCst);
        self.ivars()
            .candidate_pinyin
            .borrow_mut()
            .replace(pinyin.clone());
        ACTIVE_CANDIDATES.store(seq, Ordering::SeqCst);
        self.ensure_timer();

        std::thread::spawn(move || {
            let mut client = match ipc::ensure_daemon() {
                Ok(c) => c,
                Err(e) => {
                    push_llm(seq, error_event(&format!("Rime 候选查询失败: {e}")));
                    return;
                }
            };
            let cands = match client.rime_candidates(&pinyin, &schema, 9) {
                Ok(c) => c,
                Err(e) => {
                    push_llm(seq, error_event(&format!("Rime 候选查询失败: {e}")));
                    return;
                }
            };
            push_llm(
                seq,
                StreamEvent {
                    id: 0,
                    kind: Some(stream_event::Kind::Candidates(verba_protos::Candidates {
                        pinyin,
                        candidates: cands,
                        done: true,
                    })),
                },
            );
        });
    }

    fn feed_stream_event(&self, evt: StreamEvent) {
        let action = match evt.kind {
            Some(stream_event::Kind::Chunk(ch)) => {
                self.ivars().machine.borrow_mut().on_llm_chunk(&ch.text)
            }
            Some(stream_event::Kind::Final(_)) => self.ivars().machine.borrow_mut().on_llm_done(),
            Some(stream_event::Kind::Error(e)) => {
                self.ivars().machine.borrow_mut().on_llm_error(&e.message)
            }
            _ => Action::None,
        };
        match action {
            Action::UpdateResult { preedit } | Action::UpdatePrompt { preedit } => {
                self.set_marked(&preedit);
            }
            Action::ResultReady => {
                self.set_marked(self.ivars().machine.borrow().result());
                self.invalidate_timer();
            }
            Action::LlmFailed { message } => {
                log::warn!("[VerbaIMK] LLM 失败: {message}");
                self.clear_composition();
                self.invalidate_timer();
            }
            Action::None => {}
            other => log::debug!("[VerbaIMK] 流事件产生其它动作: {other:?}"),
        }
    }

    fn feed_candidates_event(&self, evt: StreamEvent) {
        let Some(kind) = evt.kind else {
            return;
        };
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
                let action = self.ivars().machine.borrow_mut().on_llm_candidates(
                    &pinyin,
                    &c.candidates,
                    c.done,
                );
                if let Action::UpdatePinyin {
                    preedit,
                    candidates,
                    page,
                    ..
                } = action
                {
                    self.ivars().candidates.borrow_mut().clone_from(&candidates);
                    self.ivars().page.set(page);
                    self.set_marked(&preedit);
                }
                if c.done {
                    self.ivars().candidate_pinyin.borrow_mut().take();
                    ACTIVE_CANDIDATES.store(0, Ordering::SeqCst);
                }
            }
            stream_event::Kind::Error(e) => {
                // Rime 候选错误：不再静默空白，落到日志便于排查（如 librime 未部署）。
                log::warn!("[VerbaIMK] Rime 候选错误: {}", e.message);
                self.ivars().candidate_pinyin.borrow_mut().take();
                ACTIVE_CANDIDATES.store(0, Ordering::SeqCst);
            }
            _ => {}
        }
    }

    fn cancel_stream(&self) {
        ACTIVE_STREAM.store(0, Ordering::SeqCst);
        ACTIVE_CANDIDATES.store(0, Ordering::SeqCst);
        let stream_id = STREAM_DAEMON_ID.swap(0, Ordering::SeqCst);
        let cand_id = CAND_DAEMON_ID.swap(0, Ordering::SeqCst);
        self.ivars().candidate_pinyin.borrow_mut().take();
        // 尽力向 daemon 取消在途请求，让工作线程尽快退出、停止生成资源。
        if stream_id != 0 || cand_id != 0 {
            std::thread::spawn(move || {
                if let Ok(mut c) = ipc::try_connect() {
                    if stream_id != 0 {
                        let _ = c.llm_cancel(stream_id);
                    }
                    if cand_id != 0 {
                        let _ = c.llm_cancel(cand_id);
                    }
                }
            });
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
                0.05,
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

    // SAFETY: run_server 只能从 main()（主线程）调用。
    let mtm = unsafe { MainThreadMarker::new_unchecked() };

    // 强制注册控制器类（define_class! 懒注册）。
    let _ = VerbaIMKController::class();

    let app = NSApplication::sharedApplication(mtm);
    let bundle = NSBundle::mainBundle();
    let bundle_id = bundle
        .bundleIdentifier()
        .unwrap_or_else(|| NSString::from_str("dev.verba.inputmethod.Verba"));
    let conn = NSString::from_str(CONNECTION_NAME);

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
}
