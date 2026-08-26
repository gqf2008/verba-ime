//! macOS IMK 输入控制器（全 Rust：objc2 + objc2-input-method-kit）。
//!
//! 输入链路：`inputText:key:modifiers:client:` 收按键 → `verba-core` 组合状态机
//! （拼音组合 / `//` AI 模式）→ 上屏 / 标记文本 / 候选窗；LLM 流式经 daemon：
//! 工作线程把 `StreamEvent` 推入全局队列，主线程定时器排空喂给状态机。

#![cfg(target_os = "macos")]

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
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
use verba_ipc::name::local_entropy_u64;
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

/// 全局自增序号（从 1 起，0 表示「无活跃请求」）。序号全局唯一：事件带 seq，
/// 各控制器按自己的活跃 seq 消费，互不干扰（架构审查 P2-1 per-controller）。
static LLM_SEQ: AtomicU64 = AtomicU64::new(1);

/// 全局自增会话序号（从 1 起）：每个输入控制器独占一个 AI 多轮上下文会话，
/// daemon 按 session_id 分组隔离历史（架构审查会话维度 B4b）。
static SESSION_ID_SEQ: AtomicU64 = AtomicU64::new(1);

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
    /// 本控制器的活跃 LLM 流序号（0=无）。per-controller（架构审查 P2-1）：
    /// 多会话（多应用文本域）各自消费自己 seq 的事件，互不串流。
    active_stream: Cell<u64>,
    /// 本控制器的活跃候选请求序号（0=无）。
    active_candidates: Cell<u64>,
    /// 最近废弃的序号集合（取消的流 / 被取代的候选请求）：迟到事件（取消后
    /// 补发的 Final、慢响应的旧候选）入队时 active_* 已归 0/换号无法匹配——
    /// 按此集合在 drain 丢弃，防全局队列无界滞留（复审 V7）。有界（32 条）：
    /// 更早的序号其 worker 早已退出、无在途事件。
    dead_seqs: RefCell<VecDeque<u64>>,
    /// 本控制器的 AI 多轮上下文会话 id（创建时分配，全局唯一）。daemon 按此
    /// 隔离历史：多文本域（多应用）各自独立多轮，互不串上下文（B4b）。
    session_id: Cell<u64>,
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
            active_stream: Cell::new(0),
            active_candidates: Cell::new(0),
            dead_seqs: RefCell::new(VecDeque::new()),
            session_id: Cell::new(alloc_session_id()),
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
            // 候选查询按粘贴整体合并为一次（见下）：原先每字符一个 UpdatePinyin
            // 触发一次 start_candidates → Rime worker 线程 + daemon 查询，超长
            // 粘贴（数百字）会瞬时放大为同等规模的线程/查询/主线程 marked-text
            // 更新风暴（复审发现）；中间态仅按状态机候选刷新显示，循环结束后
            // 对最终拼音补发一次查询，候选结果由 seq 过滤保证只消费最新代。
            let pasted = if key_code == 0 {
                string.map(|s| s.to_string())
            } else {
                None
            };
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

        /// 主线程定时器：排空本控制器的事件。
        ///
        /// 只取走本控制器活跃序号的事件，其余留在队列（多会话场景下另一控制器
        /// 的定时器会消费自己的部分——整体清空会互相丢弃事件）。废弃序号
        /// （已取消流、被取代的旧候选）的残留事件直接丢弃，防全局队列无界滞留。
        #[unsafe(method(drainVerbaStream))]
        fn drain_stream(&self) {
            let stream_seq = self.ivars().active_stream.get();
            let cand_seq = self.ivars().active_candidates.get();
            // 空队列快速路径：避免每 50ms 无事件时仍 borrow dead_seqs。
            if llm_queue().lock().unwrap().is_empty() {
                return;
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
                        // 事件常跨多个 50ms tick（缓冲 chunk + 守护补发的 Final），
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
                // 超限丢弃最旧条目——常规下活跃流事件每 50ms 被其控制器取走，
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
        self.ivars().active_stream.set(seq);
        let session_id = self.ivars().session_id.get();

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
            let id =
                match client.llm_start(&prompt, system.as_deref(), None, None, None, session_id) {
                    Ok(id) => id,
                    Err(e) => {
                        push_llm(seq, error_event(&format!("LLM 启动失败: {e}")));
                        cancelled_seqs().lock().unwrap().remove(&seq);
                        return;
                    }
                };
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
        // 当前 active_candidates 序号的事件）。
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
                    self.ivars().active_candidates.set(0);
                }
            }
            stream_event::Kind::Error(e) => {
                // Rime 候选错误：不再静默空白，落到日志便于排查（如 librime 未部署）。
                log::warn!("[VerbaIMK] Rime 候选错误: {}", e.message);
                self.ivars().candidate_pinyin.borrow_mut().take();
                self.ivars().active_candidates.set(0);
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
}
