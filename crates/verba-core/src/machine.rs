//! 输入组合状态机。
//!
//! 状态流转：
//! ```text
//! Idle --字母--> Pinyin --(Space/数字/Enter 选候选)--> 提交中文
//! Idle --'/'--> PendingSlash --'/'--> Prompt --Enter--> Streaming --(流完)--> ResultReady --Enter--> Idle
//!   ^                |                 |                                              |
//!   |                +-- 其它字符: 提交 "/x"                                            |
//!   +--- Esc/Backspace 取消 <---------+-----------------------------------------------+
//! ```

use std::fmt;

/// 候选：文本 + 覆盖的输入拼音字符数（用于分段承诺/整句提交）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegCandidate {
    pub text: String,
    /// 覆盖的输入拼音字符数。
    pub consumed: usize,
}

/// 输入法模式（与 IPC SetMode 对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Voice,
    Ocr,
    Ai,
}

impl Mode {
    /// 该模式下是否允许普通按键直接上屏。
    pub fn accepts_direct_input(self) -> bool {
        matches!(self, Self::Normal)
    }

    /// 协议字符串表示。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Voice => "voice",
            Self::Ocr => "ocr",
            Self::Ai => "ai",
        }
    }

    /// 从协议字符串解析。
    pub fn from_proto_str(s: &str) -> Option<Self> {
        match s {
            "normal" => Some(Self::Normal),
            "voice" => Some(Self::Voice),
            "ocr" => Some(Self::Ocr),
            "ai" => Some(Self::Ai),
            _ => None,
        }
    }
}

/// 状态机所处阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineState {
    /// 空闲：普通直输。
    Idle,
    /// 拼音组合中（缓冲区见 [`CompositionMachine::pinyin_buffer`]）。
    Pinyin,
    /// 已输入一个 `/`，等待第二个 `/` 或其它字符。
    PendingSlash,
    /// AI 提示词输入中（`//` 已消费）。
    Prompt,
    /// LLM 流式输出中。
    Streaming,
    /// LLM 已输出完毕，等待 Enter 上屏 / Esc 取消。
    ResultReady,
}

/// OCR 预览态的按键分类（feed_ocr_preview 的输入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKey {
    Enter,
    Space,
    Digit1,
    Escape,
    Other,
}

/// 前端应执行的动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 无动作。
    None,
    /// 立即上屏指定文本。
    CommitImmediate(String),
    /// 进入 AI 提示词模式，preedit 显示指定文本。
    EnterPrompt { preedit: String },
    /// 提示词更新，preedit 显示指定文本。
    UpdatePrompt { preedit: String },
    /// 拼音 preedit 更新（preedit 为纯拼音，候选单独给前端渲染候选窗）。
    /// `page` 为当前候选页码（0 起，每页 `PINYIN_PAGE_SIZE` 个）。
    /// `selected` 为当前选中候选下标（页内，0 起；方向键选字维护）。
    /// `llm_request`：拼音变更后是否需向 LLM 请求融合候选（前端负责防抖与取消）。
    UpdatePinyin {
        preedit: String,
        candidates: Vec<String>,
        page: usize,
        selected: usize,
        llm_request: Option<LlmCandidateRequest>,
    },
    /// 提示词模式下按下 Enter：发起 LLM 生成。
    StartLlm {
        prompt: String,
        system: Option<String>,
    },
    /// LLM 流式增量更新，preedit 显示结果。
    UpdateResult { preedit: String },
    /// LLM 输出完毕，等待用户确认。
    ResultReady,
    /// 确认上屏最终结果。
    CommitResult { text: String },
    /// `///`：Prompt 态空提示词按第三个斜杠 → 触发选区截图 OCR
    /// （Ctrl+Alt+O 的键盘化替代）。
    TriggerOcr,
    /// OCR 结果到达 → 进候选窗预览（首条=识别文本，Enter/空格/数字上屏；
    /// Esc 取消）。不直接插光标——用户看到了再决定。
    OcrPreview { text: String },
    /// `//<内容>` + Tab：提示词内容走改写管道（润色/纠错/成文），
    /// 前端发起 LLM 请求；流式结果沿用 Streaming/ResultReady 通道。
    StartRewrite { content: String },
    /// 取消当前组合（Esc / 清空）。
    Cancel,
    /// LLM 出错，已回到 Idle。
    LlmFailed { message: String },
}

/// Rime 候选请求：拼音 + 现有候选（供 daemon 查询 Rime；单引擎）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmCandidateRequest {
    pub pinyin: String,
    pub dictionary: Vec<String>,
}

impl std::str::FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_proto_str(s).ok_or_else(|| format!("未知模式: {s}"))
    }
}

/// 组合状态机。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionMachine {
    state: MachineState,
    /// 拼音组合缓冲（小写无调）。
    pinyin_buffer: String,
    /// 词库候选（由拼音引擎查询得到；含覆盖拼音长度，用于分段承诺）。
    dictionary_candidates: Vec<SegCandidate>,
    /// LLM / Rime 补充候选（候选融合，可为空；一律覆盖当前活跃拼音全长）。
    llm_candidates: Vec<SegCandidate>,
    /// 融合后的展示候选（词库 + LLM，去重），供选择/分页/提交。
    pinyin_candidates: Vec<SegCandidate>,
    /// 已承诺的候选段（文本 + 覆盖的拼音字符数）。
    ///
    /// 单引擎（Rime）下候选 `consumed = 活跃拼音全长`，选中即整句提交，此字段恒空；
    /// 「分段承诺」需 daemon 返回覆盖长度后方可启用（当前保留结构）。
    committed: Vec<(String, usize)>,
    /// 已承诺段累计覆盖的拼音缓冲前缀长度。
    commit_offset: usize,
    /// 已发起 LLM 候选请求的拼音（避免同一拼音重复请求）。
    last_candidates_request: Option<String>,
    /// 候选请求是否在途（已发出、Rime 结果未回）。
    /// 快速输入时若此刻按空格选首候选，会把拼音原文上屏——须暂缓到结果到达。
    candidates_in_flight: bool,
    /// 盲按窗口内暂缓的意图：候选在途且零已知真实结果时，空格/大写/标点
    /// 的提交通道都不立即按原文提交——记下最新一次按键，结果 settle 后重放。
    /// 最新击键覆盖旧暂缓（被替换的键已被吞，属盲按保护的可接受损失）。
    deferred_intent: Option<DeferredIntent>,
    /// 当前候选页码（0 起）。
    pinyin_page: usize,
    /// 当前选中候选下标（页内，0 起；方向键 Up/Down 移动，候选刷新时归 0）。
    selected_index: usize,
    /// OCR 预览文本（preview 状态期间候选窗首条显示它；None=非预览态）。
    ocr_preview: Option<String>,
    /// AI 提示词（不含 `//` 前缀）。
    prompt: String,
    /// LLM 流式结果。
    result: String,
    /// 成对引号交替开闭状态，**双引号与单引号各自独立交替**（false=下一个是
    /// 开引号）。全角引号无左右键位，按 IME 惯例同键交替；跨组合延续（会话级，
    /// 不复位）。两键共用一个标志会让 `"` 后紧跟 `'` 产出「“’」错配对。
    double_quote_open: bool,
    single_quote_open: bool,
}

/// 半角 → 全角标点映射表（成对引号另由状态机交替处理，不入表；
/// `-`/`=` 在拼音组合中作翻页键优先消费，不会进入标点路径）。
fn fullwidth_punct(c: char) -> Option<char> {
    Some(match c {
        ',' => '，',
        '.' => '。',
        ';' => '；',
        ':' => '：',
        '?' => '？',
        '!' => '！',
        '\\' => '、',
        '(' => '（',
        ')' => '）',
        '[' => '【',
        ']' => '】',
        '{' => '｛',
        '}' => '｝',
        '<' => '《',
        '>' => '》',
        '-' => '－',
        '$' => '￥',
        '~' => '～',
        _ => return None,
    })
}

/// 该字符会被状态机标点路径消费（全角映射或成对引号交替）。
///
/// 供前端「是否认领该键」的路由判定（Windows TSF should_claim_key）：认领
/// 后按键经状态机输出全角——此前 Windows 宿主直插半角与 macOS 契约不一致
/// （跨平台审查发现）。判定与映射表同源，杜绝两端清单漂移。
pub fn is_fullwidth_mapped_punct(c: char) -> bool {
    c == '"' || c == '\'' || fullwidth_punct(c).is_some()
}

/// 盲按窗口内暂缓的提交意图（见 `deferred_intent` 字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredIntent {
    /// 空格：settle 后有真实结果则选首候选；零结果不提交（展示合成项，
    /// 再按一次空格才是知情选择）。
    SelectSpace,
    /// 大写字符：settle 后按「候选 0（或原文）+ 字符」通道补执行。
    Uppercase(char),
    /// 标点：settle 后按「候选 0（或原文）+ 全角标点」通道补执行。
    Punct(char),
}

impl Default for CompositionMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl CompositionMachine {
    pub fn new() -> Self {
        Self {
            state: MachineState::Idle,
            pinyin_buffer: String::new(),
            dictionary_candidates: Vec::new(),
            llm_candidates: Vec::new(),
            pinyin_candidates: Vec::new(),
            committed: Vec::new(),
            commit_offset: 0,
            last_candidates_request: None,
            candidates_in_flight: false,
            deferred_intent: None,
            pinyin_page: 0,
            selected_index: 0,
            ocr_preview: None,
            prompt: String::new(),
            result: String::new(),
            double_quote_open: false,
            single_quote_open: false,
        }
    }

    pub fn state(&self) -> MachineState {
        self.state
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn result(&self) -> &str {
        &self.result
    }

    /// 当前应显示的 preedit 文本（无组合时为空）。
    pub fn preedit(&self) -> String {
        match self.state {
            MachineState::PendingSlash => "/".to_owned(),
            MachineState::Pinyin => self.pinyin_preedit(),
            MachineState::Prompt => {
                if self.pinyin_composing() {
                    format!("//{}{}", self.prompt, self.pinyin_preedit())
                } else {
                    format!("//{}", self.prompt)
                }
            }
            MachineState::Streaming | MachineState::ResultReady => self.result.clone(),
            MachineState::Idle => String::new(),
        }
    }

    /// 每页候选数（与候选窗主题 page_size 对齐）。
    pub const PINYIN_PAGE_SIZE: usize = 9;

    /// 拼音组合缓冲上限（字母数）。手心式「装不下不再输入」：到顶后字母不再
    /// 入缓冲（退格仍可删），同时约束 preedit/查询/候选面板规模——长句候选
    /// 慢与不全的源头之一。
    pub const MAX_PINYIN_BUFFER: usize = 48;

    /// 翻到上一页（仅拼音态；无候选或单页时无动作）。
    pub fn feed_page_up(&mut self) -> Action {
        self.paginate(false)
    }

    /// 翻到下一页（仅拼音态；无候选或单页时无动作）。
    pub fn feed_page_down(&mut self) -> Action {
        self.paginate(true)
    }

    fn paginate(&mut self, next: bool) -> Action {
        if self.state != MachineState::Pinyin || self.pinyin_candidates.is_empty() {
            return Action::None;
        }
        let total = self
            .pinyin_candidates
            .len()
            .div_ceil(Self::PINYIN_PAGE_SIZE);
        if total <= 1 {
            return Action::None;
        }
        if next {
            self.pinyin_page = (self.pinyin_page + 1) % total;
        } else {
            self.pinyin_page = (self.pinyin_page + total - 1) % total;
        }
        // 翻页后选中回到新页首项。
        self.selected_index = 0;
        Action::UpdatePinyin {
            preedit: self.pinyin_composition_preedit(),
            candidates: self.display_candidate_texts(),
            page: self.pinyin_page,
            selected: self.selected_index,
            llm_request: None,
        }
    }

    /// 输入一个可打印字符。
    /// 中文态标点 → 全角输出（Chinese IME 惯例）。未列入表中的符号
    /// （@ # % ^ & * _ + = / | \` 等）保持半角直通。
    fn punct_commit_text(&mut self, c: char) -> String {
        match c {
            // 成对引号同键交替开闭（全角引号无左右键位）；双/单引号独立配对
            '"' => {
                self.double_quote_open = !self.double_quote_open;
                if self.double_quote_open { "“" } else { "”" }.to_owned()
            }
            '\'' => {
                self.single_quote_open = !self.single_quote_open;
                if self.single_quote_open { "‘" } else { "’" }.to_owned()
            }
            other => fullwidth_punct(other).unwrap_or(other).to_string(),
        }
    }

    pub fn feed_char(&mut self, c: char) -> Action {
        match self.state {
            MachineState::Idle => {
                if c == '/' {
                    self.state = MachineState::PendingSlash;
                    Action::UpdatePrompt {
                        preedit: "/".to_owned(),
                    }
                } else if c.is_ascii_uppercase() {
                    // 大写 ASCII（Shift+字母）直上屏，不进拼音组合（IME 惯例）
                    Action::CommitImmediate(c.to_string())
                } else if c.is_ascii_alphabetic() {
                    // 字母开始拼音组合
                    self.state = MachineState::Pinyin;
                    self.pinyin_buffer.clear();
                    self.pinyin_buffer.push(c.to_ascii_lowercase());
                    self.refresh_candidates();
                    Action::UpdatePinyin {
                        preedit: self.pinyin_composition_preedit(),
                        candidates: self.display_candidate_texts(),
                        page: self.pinyin_page,
                        selected: self.selected_index,
                        llm_request: self.request_llm_candidates_if_needed(),
                    }
                } else {
                    // 可打印非字母：标点转全角（直通符号原样），见 punct_commit_text
                    Action::CommitImmediate(self.punct_commit_text(c))
                }
            }
            MachineState::PendingSlash => {
                if c == '/' {
                    self.state = MachineState::Prompt;
                    self.prompt.clear();
                    Action::EnterPrompt {
                        preedit: "//".to_owned(),
                    }
                } else {
                    self.state = MachineState::Idle;
                    Action::CommitImmediate(format!("/{c}"))
                }
            }
            MachineState::Pinyin => self.feed_pinyin_char(c),
            MachineState::Prompt => self.feed_prompt_char(c),
            MachineState::Streaming | MachineState::ResultReady => Action::None,
        }
    }

    /// 拼音状态下的字符输入。
    fn feed_pinyin_char(&mut self, c: char) -> Action {
        if c == '/' {
            // 裁决（issue #44「'/' 通道盲窗直出标注」）：'/' 是显式模式切换键，
            // 盲窗中保持直出、不同走暂缓——①用户按 '/' 的意图是进入提示词模式
            // 而非「期望候选」，提交的只是自己刚敲的拼音字母，可感知、可退格
            // 撤销，不属于盲窗保护针对的「静默退化漏字」；②若同走暂缓，settle
            // 重放会把已解决的候选插进用户正在输入的提示词组合里（Prompt 态
            // 活跃时插入文本），引入新的竞态与错序风险，收益远小于风险。
            // 行为由 slash_in_blind_window_commits_raw_enters_pending 钉住。
            // 提交当前拼音，再进入 AI 触发
            let text = self.commit_pinyin_text();
            self.reset_pinyin();
            self.state = MachineState::PendingSlash;
            return Action::CommitImmediate(text);
        }
        if c == '-' {
            return self.paginate(false);
        }
        if c == '=' {
            return self.paginate(true);
        }
        if c.is_ascii_digit() && c != '0' {
            let idx = self.pinyin_page * Self::PINYIN_PAGE_SIZE + (c as u8 - b'1') as usize;
            if idx < self.pinyin_candidates.len() {
                return self.select_candidate(idx);
            }
            // 候选不存在：忽略该数字（不吞后文，也不提交原文）。
            return Action::None;
        }
        if c.is_ascii_alphabetic() {
            if c.is_ascii_uppercase() {
                // 大写：提交当前候选（或原文）+ 该字符，避免吞字（与其它可打印字符一致）
                if self.blind_window() {
                    // 盲窗（查询在途且零已知结果）：按原文提交会带上拼音字母——
                    // 与空格同走暂缓，settle 后重放本通道。
                    self.deferred_intent = Some(DeferredIntent::Uppercase(c));
                    return self.pinyin_action();
                }
                let text = format!("{}{c}", self.commit_pinyin_text());
                self.reset_pinyin();
                return Action::CommitImmediate(text);
            }
            if self.pinyin_buffer.len() >= Self::MAX_PINYIN_BUFFER {
                // 组合长度到顶：吞掉后续字母（装不下不再输入），退格仍可删
                return Action::None;
            }
            self.pinyin_buffer.push(c.to_ascii_lowercase());
            self.refresh_candidates();
            return self.pinyin_action();
        }
        if c == ' ' {
            // 空格：选当前选中候选（方向键移动后按空格提交选中项）。
            // selected_index 是页内下标，须换算成全量列表的全局下标——
            // 翻页后 selected_index 归 0，直接用页内下标会提交到第一页
            // （真机：翻到第 2 页空格上屏的却是第 1 页首项）。
            let global = self.pinyin_page * Self::PINYIN_PAGE_SIZE + self.selected_index;
            return self.select_candidate(global);
        }
        // 其它可打印字符：提交候选 0 + 该字符，避免吞字；标点同时转全角
        if self.blind_window() {
            // 盲窗同上：暂缓到 settle 重放（在 punct_commit_text 之前登记，
            // 引号交替等映射状态留待重放时恰好翻转一次）。
            self.deferred_intent = Some(DeferredIntent::Punct(c));
            return self.pinyin_action();
        }
        let punct = self.punct_commit_text(c);
        let text = format!("{}{punct}", self.commit_pinyin_text());
        self.reset_pinyin();
        Action::CommitImmediate(text)
    }

    /// 盲按窗口：查询在途且尚无任何已知真实结果。此刻所有「候选优先、
    /// 原文兜底」的提交通道都会退化为原文提交——即漏字，须暂缓。
    fn blind_window(&self) -> bool {
        self.candidates_in_flight
            && self.dictionary_candidates.is_empty()
            && self.llm_candidates.is_empty()
    }

    /// 提示词态的字符输入：支持拼音组合（字母→候选→选中上屏到提示词）。
    /// 无候选时按原文提交（保住 `//translate hello` 这类英文提示词流程）。
    fn feed_prompt_char(&mut self, c: char) -> Action {
        if self.pinyin_composing() {
            if c.is_ascii_alphabetic() {
                if c.is_ascii_uppercase() {
                    // 大写：提交拼音候选（或原文）+ 大写字符到提示词
                    self.prompt.push_str(&self.commit_pinyin_text());
                    self.prompt.push(c);
                    self.clear_pinyin();
                    return Action::UpdatePrompt {
                        preedit: self.preedit(),
                    };
                }
                if self.pinyin_buffer.len() >= Self::MAX_PINYIN_BUFFER {
                    // 组合长度到顶：吞掉后续字母（同主组合，见 MAX_PINYIN_BUFFER）
                    return Action::None;
                }
                self.pinyin_buffer.push(c.to_ascii_lowercase());
                self.refresh_candidates();
                // 提示词内拼音组合：走 Rime 候选（与主组合一致），前端已能处理 UpdatePinyin。
                return self.pinyin_action();
            }
            if c.is_ascii_digit() && c != '0' {
                let idx = (c as u8 - b'1') as usize;
                if let Some(seg) = self.pinyin_candidates.get(idx) {
                    self.prompt.push_str(&seg.text);
                    self.clear_pinyin();
                    return Action::UpdatePrompt {
                        preedit: self.preedit(),
                    };
                }
                return Action::None;
            }
            if c == ' ' || c == '/' {
                // 不做在途暂缓（主组合有暂缓+双击回退，这里刻意保留即时
                // 回退）：英文提示词逐键都会刷新候选查询，若首按空格被吞，
                // 「//translate␣」这类整词输入永远无法出词（回归测试
                // prompt_english_fallback_commits_raw 锁定此语义）。残余竞态：
                // 中文拼音在提示词内于结果未达时按空格会先见原文——结果到达
                // 后按退格重选的代价远小于英文路径被卡死。
                // 空格/斜杠：提交候选（或原文）后，空格入提示词、斜杠交给 AI 触发判定
                let text = self.commit_pinyin_text();
                self.prompt.push_str(&text);
                self.clear_pinyin();
                if c == '/' {
                    // 斜杠在提示词中按字面加入（无特殊触发）
                    self.prompt.push('/');
                }
                return Action::UpdatePrompt {
                    preedit: self.preedit(),
                };
            }
            // 其它可打印字符：提交拼音 + 追加该字符
            self.prompt.push_str(&self.commit_pinyin_text());
            self.prompt.push(c);
            self.clear_pinyin();
            return Action::UpdatePrompt {
                preedit: self.preedit(),
            };
        }
        // 未组合：Tab = 改写管道（提示词内容非空）；字母开始拼音；其它字符直接入提示词
        if c == '\t' {
            if self.prompt.is_empty() {
                return Action::None; // 空内容无改写对象
            }
            let content = std::mem::take(&mut self.prompt);
            self.state = MachineState::Streaming;
            self.result.clear();
            return Action::StartRewrite { content };
        }
        if c == '/' && self.prompt.is_empty() {
            // `///`：第三个斜杠（提示词空）→ 选区截图 OCR
            return Action::TriggerOcr;
        }
        if c.is_ascii_uppercase() {
            // 大写 ASCII 直接入提示词（保 `//translate Hello` 这类英文提示词）
            self.prompt.push(c);
            return Action::UpdatePrompt {
                preedit: self.preedit(),
            };
        }
        if c.is_ascii_alphabetic() {
            self.pinyin_buffer.clear();
            self.pinyin_buffer.push(c.to_ascii_lowercase());
            self.refresh_candidates();
            return self.pinyin_action();
        }
        self.prompt.push(c);
        Action::UpdatePrompt {
            preedit: self.preedit(),
        }
    }

    /// 退格。
    pub fn feed_backspace(&mut self) -> Action {
        match self.state {
            MachineState::Idle => Action::None,
            MachineState::PendingSlash => {
                self.state = MachineState::Idle;
                Action::Cancel
            }
            MachineState::Pinyin => {
                if !self.committed.is_empty() {
                    // 弹回上一已承诺段（不删活跃拼音字符）。
                    if let Some((_, consumed)) = self.committed.pop() {
                        self.commit_offset = self.commit_offset.saturating_sub(consumed);
                    }
                    self.refresh_candidates();
                    if self.pinyin_buffer.is_empty()
                        || self.commit_offset >= self.pinyin_buffer.len()
                    {
                        self.reset_pinyin();
                        Action::Cancel
                    } else {
                        self.pinyin_action()
                    }
                } else if self.pinyin_buffer.pop().is_some() {
                    self.refresh_candidates();
                    if self.pinyin_buffer.is_empty() {
                        self.reset_pinyin();
                        Action::Cancel
                    } else {
                        self.pinyin_action()
                    }
                } else {
                    Action::None
                }
            }
            MachineState::Prompt => {
                if self.pinyin_composing() {
                    if self.pinyin_buffer.pop().is_some() {
                        self.refresh_candidates();
                        if self.pinyin_buffer.is_empty() {
                            self.clear_pinyin();
                        }
                        Action::UpdatePrompt {
                            preedit: self.preedit(),
                        }
                    } else {
                        Action::None
                    }
                } else if self.prompt.pop().is_some() {
                    Action::UpdatePrompt {
                        preedit: self.preedit(),
                    }
                } else {
                    self.state = MachineState::Idle;
                    Action::Cancel
                }
            }
            MachineState::Streaming | MachineState::ResultReady => Action::None,
        }
    }

    /// Enter。
    pub fn feed_enter(&mut self) -> Action {
        match self.state {
            MachineState::Idle | MachineState::PendingSlash => Action::None,
            MachineState::Pinyin => {
                // 回车：上屏原始输入（IME 惯例「英文通道」）。此前误取首选
                // 候选——用户打英文串（如 soft）会被 Rime 切出的中文候选顶替
                // 上屏。选中文请用空格/数字键。
                let text = format!("{}{}", self.committed_text(), self.active_pinyin());
                self.reset_pinyin();
                Action::CommitImmediate(text)
            }
            MachineState::Prompt => {
                if self.pinyin_composing() {
                    // 组合中：回车提交原始字母到提示词（与主组合「回车=英文」
                    // 一致；不触发 LLM）
                    let text = format!("{}{}", self.committed_text(), self.active_pinyin());
                    self.prompt.push_str(&text);
                    self.clear_pinyin();
                    Action::UpdatePrompt {
                        preedit: self.preedit(),
                    }
                } else {
                    let prompt = std::mem::take(&mut self.prompt);
                    self.state = MachineState::Streaming;
                    self.result.clear();
                    Action::StartLlm {
                        prompt,
                        system: None,
                    }
                }
            }
            MachineState::Streaming | MachineState::ResultReady => {
                let text = std::mem::take(&mut self.result);
                self.state = MachineState::Idle;
                Action::CommitResult { text }
            }
        }
    }

    /// Esc。
    pub fn feed_escape(&mut self) -> Action {
        match self.state {
            MachineState::Idle => Action::None,
            _ => {
                self.state = MachineState::Idle;
                self.pinyin_buffer.clear();
                self.dictionary_candidates.clear();
                self.llm_candidates.clear();
                self.pinyin_candidates.clear();
                self.committed.clear();
                self.commit_offset = 0;
                self.last_candidates_request = None;
                self.candidates_in_flight = false;
                self.deferred_intent = None;
                self.prompt.clear();
                self.result.clear();
                Action::Cancel
            }
        }
    }

    /// 尚未被分段承诺消费的活跃缓冲（全缓冲的剩余尾部）。
    fn active_pinyin(&self) -> &str {
        &self.pinyin_buffer[self.commit_offset.min(self.pinyin_buffer.len())..]
    }

    /// 已承诺段的拼接文本。
    fn committed_text(&self) -> String {
        self.committed.iter().map(|(t, _)| t.as_str()).collect()
    }

    /// 展示候选文本列表（供 Action::UpdatePinyin / 候选窗 / 提示词态）。
    fn pinyin_candidate_texts(&self) -> Vec<String> {
        self.pinyin_candidates
            .iter()
            .map(|c| c.text.clone())
            .collect()
    }

    /// 构建拼音态 UpdatePinyin 动作（preedit/候选/页码/选中/LLM 请求）。
    fn pinyin_action(&mut self) -> Action {
        Action::UpdatePinyin {
            preedit: self.pinyin_composition_preedit(),
            candidates: self.display_candidate_texts(),
            page: self.pinyin_page,
            selected: self.selected_index,
            llm_request: self.request_llm_candidates_if_needed(),
        }
    }

    /// 当前页内候选条数（最后一页可能不满页）。
    fn page_item_count(&self) -> usize {
        let total = self.pinyin_candidates.len();
        if total == 0 {
            return 0;
        }
        let start = self.pinyin_page * Self::PINYIN_PAGE_SIZE;
        (total - start).min(Self::PINYIN_PAGE_SIZE)
    }

    /// 候选总页数（0 候选 = 0 页）。
    fn total_pages(&self) -> usize {
        self.pinyin_candidates
            .len()
            .div_ceil(Self::PINYIN_PAGE_SIZE)
    }

    /// 方向键选字：Up 上移选中；页首继续 Up 翻到上一页末项。
    /// 候选为空时不动（返回原态刷新，前端保持当前显示）。
    pub fn feed_arrow_up(&mut self) -> Action {
        if self.state == MachineState::Pinyin {
            let n = self.page_item_count();
            if n == 0 {
                return Action::None;
            }
            if self.selected_index > 0 {
                self.selected_index -= 1;
            } else if self.pinyin_page > 0 {
                // 页首继续 Up：翻上一页并选中末项（跨页遍历）。
                self.pinyin_page -= 1;
                self.selected_index = self.page_item_count().saturating_sub(1);
            } else {
                return Action::None; // 首页首项
            }
            return self.pinyin_action();
        }
        Action::None
    }

    /// 方向键选字：Down 下移选中；页尾继续 Down 翻到下一页首项。
    pub fn feed_arrow_down(&mut self) -> Action {
        if self.state == MachineState::Pinyin {
            let n = self.page_item_count();
            if n == 0 {
                return Action::None;
            }
            if self.selected_index + 1 < n {
                self.selected_index += 1;
            } else if self.pinyin_page + 1 < self.total_pages() {
                // 页尾继续 Down：翻到下一页首项（跨页遍历，微软拼音/手心行为）。
                self.pinyin_page += 1;
                self.selected_index = 0;
            } else {
                return Action::None; // 末页末项
            }
            return self.pinyin_action();
        }
        Action::None
    }

    /// 取融合候选中的第 `index` 个（含覆盖长度）。
    fn fused_segment(&self, index: usize) -> Option<&SegCandidate> {
        self.pinyin_candidates.get(index)
    }

    /// 选择第 `index` 个候选：整句提交或分段承诺。
    ///
    /// - 候选覆盖全部剩余拼音（或无候选）→ 整句提交并回 Idle。
    /// - 候选只覆盖一个前缀段 → 将候选压入 `committed`，剩余拼音继续组合；
    ///   若已消费完整个缓冲则自动提交。
    fn select_candidate(&mut self, index: usize) -> Action {
        let active_len = self.active_pinyin().len();
        let (text, consumed) = match self.fused_segment(index) {
            Some(seg) => (seg.text.clone(), seg.consumed.min(active_len)),
            None if self.candidates_in_flight => {
                // 候选在途（Rime 查询未回）：首个空格暂缓选择，结果到达后补执行。
                // 真机竞态：worker 已拿到候选、事件尚未经主线程 drain 到达状态机，
                // 此刻空格若走原文回退会把拼音字母上屏（快速输入偶发漏字母）。
                //
                // 暂缓后的**重复**空格 = 知情回退：结果迟迟未达（守护重启、查询
                // 被更新的请求取代、连接失效等极端场景下可能永不回达），再按一次
                // 空格说明用户明确要上屏——按原文提交并清掉暂缓，避免无限吞键。
                // 与「终结空结果展示原文条目后，再按空格是知情选择」同一语义通道。
                if self.deferred_intent != Some(DeferredIntent::SelectSpace) {
                    self.deferred_intent = Some(DeferredIntent::SelectSpace);
                    return Action::None;
                }
                self.deferred_intent = None;
                let text = format!("{}{}", self.committed_text(), self.active_pinyin());
                self.reset_pinyin();
                return Action::CommitImmediate(text);
            }
            None => (self.active_pinyin().to_owned(), active_len),
        };
        if consumed >= active_len {
            // 覆盖全部剩余：整句提交
            let full = format!("{}{}", self.committed_text(), text);
            self.reset_pinyin();
            Action::CommitImmediate(full)
        } else {
            // 分段承诺：保留已选段，剩余拼音继续组合
            self.committed.push((text, consumed));
            self.commit_offset += consumed;
            if self.commit_offset >= self.pinyin_buffer.len() {
                // 已消费完整个缓冲：自动提交
                let full = self.committed_text();
                self.reset_pinyin();
                Action::CommitImmediate(full)
            } else {
                self.refresh_candidates();
                self.pinyin_action()
            }
        }
    }

    /// 是否正在拼音组合（缓冲非空）。
    fn pinyin_composing(&self) -> bool {
        !self.pinyin_buffer.is_empty()
    }

    /// 清空拼音缓冲与候选（保留提示词）。
    fn clear_pinyin(&mut self) {
        self.pinyin_buffer.clear();
        self.dictionary_candidates.clear();
        self.llm_candidates.clear();
        self.pinyin_candidates.clear();
        self.committed.clear();
        self.commit_offset = 0;
        self.last_candidates_request = None;
        self.candidates_in_flight = false;
        self.deferred_intent = None;
        self.pinyin_page = 0;
        self.selected_index = 0;
        self.ocr_preview = None;
    }

    /// 拼音组合区的 preedit（`buffer 1.候选 2.候选…`），无候选时仅缓冲。
    fn pinyin_preedit(&self) -> String {
        let base = format!("{}{}", self.committed_text(), self.active_pinyin());
        if self.pinyin_candidates.is_empty() {
            base
        } else {
            let mut out = base;
            for (i, cand) in self.pinyin_candidates.iter().enumerate() {
                out.push_str(&format!(" {}.{}", i + 1, cand.text));
            }
            out
        }
    }

    /// 纯拼音组合 preedit（不含内联候选；候选窗接管显示时使用）。
    /// 提示词态带 `//` 与已提交提示词前缀。
    pub fn pinyin_composition_preedit(&self) -> String {
        match self.state {
            MachineState::Pinyin => format!("{}{}", self.committed_text(), self.active_pinyin()),
            MachineState::Prompt => format!("//{}{}", self.prompt, self.active_pinyin()),
            _ => String::new(),
        }
    }

    /// 当前拼音提交文本：有候选取候选 0，否则取原始缓冲。
    fn commit_pinyin_text(&self) -> String {
        let committed = self.committed_text();
        if let Some(first) = self.pinyin_candidates.first() {
            format!("{committed}{}", first.text)
        } else {
            format!("{committed}{}", self.active_pinyin())
        }
    }

    /// 重置拼音状态回 Idle。
    fn reset_pinyin(&mut self) {
        self.state = MachineState::Idle;
        self.pinyin_buffer.clear();
        self.dictionary_candidates.clear();
        self.llm_candidates.clear();
        self.pinyin_candidates.clear();
        self.committed.clear();
        self.commit_offset = 0;
        self.last_candidates_request = None;
        self.candidates_in_flight = false;
        self.deferred_intent = None;
        self.pinyin_page = 0;
        self.selected_index = 0;
        self.ocr_preview = None;
    }

    /// 用当前缓冲刷新候选（缓冲变化时回到第 1 页，并丢弃旧远程候选）。
    ///
    /// 单引擎（Rime）：候选只来自 `on_llm_candidates`（daemon 一次性推送），
    /// 不再使用内置 `verba-pinyin` 生成词库候选。
    fn refresh_candidates(&mut self) {
        self.pinyin_page = 0;
        self.dictionary_candidates.clear();
        self.llm_candidates.clear();
        self.fuse_candidates();
    }

    /// 融合可选候选 = 词库候选 ++ LLM 候选（LLM 侧已去重）。**只含真实
    /// 结果**：合成的原文条目仅用于面板展示（display_candidate_texts），
    /// 绝不进入本列表——否则在途空格会直接"选中"它提交原文，击穿防漏暂缓。
    fn fuse_candidates(&mut self) {
        let mut out = self.dictionary_candidates.clone();
        for cand in &self.llm_candidates {
            if !out.iter().any(|c| c.text == cand.text) {
                out.push(cand.clone());
            }
        }
        self.pinyin_candidates = out;
    }

    /// 面板展示候选：真实候选；本拼音查询已终结且确认为空时补一条当前
    /// 字母串的「原文条目」（英文原文本身即候选）。仅在途不补——逐键闪现
    /// 拼音字母、与随后到达的中文候选来回切换会让面板持续抖动（用户反馈）；
    /// 在途空窗由前端「空数据保持原内容」策略自然盖住。仅展示不参与选择。
    fn display_candidate_texts(&self) -> Vec<String> {
        let mut v = self.pinyin_candidate_texts();
        if v.is_empty() && self.pinyin_composing() {
            let active = self.active_pinyin();
            let settled = !self.candidates_in_flight
                && self
                    .last_candidates_request
                    .as_ref()
                    .is_some_and(|py| py == active);
            if settled {
                v.push(active.to_string());
            }
        }
        v
    }

    /// 拼音变更后是否需要发起 LLM 候选请求（同一拼音只请求一次）。
    fn request_llm_candidates_if_needed(&mut self) -> Option<LlmCandidateRequest> {
        let active = self.active_pinyin().to_owned();
        if !self.pinyin_composing() || active.is_empty() {
            self.last_candidates_request = None;
            return None;
        }
        if self.last_candidates_request.as_deref() == Some(active.as_str()) {
            return None;
        }
        self.last_candidates_request = Some(active.clone());
        self.candidates_in_flight = true;
        Some(LlmCandidateRequest {
            pinyin: active,
            dictionary: self
                .dictionary_candidates
                .iter()
                .map(|c| c.text.clone())
                .collect(),
        })
    }

    /// OCR 结果进入预览：候选窗首条 = 识别文本（选中态），非流式通道。
    /// 不覆盖已有组合（OCR 触发时组合已结束，状态应 Idle）。
    pub fn begin_ocr_preview(&mut self, text: String) -> Option<Action> {
        if self.state != MachineState::Idle || text.is_empty() {
            return None;
        }
        self.ocr_preview = Some(text.clone());
        Some(Action::OcrPreview { text })
    }

    /// OCR 预览态的按键处理：Enter/空格/数字 1 上屏识别文本；
    /// Esc 取消；其余键退出预览并照常处理（不打断打字流）。
    pub fn feed_ocr_preview(&mut self, key: PreviewKey) -> Option<Action> {
        debug_assert!(self.ocr_preview.is_some(), "preview 状态须有文本");
        let text = self.ocr_preview.clone().unwrap_or_default();
        match key {
            PreviewKey::Enter | PreviewKey::Space | PreviewKey::Digit1 => {
                self.ocr_preview = None;
                Some(Action::CommitImmediate(text))
            }
            PreviewKey::Escape => {
                self.ocr_preview = None;
                Some(Action::Cancel)
            }
            PreviewKey::Other => {
                // 其他键：退出预览（丢弃），该键交回调用方重走正常路径。
                self.ocr_preview = None;
                None
            }
        }
    }

    pub fn ocr_previewing(&self) -> bool {
        self.ocr_preview.is_some()
    }

    /// 退出预览（丢弃识别文本；其他键照常处理时调用）。
    pub fn end_ocr_preview(&mut self) {
        self.ocr_preview = None;
    }

    /// LLM 流式增量。
    pub fn on_llm_chunk(&mut self, chunk: &str) -> Action {
        match self.state {
            MachineState::Streaming => {
                self.result.push_str(chunk);
                Action::UpdateResult {
                    preedit: self.result.clone(),
                }
            }
            _ => Action::None,
        }
    }

    /// LLM 输出完成。
    pub fn on_llm_done(&mut self) -> Action {
        match self.state {
            MachineState::Streaming => {
                self.state = MachineState::ResultReady;
                Action::ResultReady
            }
            _ => Action::None,
        }
    }

    /// LLM 出错。
    pub fn on_llm_error(&mut self, message: &str) -> Action {
        let was_active = matches!(self.state, MachineState::Streaming);
        self.state = MachineState::Idle;
        self.prompt.clear();
        self.result.clear();
        if was_active {
            Action::LlmFailed {
                message: message.to_owned(),
            }
        } else {
            Action::None
        }
    }

    /// LLM 候选融合增量：追加候选（去重），返回更新后的候选列表。
    /// `pinyin` 与当前组合不符时视为过期结果直接忽略。
    pub fn on_llm_candidates(&mut self, pinyin: &str, candidates: &[String], done: bool) -> Action {
        // 单引擎（Rime）：在主组合（Pinyin）与提示词内拼音（Prompt 组合中）都接受 Rime 候选。
        if !self.pinyin_composing() || self.active_pinyin() != pinyin {
            return Action::None;
        }
        if done {
            // 本拼音查询终结（即使空结果）：释放在途标记，避免后续选择被无限暂缓。
            self.candidates_in_flight = false;
        }
        let active_len = self.active_pinyin().len();
        // 融合前公开列表快照：判定终结结果是否真的改变了展示内容
        let llm_before_texts = self.display_candidate_texts();
        let mut changed = false;
        for cand in candidates {
            let cand = cand.trim();
            if cand.is_empty() {
                continue;
            }
            if self.llm_candidates.iter().any(|c| c.text == cand)
                || self.dictionary_candidates.iter().any(|c| c.text == cand)
            {
                continue;
            }
            self.llm_candidates.push(SegCandidate {
                text: cand.to_owned(),
                consumed: active_len,
            });
            changed = true;
        }
        // done 必触发重融合：真实候选为空的终结结果会在此合成原文候选
        // （见 fuse_candidates）。
        if done || changed {
            self.fuse_candidates();
            // 候选列表变化后选中回到首项（方向键选中的位置失效）。
            self.selected_index = 0;
        }
        // 查询终结（done）时补执行在途暂缓的意图。部分块（done=false，legacy
        // 流式通道保留的入口）只累积候选、不触发重放——提前重放会以「原文+
        // 后缀」直出，击穿防漏暂缓；当前唯一在用通道 rime_candidates 单事件
        // 即终结（daemon handler），此处门控是对未来接入方的语义护栏。
        //
        // 空格的零结果分支不自动提交——按空格那一刻用户还没见过面板（快打
        // 场景），盲提原文即漏字（真机：「kjf是埃迪卡拉纪」的前半段）。吞掉
        // 该键、组合保留；原文条目此刻已在面板上，再按一次空格才是知情选择。
        //
        // 大写/标点不同：按下的是明确的「收尾字符」，意图是「文本＋后缀」
        // 完整单元。settle 时刻合成项已可见，无论有无真实候选都执行提交：
        // 有则选中首候选再接后缀，无则原文接后缀（全角映射/引号交替照常，
        // 且恰好只翻转一次）。若查询永不回达则无 settle、无重放——该路径由
        // 「重复按键 = 最新意图覆盖」或 Esc 复位解除，不会无限挂起。
        if done {
            if let Some(intent) = self.deferred_intent.take() {
                let has_real =
                    !self.dictionary_candidates.is_empty() || !self.llm_candidates.is_empty();
                match intent {
                    DeferredIntent::SelectSpace => {
                        if has_real {
                            return self.select_candidate(0);
                        }
                        self.refresh_candidates();
                        return Action::UpdatePinyin {
                            preedit: self.pinyin_composition_preedit(),
                            candidates: self.display_candidate_texts(),
                            page: self.pinyin_page,
                            selected: self.selected_index,
                            llm_request: None,
                        };
                    }
                    DeferredIntent::Uppercase(ch) => {
                        let text = format!("{}{ch}", self.commit_pinyin_text());
                        self.reset_pinyin();
                        return Action::CommitImmediate(text);
                    }
                    DeferredIntent::Punct(ch) => {
                        let punct = self.punct_commit_text(ch);
                        let text = format!("{}{punct}", self.commit_pinyin_text());
                        self.reset_pinyin();
                        return Action::CommitImmediate(text);
                    }
                }
            }
        }
        // 仅当公开候选列表实际变化时发刷新（done 但列表未变的重复注入
        // 不重复推送），合成项的首次出现也走此通道到前端。
        if !changed && self.display_candidate_texts() == llm_before_texts {
            return Action::None;
        }
        Action::UpdatePinyin {
            preedit: self.pinyin_composition_preedit(),
            candidates: self.display_candidate_texts(),
            page: self.pinyin_page,
            selected: self.selected_index,
            llm_request: None,
        }
    }
}

impl fmt::Display for MachineState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Idle => "idle",
            Self::Pinyin => "pinyin",
            Self::PendingSlash => "pending-slash",
            Self::Prompt => "prompt",
            Self::Streaming => "streaming",
            Self::ResultReady => "result-ready",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模拟 Rime 候选推送（单引擎）：向状态机注入候选。
    fn rime(m: &mut CompositionMachine, py: &str, texts: &[&str]) {
        let _ = m.on_llm_candidates(
            py,
            &texts.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            true,
        );
    }

    /// 多候选（>9）用于分页测试。
    const MANY: [&str; 12] = [
        "c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10", "c11",
    ];

    #[test]
    fn idle_punct_commits_fullwidth() {
        let mut m = CompositionMachine::new();
        for (ascii, wide) in [(',', '，'), ('.', '。'), ('?', '？'), ('\\', '、')] {
            let a = m.feed_char(ascii);
            assert!(
                matches!(&a, Action::CommitImmediate(t) if *t == wide.to_string()),
                "{ascii} 应上屏全角 {wide}，实际 {a:?}"
            );
        }
        // 未入表符号保持半角
        assert!(
            matches!(m.feed_char('@'), Action::CommitImmediate(t) if t == "@"),
            "@ 应保持半角直通"
        );
    }

    #[test]
    fn punct_after_composition_flushes_with_fullwidth() {
        let mut m = CompositionMachine::new();
        let _ = m.feed_char('n');
        rime(&mut m, "n", &["你"]);
        let a = m.feed_char(',');
        assert!(
            matches!(&a, Action::CommitImmediate(t) if t == "你，"),
            "组合中标点应提交「候选+全角标点」，实际 {a:?}"
        );
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn paired_quotes_alternate() {
        let mut m = CompositionMachine::new();
        assert!(matches!(m.feed_char('"'), Action::CommitImmediate(t) if t == "“"));
        assert!(matches!(m.feed_char('"'), Action::CommitImmediate(t) if t == "”"));
        assert!(matches!(m.feed_char('\''), Action::CommitImmediate(t) if t == "‘"));
        assert!(matches!(m.feed_char('\''), Action::CommitImmediate(t) if t == "’"));
    }

    #[test]
    fn double_and_single_quotes_pair_independently() {
        // 回归：双/单引号曾共用交替标志，`"` 后紧跟 `'` 会产出「“’」错配对。
        let mut m = CompositionMachine::new();
        assert!(matches!(m.feed_char('"'), Action::CommitImmediate(t) if t == "“"));
        assert!(
            matches!(m.feed_char('\''), Action::CommitImmediate(t) if t == "‘"),
            "单引号应独立从开引号开始"
        );
        assert!(matches!(m.feed_char('"'), Action::CommitImmediate(t) if t == "”"));
        assert!(matches!(m.feed_char('\''), Action::CommitImmediate(t) if t == "’"));
    }

    #[test]
    fn composing_flush_with_quote_keeps_pair_state() {
        let mut m = CompositionMachine::new();
        let _ = m.feed_char('n');
        rime(&mut m, "n", &["你"]);
        assert!(
            matches!(m.feed_char('"'), Action::CommitImmediate(t) if t == "你“"),
            "组合后引号按开引号输出"
        );
        assert!(
            matches!(m.feed_char('"'), Action::CommitImmediate(t) if t == "”"),
            "下一个引号交替为闭"
        );
    }

    #[test]
    fn zero_real_candidates_synth_raw_entry_and_space_commits_it() {
        let mut m = CompositionMachine::new();
        let _ = m.feed_char('x');
        let _ = m.feed_char('q');
        rime(&mut m, "xq", &[]);
        // 真实候选为空：展示层补合成原文条目（英文原文本身即候选）
        assert_eq!(m.display_candidate_texts(), ["xq"]);
        // 空格选中它 = 显式选择原文
        assert!(matches!(m.feed_char(' '), Action::CommitImmediate(ref t) if t == "xq"));
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn synthetic_entry_only_after_settled_empty() {
        let mut m = CompositionMachine::new();
        let a = m.feed_char('x'); // 查询在途
                                  // 在途不合成：逐键闪现拼音字母会让面板持续抖动（用户反馈）
        assert!(
            matches!(&a, Action::UpdatePinyin { candidates, .. } if candidates.is_empty()),
            "在途应为空展示，实际 {a:?}"
        );
        rime(&mut m, "x", &[]); // 本拼音查询终结且为空
        assert_eq!(m.display_candidate_texts(), ["x"], "终结空后应补原文条目");
    }

    #[test]
    fn pinyin_buffer_cap_stops_appending() {
        let mut m = CompositionMachine::new();
        for _ in 0..CompositionMachine::MAX_PINYIN_BUFFER + 5 {
            let _ = m.feed_char('a');
        }
        assert_eq!(
            m.pinyin_buffer.len(),
            CompositionMachine::MAX_PINYIN_BUFFER,
            "到顶后字母不再入缓冲"
        );
        assert_eq!(m.state(), MachineState::Pinyin);
    }

    #[test]
    fn letters_start_pinyin_composition() {
        let mut m = CompositionMachine::new();
        let a = m.feed_char('h');
        assert!(
            matches!(a, Action::UpdatePinyin { .. }),
            "字母应进入拼音组合，实际 {a:?}"
        );
        assert_eq!(m.state(), MachineState::Pinyin);
        assert!(
            m.preedit().starts_with('h'),
            "preedit 应显示拼音，实际 {:?}",
            m.preedit()
        );
    }

    #[test]
    fn punctuation_and_digits_commit_directly_in_idle() {
        let mut m = CompositionMachine::new();
        // 句号转全角（中文态标点惯例）；数字保持半角直通
        assert_eq!(m.feed_char('.'), Action::CommitImmediate("。".into()));
        assert_eq!(m.feed_char('5'), Action::CommitImmediate("5".into()));
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn pinyin_space_commits_first_candidate() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        rime(&mut m, "ni", &["你", "你好"]);
        assert_eq!(m.state(), MachineState::Pinyin);
        let first = m.commit_pinyin_text();
        assert_eq!(first, "你", "ni 首选应为 你，实际 {first:?}");
        let a = m.feed_char(' ');
        assert_eq!(a, Action::CommitImmediate("你".into()));
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn uppercase_in_idle_commits_directly() {
        let mut m = CompositionMachine::new();
        assert_eq!(m.feed_char('A'), Action::CommitImmediate("A".into()));
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn uppercase_in_pinyin_commits_candidate_plus_char() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        rime(&mut m, "ni", &["你"]);
        assert_eq!(m.state(), MachineState::Pinyin);
        // 候选 0 为「你」：大写 A 提交「你A」并回到 Idle
        let a = m.feed_char('A');
        assert_eq!(a, Action::CommitImmediate("你A".into()));
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn uppercase_in_prompt_not_composing_appends() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        // Prompt 未组合：大写直接入提示词（保英文提示词）
        let a = m.feed_char('H');
        assert!(matches!(a, Action::UpdatePrompt { preedit } if preedit == "//H"));
        assert_eq!(m.prompt(), "H");
    }

    #[test]
    fn uppercase_in_prompt_composing_commits_candidate_plus_char() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        m.feed_char('n');
        m.feed_char('i');
        rime(&mut m, "ni", &["你"]);
        // 提示词内拼音组合中：大写提交候选「你」+ H 到提示词
        let a = m.feed_char('H');
        assert!(matches!(a, Action::UpdatePrompt { preedit } if preedit == "//你H"));
        assert_eq!(m.prompt(), "你H");
    }

    #[test]
    fn pinyin_digit_selects_candidate() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        rime(&mut m, "ni", &["你", "你好", "你好吗", "你来"]);
        // 候选索引 1（第二个）
        if m.pinyin_candidates.len() > 1 {
            let expected = m.pinyin_candidates[1].text.clone();
            let a = m.feed_char('2');
            assert_eq!(a, Action::CommitImmediate(expected));
            assert_eq!(m.state(), MachineState::Idle);
        }
    }

    #[test]
    fn pinyin_digit_out_of_range_ignored() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        // 注入少于 1 页（9 个）的候选：按 9 超出候选 → 忽略（不吞字、不提交原文）。
        rime(&mut m, "n", &["你", "你好"]);
        assert!(m.pinyin_candidates.len() < CompositionMachine::PINYIN_PAGE_SIZE);
        assert_eq!(m.feed_char('9'), Action::None);
        assert_eq!(m.state(), MachineState::Pinyin);
    }

    #[test]
    fn pinyin_engine_returns_more_than_one_page() {
        // 候选数须多于 1 页，否则分页无意义。
        let mut m = CompositionMachine::new();
        let _ = m.feed_char('n');
        rime(&mut m, "n", &MANY);
        assert!(
            m.pinyin_candidates.len() > CompositionMachine::PINYIN_PAGE_SIZE,
            "候选应多于一页（{} > {}），实际 {} 个",
            m.pinyin_candidates.len(),
            CompositionMachine::PINYIN_PAGE_SIZE,
            m.pinyin_candidates.len()
        );
    }

    #[test]
    fn pinyin_page_down_advances_and_wraps() {
        let mut m = CompositionMachine::new();
        let _ = m.feed_char('n');
        rime(&mut m, "n", &MANY);
        let total = m
            .pinyin_candidates
            .len()
            .div_ceil(CompositionMachine::PINYIN_PAGE_SIZE);
        assert!(total >= 2, "需要至少 2 页才有回绕可测，实际 {total}");
        // 翻到最后一页
        for _ in 0..(total - 1) {
            assert!(matches!(m.feed_page_down(), Action::UpdatePinyin { .. }));
        }
        // 末页再下翻 → 回绕到第 1 页
        assert!(matches!(
            m.feed_page_down(),
            Action::UpdatePinyin { page: 0, .. }
        ));
        // 第 1 页上翻 → 回绕到最后一页
        let page = match m.feed_page_up() {
            Action::UpdatePinyin { page, .. } => page,
            other => panic!("应翻页，实际 {other:?}"),
        };
        assert_eq!(page, total - 1);
    }

    #[test]
    fn pinyin_digit_selects_page_relative() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        rime(&mut m, "n", &MANY);
        // 翻到第 2 页
        let full = match m.feed_page_down() {
            Action::UpdatePinyin {
                candidates, page, ..
            } => {
                assert_eq!(page, 1);
                candidates
            }
            other => panic!("应翻到第 2 页，实际 {other:?}"),
        };
        let expected = full[CompositionMachine::PINYIN_PAGE_SIZE].clone();
        // 第 2 页按 1 → 应选中全列表第 PAGE_SIZE 个（页码偏移）
        let a = m.feed_char('1');
        assert_eq!(a, Action::CommitImmediate(expected));
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn pinyin_oem_paging_keys() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        rime(&mut m, "n", &MANY);
        assert!(matches!(
            m.feed_char('='),
            Action::UpdatePinyin { page: 1, .. }
        ));
        assert!(matches!(
            m.feed_char('-'),
            Action::UpdatePinyin { page: 0, .. }
        ));
    }

    #[test]
    fn paging_noop_when_single_page() {
        let mut m = CompositionMachine::new();
        for c in "nihao".chars() {
            m.feed_char(c);
        }
        assert_eq!(m.feed_page_down(), Action::None);
        assert_eq!(m.feed_page_up(), Action::None);
    }

    #[test]
    fn typing_resets_page() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_page_down();
        assert!(matches!(
            m.feed_char('i'),
            Action::UpdatePinyin { page: 0, .. }
        ));
    }

    #[test]
    fn pinyin_full_word_commits() {
        let mut m = CompositionMachine::new();
        for c in "nihao".chars() {
            m.feed_char(c);
        }
        rime(&mut m, "nihao", &["你好", "你好吗"]);
        assert_eq!(m.commit_pinyin_text(), "你好");
        let a = m.feed_char(' ');
        assert_eq!(a, Action::CommitImmediate("你好".into()));
    }

    #[test]
    fn pinyin_backspace_pops_and_cancels_when_empty() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        assert!(matches!(m.feed_backspace(), Action::UpdatePinyin { .. }));
        assert_eq!(m.state(), MachineState::Pinyin);
        assert_eq!(m.feed_backspace(), Action::Cancel);
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn pinyin_slash_commits_then_ai_trigger() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        rime(&mut m, "ni", &["你"]);
        assert_eq!(m.feed_char('/'), Action::CommitImmediate("你".into()));
        assert_eq!(m.state(), MachineState::PendingSlash);
        m.feed_char('/');
        assert_eq!(m.state(), MachineState::Prompt);
    }

    #[test]
    fn pinyin_enter_commits_raw_and_escape_cancels() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        rime(&mut m, "ni", &["你"]);
        // 回车 = 英文通道：上屏原始输入，绝不取候选（候选用空格/数字选）
        assert_eq!(m.feed_enter(), Action::CommitImmediate("ni".into()));
        assert_eq!(m.state(), MachineState::Idle);

        let mut m2 = CompositionMachine::new();
        m2.feed_char('n');
        m2.feed_char('i');
        rime(&mut m2, "ni", &["你"]);
        assert_eq!(m2.feed_escape(), Action::Cancel);
        assert_eq!(m2.state(), MachineState::Idle);
    }

    #[test]
    fn pinyin_punctuation_commits_candidate_plus_char() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        // 有真实结果（非盲窗）：候选优先 + 全角标点
        rime(&mut m, "ni", &["你"]);
        let a = m.feed_char(',');
        assert!(
            matches!(&a, Action::CommitImmediate(t) if t == "你，"),
            "标点应提交候选+标点，实际 {a:?}"
        );
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn pinyin_punctuation_blind_window_defers_then_settles() {
        // 盲窗（在途且零已知结果）：标点不立即按原文提交，暂缓到 settle 重放。
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        let a = m.feed_char(',');
        assert!(
            matches!(a, Action::UpdatePinyin { .. }),
            "盲窗应暂缓，实际 {a:?}"
        );
        assert_eq!(m.state(), MachineState::Pinyin);
        // settle 空结果：原文 + 全角后缀（此刻合成项已可见，知情提交）
        let a = m.on_llm_candidates("ni", &[], true);
        assert!(
            matches!(a, Action::CommitImmediate(ref t) if t == "ni，"),
            "settle 应重放为原文+全角，实际 {a:?}"
        );
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn double_slash_enters_prompt_mode() {
        let mut m = CompositionMachine::new();
        assert_eq!(
            m.feed_char('/'),
            Action::UpdatePrompt {
                preedit: "/".into()
            }
        );
        assert_eq!(m.state(), MachineState::PendingSlash);
        assert_eq!(
            m.feed_char('/'),
            Action::EnterPrompt {
                preedit: "//".into()
            }
        );
        assert_eq!(m.state(), MachineState::Prompt);
    }

    #[test]
    fn slash_followed_by_char_commits_both() {
        let mut m = CompositionMachine::new();
        assert_eq!(
            m.feed_char('/'),
            Action::UpdatePrompt {
                preedit: "/".into()
            }
        );
        assert_eq!(m.feed_char('x'), Action::CommitImmediate("/x".into()));
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn backspace_in_pending_slash_cancels() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        assert_eq!(m.feed_backspace(), Action::Cancel);
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn prompt_enter_starts_llm() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        m.feed_char('翻');
        m.feed_char('译');
        assert_eq!(
            m.feed_enter(),
            Action::StartLlm {
                prompt: "翻译".into(),
                system: None
            }
        );
        assert_eq!(m.state(), MachineState::Streaming);
    }

    #[test]
    fn streaming_then_enter_commits_result() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        m.feed_enter();
        assert_eq!(
            m.on_llm_chunk("你"),
            Action::UpdateResult {
                preedit: "你".into()
            }
        );
        assert_eq!(
            m.on_llm_chunk("好"),
            Action::UpdateResult {
                preedit: "你好".into()
            }
        );
        assert_eq!(m.on_llm_done(), Action::ResultReady);
        assert_eq!(
            m.feed_enter(),
            Action::CommitResult {
                text: "你好".into()
            }
        );
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn escape_cancels_at_any_stage() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        assert_eq!(m.feed_escape(), Action::Cancel);
        assert_eq!(m.state(), MachineState::Idle);

        m.feed_char('/');
        m.feed_char('/');
        m.feed_enter();
        m.on_llm_chunk("x");
        assert_eq!(m.feed_escape(), Action::Cancel);
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn llm_error_returns_to_idle() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        m.feed_enter();
        assert_eq!(
            m.on_llm_error("网络错误"),
            Action::LlmFailed {
                message: "网络错误".into()
            }
        );
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn preedit_matches_state() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        m.feed_char('翻');
        assert_eq!(m.preedit(), "//翻");
        m.feed_enter();
        assert_eq!(m.preedit(), "");
        m.on_llm_chunk("Hello");
        assert_eq!(m.preedit(), "Hello");
    }

    #[test]
    fn mode_roundtrip_via_str() {
        for mode in [Mode::Normal, Mode::Voice, Mode::Ocr, Mode::Ai] {
            assert_eq!(Mode::from_proto_str(mode.as_str()), Some(mode));
        }
        assert_eq!(Mode::from_proto_str("bogus"), None);
        assert!(Mode::Normal.accepts_direct_input());
        assert!(!Mode::Ai.accepts_direct_input());
    }

    #[test]
    fn prompt_pinyin_commits_chinese() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        for c in "nihao".chars() {
            m.feed_char(c);
        }
        rime(&mut m, "nihao", &["你好", "你好吗"]);
        assert!(m.pinyin_composing(), "提示词中应处于拼音组合");
        assert!(
            m.preedit().contains(" 1."),
            "preedit 应含内联候选: {:?}",
            m.preedit()
        );
        let a = m.feed_char(' ');
        assert!(matches!(a, Action::UpdatePrompt { .. }));
        assert_eq!(
            m.prompt(),
            "你好",
            "提示词应提交中文，实际 {:?}",
            m.prompt()
        );
        assert!(!m.pinyin_composing());
    }

    #[test]
    fn prompt_pinyin_enter_commits_then_submits() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        for c in "nihao".chars() {
            m.feed_char(c);
        }
        rime(&mut m, "nihao", &["你好"]);
        // 第一次 Enter：上屏原始字母到提示词（与主组合「回车=英文」一致，
        // 不触发 LLM）；要放中文用空格/数字选候选
        let a1 = m.feed_enter();
        assert!(
            matches!(a1, Action::UpdatePrompt { .. }),
            "组合中 Enter 应先提交拼音原文: {a1:?}"
        );
        assert_eq!(m.prompt(), "nihao");
        // 第二次 Enter：无组合 → 提交 LLM
        assert_eq!(
            m.feed_enter(),
            Action::StartLlm {
                prompt: "nihao".into(),
                system: None
            }
        );
    }

    #[test]
    fn prompt_english_fallback_commits_raw() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        for c in "translate".chars() {
            m.feed_char(c);
        }
        // "translate" 不是合法拼音 → 无候选 → 空格提交原文
        assert!(
            !m.pinyin_composing() || m.pinyin_candidates.is_empty(),
            "非拼音应无候选"
        );
        let _ = m.feed_char(' ');
        assert_eq!(m.prompt(), "translate");
        assert_eq!(
            m.feed_enter(),
            Action::StartLlm {
                prompt: "translate".into(),
                system: None
            }
        );
    }

    #[test]
    fn prompt_backspace_pops_pinyin_then_prompt() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        m.feed_char('n');
        m.feed_char('i');
        assert!(matches!(m.feed_backspace(), Action::UpdatePrompt { .. }));
        assert_eq!(m.pinyin_buffer, "n");
        m.feed_backspace();
        assert!(!m.pinyin_composing(), "拼音清空后应退出组合");
        // 再退格弹提示词
        m.prompt.push('x');
        assert!(matches!(m.feed_backspace(), Action::UpdatePrompt { .. }));
        assert_eq!(m.prompt(), "");
    }

    // ---- 候选融合（词库候选 + LLM 候选）----

    #[test]
    fn pinyin_change_requests_llm_candidates() {
        let mut m = CompositionMachine::new();
        let a = m.feed_char('n');
        let llm_req = match a {
            Action::UpdatePinyin { llm_request, .. } => llm_request,
            other => panic!("应进入拼音，实际 {other:?}"),
        };
        let req = llm_req.expect("拼音变更后应请求 Rime 候选");
        assert_eq!(req.pinyin, "n");
        assert!(req.dictionary.is_empty(), "单引擎 Rime 词库候选应为空");
        // 同一拼音重复刷新不再发请求（last_candidates_request 去重）
        assert!(m.request_llm_candidates_if_needed().is_none());
        // 拼音变化再次请求
        let a = m.feed_char('i');
        match a {
            Action::UpdatePinyin { llm_request, .. } => {
                assert_eq!(llm_request.expect("拼音变化应再次请求").pinyin, "ni");
            }
            other => panic!("应更新拼音，实际 {other:?}"),
        }
    }

    #[test]
    fn llm_candidates_fuse_and_dedupe() {
        let mut m = CompositionMachine::new();
        let _ = m.feed_char('n');
        // 首次 Rime 候选：全部追加
        let a = m.on_llm_candidates("n", &["你".into(), "你是".into()], false);
        match a {
            Action::UpdatePinyin {
                candidates, page, ..
            } => {
                assert_eq!(page, 0);
                assert_eq!(candidates, vec!["你".to_string(), "你是".to_string()]);
            }
            other => panic!("融合应返回更新，实际 {other:?}"),
        }
        // 已存在的候选不重复，新候选追加到尾部
        let a = m.on_llm_candidates("n", &["你是".into(), "你好".into()], false);
        match a {
            Action::UpdatePinyin { candidates, .. } => {
                assert_eq!(
                    candidates,
                    vec!["你".to_string(), "你是".to_string(), "你好".to_string()]
                );
            }
            other => panic!("融合应返回更新，实际 {other:?}"),
        }
        // 无新增 → 无动作
        assert_eq!(
            m.on_llm_candidates("n", &["你是".into()], true),
            Action::None
        );
    }

    #[test]
    fn default_rime_only_candidates() {
        // 单引擎（Rime）：候选只来自 Rime；打字时无内置即时候选，Rime 到达后填充候选列表。
        let mut m = CompositionMachine::new();
        let dict = match m.feed_char('n') {
            Action::UpdatePinyin { candidates, .. } => candidates,
            other => panic!("应进入拼音，实际 {other:?}"),
        };
        // 单引擎无内置词库候选；在途也不合成（防抖动），结果到达后填充
        assert!(dict.is_empty(), "实际 {dict:?}");
        match m.on_llm_candidates("n", &["你".into(), "你是".into()], true) {
            Action::UpdatePinyin { candidates, .. } => {
                assert_eq!(candidates, vec!["你".to_string(), "你是".to_string()]);
            }
            other => panic!("Rime 融合应填充候选，实际 {other:?}"),
        }
        // 数字选择提交首候选
        assert_eq!(m.feed_char('1'), Action::CommitImmediate("你".into()));
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn stale_llm_candidates_ignored() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        // 拼音已变成 "ni" 后才到达的 "n" 结果 → 忽略
        m.feed_char('i');
        assert_eq!(
            m.on_llm_candidates("n", &["你是".into()], false),
            Action::None
        );
        // 非拼音态忽略
        m.feed_escape();
        assert_eq!(
            m.on_llm_candidates("ni", &["你是".into()], false),
            Action::None
        );
    }

    #[test]
    fn llm_candidate_selectable_by_digit() {
        let mut m = CompositionMachine::new();
        let fused = match m.feed_char('n') {
            Action::UpdatePinyin { candidates, .. } => candidates,
            other => panic!("应进入拼音，实际 {other:?}"),
        };
        let _ = m.on_llm_candidates("n", &["你是".into()], false);
        // LLM 候选追加在词库候选之后，可能不在第 1 页：翻到其所在页后按页内序号选择
        let full = m.pinyin_candidates.clone();
        let llm_idx = full
            .iter()
            .position(|c| c.text == "你是")
            .expect("LLM 候选应在融合列表");
        let page = llm_idx / CompositionMachine::PINYIN_PAGE_SIZE;
        let rel = llm_idx % CompositionMachine::PINYIN_PAGE_SIZE;
        for _ in 0..page {
            assert!(matches!(m.feed_page_down(), Action::UpdatePinyin { .. }));
        }
        assert!(full.len() > fused.len(), "融合后应更多候选");
        let key = (rel as u8 + b'1') as char;
        assert_eq!(m.feed_char(key), Action::CommitImmediate("你是".into()));
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn rime_mode_disables_dictionary_candidates() {
        let mut m = CompositionMachine::new();
        let a = m.feed_char('n');
        match a {
            Action::UpdatePinyin { candidates, .. } => {
                // 内置词库被抑制；在途不合成
                assert!(candidates.is_empty(), "实际 {candidates:?}");
            }
            other => panic!("应进入拼音，实际 {other:?}"),
        }
        // Rime 候选融合后去重、可数字选择上屏
        let _ = m.on_llm_candidates("n", &["你好".into(), "你是".into()], false);
        match m.on_llm_candidates("n", &["你".into()], true) {
            Action::UpdatePinyin { candidates, .. } => {
                assert_eq!(candidates, ["你好", "你是", "你"]);
            }
            other => panic!("应融合 Rime 候选，实际 {other:?}"),
        }
        assert_eq!(m.feed_char('1'), Action::CommitImmediate("你好".into()));
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn rime_mode_keeps_prompt_pinyin_dictionary() {
        // 单引擎 Rime：Pinyin 态与提示词拼音都经 Rime 候选。
        let mut m = CompositionMachine::new();
        // Pinyin 态：无内置候选（合成原文项占位，等 Rime）
        match m.feed_char('n') {
            Action::UpdatePinyin { candidates, .. } => assert!(candidates.is_empty()),
            other => panic!("应进入拼音，实际 {other:?}"),
        }
        m.feed_escape();
        // Prompt 态：//nihao → 注入 Rime 候选后应含「你好」
        m.feed_char('/');
        m.feed_char('/');
        for c in "nihao".chars() {
            m.feed_char(c);
        }
        rime(&mut m, "nihao", &["你好"]);
        assert!(m.pinyin_composing());
        assert!(
            m.preedit().contains("你好"),
            "Prompt 态应含 Rime 内联候选，实际 {:?}",
            m.preedit()
        );
        // 空格提交候选到提示词（而非拼音原文）
        assert!(matches!(m.feed_char(' '), Action::UpdatePrompt { .. }));
        assert_eq!(m.prompt(), "你好");
    }

    #[test]
    fn page_flip_does_not_re_request_llm() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        rime(&mut m, "n", &MANY);
        assert!(matches!(
            m.feed_page_down(),
            Action::UpdatePinyin {
                llm_request: None,
                ..
            }
        ));
    }

    #[test]
    fn pinyin_rime_candidate_commits_whole() {
        // 单引擎（Rime）：候选覆盖活跃拼音全长，选中即整句提交（无内置子短语分段）。
        let mut m = CompositionMachine::new();
        for c in "nishishui".chars() {
            m.feed_char(c);
        }
        rime(&mut m, "nishishui", &["你是谁", "你是说"]);
        assert_eq!(m.state(), MachineState::Pinyin);
        let a = m.select_candidate(0);
        assert!(
            matches!(a, Action::CommitImmediate(_)),
            "Rime 整句候选应整句提交，实际 {a:?}"
        );
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn pinyin_candidate_commits_whole() {
        // 候选覆盖全部剩余（无子短语）→ 整句提交并回 Idle（不破坏原有整句行为）。
        let mut m = CompositionMachine::new();
        for c in "ni".chars() {
            m.feed_char(c);
        }
        rime(&mut m, "ni", &["你"]);
        let a = m.select_candidate(0);
        assert!(
            matches!(a, Action::CommitImmediate(_)),
            "整句提交应上屏，实际 {a:?}"
        );
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn pinyin_backspace_pops_pinyin_char() {
        // 单引擎（Rime）：候选均为整句（consumed=active_len），无已承诺段；退格回退拼音字符。
        let mut m = CompositionMachine::new();
        for c in "nishishui".chars() {
            m.feed_char(c);
        }
        rime(&mut m, "nishishui", &["你是谁"]);
        assert_eq!(m.committed_text(), "");
        let a = m.feed_backspace();
        assert!(
            matches!(a, Action::UpdatePinyin { .. }),
            "退格应回退拼音字符，实际 {a:?}"
        );
        assert_eq!(m.committed_text(), "");
        assert_eq!(m.active_pinyin(), "nishishu");
        assert_eq!(m.state(), MachineState::Pinyin);
    }

    #[test]
    fn pinyin_rime_whole_sentence_commits() {
        // 单引擎（Rime）：注入覆盖全长的整句候选，选中即整句提交。
        let mut m = CompositionMachine::new();
        for c in "nishishui".chars() {
            m.feed_char(c);
        }
        rime(&mut m, "nishishui", &["你是谁"]);
        let a = m.select_candidate(0);
        assert!(
            matches!(&a, Action::CommitImmediate(text) if text == "你是谁"),
            "应整句提交，实际 {a:?}"
        );
        assert_eq!(m.state(), MachineState::Idle);
    }

    // ---- 快速输入竞态：候选在途时按空格 ----

    /// 空格在 Rime 候选回达前按下：不提交拼音原文，暂缓；候选到达后补执行。
    /// 真机竞态：worker 已拿到候选、主线程 drain 尚未送达状态机，此刻空格
    /// 若走原文回退会把拼音字母上屏（快速输入偶发漏字母）。
    #[test]
    fn space_while_candidates_in_flight_defers_then_selects() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        assert_eq!(m.feed_char(' '), Action::None, "候选在途时空格应暂缓");
        assert_eq!(m.state(), MachineState::Pinyin, "暂缓期间保持组合态");
        assert_eq!(
            m.on_llm_candidates("ni", &["你".into(), "拟".into()], true),
            Action::CommitImmediate("你".into()),
            "候选到达后应补执行首候选提交"
        );
        assert_eq!(m.state(), MachineState::Idle);
    }

    /// 空格暂缓后 Rime 回空结果（非法拼音）：不盲提原文（真机漏字场景——
    /// 按空格那一刻用户还没见过面板）。组合保留、合成原文条目上板；
    /// 再按一次空格才是知情选择。
    #[test]
    fn deferred_space_on_empty_result_shows_entry_without_commit() {
        let mut m = CompositionMachine::new();
        m.feed_char('q');
        m.feed_char('q');
        assert_eq!(m.feed_char(' '), Action::None);
        let a = m.on_llm_candidates("qq", &[], true);
        assert!(
            matches!(&a, Action::UpdatePinyin { candidates, .. }
                if candidates == &vec!["qq".to_string()]),
            "应上板原文条目，实际 {a:?}"
        );
        assert_eq!(m.state(), MachineState::Pinyin);
        // 用户此刻看得见了：再按空格 = 知情选择原文
        assert_eq!(m.feed_char(' '), Action::CommitImmediate("qq".into()));
    }

    /// 候选已回达（done 后）：空格立即提交首候选，无暂缓。
    #[test]
    fn space_after_candidates_done_commits_immediately() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        assert!(matches!(
            m.on_llm_candidates("n", &["你".into()], true),
            Action::UpdatePinyin { .. }
        ));
        assert_eq!(m.feed_char(' '), Action::CommitImmediate("你".into()));
    }

    /// 暂缓空格后 Esc：暂缓随之取消，不产生迟到的提交。
    #[test]
    fn escape_clears_deferred_space() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        assert_eq!(m.feed_char(' '), Action::None);
        assert!(matches!(m.feed_escape(), Action::Cancel));
        assert_eq!(
            m.on_llm_candidates("n", &["你".into()], true),
            Action::None,
            "组合已取消，迟到候选与暂缓空格都不应产生动作"
        );
    }

    /// 暂缓后的重复空格 = 知情回退：结果迟迟未达（守护重启/查询被取代/连接
    /// 失效等）时按原文提交，不无限吞键。
    #[test]
    fn second_space_while_in_flight_commits_raw() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        assert_eq!(m.feed_char(' '), Action::None, "首个在途空格应暂缓");
        assert_eq!(
            m.feed_char(' '),
            Action::CommitImmediate("ni".into()),
            "重复空格知情回退原文"
        );
        assert_eq!(m.state(), MachineState::Idle);
    }

    /// 大写盲窗暂缓：settle 有真实结果 → 首候选 + 大写字符。
    #[test]
    fn uppercase_blind_window_defers_then_appends_candidate() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        let a = m.feed_char('A');
        assert!(
            matches!(a, Action::UpdatePinyin { .. }),
            "盲窗大写应暂缓，实际 {a:?}"
        );
        let a = m.on_llm_candidates("ni", &["你".into()], true);
        assert!(
            matches!(&a, Action::CommitImmediate(t) if t == "你A"),
            "settle 应重放为候选+大写，实际 {a:?}"
        );
        assert_eq!(m.state(), MachineState::Idle);
    }

    /// 大写盲窗暂缓、settle 空结果 → 原文 + 大写字符（收尾单元知情提交，
    /// 与空格的「展示后二次确认」分流——按下即明确的文本+后缀意图）。
    #[test]
    fn uppercase_blind_window_settles_empty_commits_raw() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        assert!(matches!(m.feed_char('A'), Action::UpdatePinyin { .. }));
        let a = m.on_llm_candidates("ni", &[], true);
        assert!(
            matches!(&a, Action::CommitImmediate(t) if t == "niA"),
            "settle 空结果应原文+大写，实际 {a:?}"
        );
    }

    /// 标点盲窗暂缓、settle 有结果 → 候选 + 全角标点。
    #[test]
    fn punct_blind_window_defers_then_fullwidth() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        assert!(matches!(m.feed_char(','), Action::UpdatePinyin { .. }));
        let a = m.on_llm_candidates("ni", &["你".into()], true);
        assert!(
            matches!(&a, Action::CommitImmediate(t) if t == "你，"),
            "settle 应重放为候选+全角，实际 {a:?}"
        );
    }

    /// 最新意图覆盖旧暂缓：在途按了空格又按标点，settle 只重放标点
    /// （被替换的键已被吞，属盲按保护的可接受损失），且只产生一次提交。
    #[test]
    fn latest_intent_replaces_pending() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        assert_eq!(m.feed_char(' '), Action::None, "空格先暂缓");
        assert!(matches!(m.feed_char(','), Action::UpdatePinyin { .. }));
        let a = m.on_llm_candidates("ni", &["你".into()], true);
        assert!(
            matches!(&a, Action::CommitImmediate(t) if t == "你，"),
            "应只重放最新的标点意图，实际 {a:?}"
        );
        assert_eq!(m.state(), MachineState::Idle);
    }

    /// 成对引号跨暂缓重放：交替状态恰好在重放时翻转一次，之后延续。
    #[test]
    fn paired_quote_alternates_across_deferred_flush() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        assert!(matches!(m.feed_char('"'), Action::UpdatePinyin { .. }));
        let a = m.on_llm_candidates("ni", &["你".into()], true);
        assert!(
            matches!(&a, Action::CommitImmediate(t) if t == "你“"),
            "重放应为候选+开引号，实际 {a:?}"
        );
        // 组合已复回 Idle：下一个双引号是闭引号（交替延续）
        assert_eq!(
            m.feed_char('"'),
            Action::CommitImmediate("”".into()),
            "重放外的下一引号应为闭引号"
        );
    }

    /// 在途暂缓期间缓冲继续增长：settle 按当前全长拼音补执行（选择/回退
    /// 都以增长后的 buffer 为准，行为与无暂缓的即时路径一致）。
    #[test]
    fn deferred_space_settles_against_grown_buffer() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        assert_eq!(m.feed_char(' '), Action::None, "在途空格暂缓");
        m.feed_char('i');
        m.feed_char('k');
        // 结果针对增长后的 "nik" 到达：整段提交
        let a = m.on_llm_candidates("nik", &["你好".into()], true);
        assert!(
            matches!(&a, Action::CommitImmediate(t) if t == "你好"),
            "增长后的 settle 应选全长首候选，实际 {a:?}"
        );
    }

    /// 数字选字在途忽略行为钉住：查询未达时数字键无效且不吞组合；结果到达
    /// 后同一数字正常选词。
    #[test]
    fn digit_while_in_flight_ignored_until_results() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        assert_eq!(
            m.feed_char('2'),
            Action::None,
            "在途数字键应忽略（idx 越界保护）"
        );
        assert_eq!(m.state(), MachineState::Pinyin, "忽略不得破坏组合");
        rime(&mut m, "ni", &["你", "呢"]);
        assert_eq!(m.feed_char('2'), Action::CommitImmediate("呢".into()));
    }

    /// 组合长度到顶（48）后：继续输入被吞，退格仍可删（P2 语义承诺）。
    #[test]
    fn cap_backspace_still_deletes_at_max_buffer() {
        let mut m = CompositionMachine::new();
        for _ in 0..CompositionMachine::MAX_PINYIN_BUFFER {
            m.feed_char('a');
        }
        let before_len = CompositionMachine::MAX_PINYIN_BUFFER;
        m.feed_char('b');
        let remains = match m.feed_backspace() {
            Action::UpdatePinyin { preedit, .. } => preedit,
            other => panic!("退格应刷新组合，实际 {other:?}"),
        };
        assert_eq!(remains.len(), before_len - 1, "封顶后退格应删掉一个字母");
    }

    /// PendingSlash 回退保持半角字面前缀语义（产品裁决 2026-08-27）：
    /// '/' 后的非 '/' 字符（含标点）恒以 "/x" 半角直出，不走全角映射。
    #[test]
    fn slash_fallback_keeps_halfwidth_literal() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        // '/' 先提交拼音并进入 PendingSlash
        assert_eq!(m.feed_char('/'), Action::CommitImmediate("ni".into()));
        let a = m.feed_char(',');
        assert_eq!(a, Action::CommitImmediate("/,".into()), "斜杠通道半角直出");
    }

    /// '/' 盲窗直出裁决钉住（issue #44）：与空格/大写/标点不同，'/' 是显式
    /// 模式切换键，盲窗中也立即提交当前拼音（候选未回达即原文）并进入
    /// PendingSlash——暂缓重放会把候选插进用户正在输入的提示词组合里，
    /// 收益远小于错序风险。见 feed_pinyin_char 的裁决注释。
    #[test]
    fn slash_in_blind_window_commits_raw_enters_pending() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        // 未注入候选：查询在途盲窗。'/' 直出原文拼音并切到 PendingSlash
        assert_eq!(
            m.feed_char('/'),
            Action::CommitImmediate("ni".into()),
            "盲窗中 '/' 应立即提交当前拼音（候选在途，原文兜底）"
        );
        assert_eq!(m.state(), MachineState::PendingSlash);
        // 第二个 '/' 进入提示词模式，提示词组合从零开始
        assert_eq!(
            m.feed_char('/'),
            Action::EnterPrompt {
                preedit: "//".to_owned()
            }
        );
        assert_eq!(m.state(), MachineState::Prompt);
    }

    /// 方向键选字：Down/Up 移动选中（页内 clamp），空格提交选中项；
    /// 候选刷新（rime 注入）后选中回到首项。
    #[test]
    fn arrow_keys_move_selection_and_space_commits_selected() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        rime(&mut m, "ni", &["你", "泥", "拟"]);
        assert!(matches!(
            &m.feed_arrow_down(),
            Action::UpdatePinyin { selected: 1, .. }
        ));
        assert!(matches!(
            &m.feed_arrow_down(),
            Action::UpdatePinyin { selected: 2, .. }
        ));
        assert_eq!(m.feed_arrow_down(), Action::None, "单页末项再 Down 不动作");
        assert!(matches!(
            &m.feed_arrow_up(),
            Action::UpdatePinyin { selected: 1, .. }
        ));
        assert!(matches!(
            &m.feed_arrow_up(),
            Action::UpdatePinyin { selected: 0, .. }
        ));
        // 空格提交当前选中项（方向键移动后 selected=0 → 你）
        assert!(matches!(
            m.feed_char(' '),
            Action::CommitImmediate(t) if t == "你"
        ));
        // 无候选时方向键不动（保持原态）
        let mut m2 = CompositionMachine::new();
        m2.feed_char('x');
        assert_eq!(m2.feed_arrow_down(), Action::None);
        assert_eq!(m2.feed_arrow_up(), Action::None);
    }

    /// 方向键选中后候选刷新（新查询结果到达）→ 选中回落到首项。
    #[test]
    fn candidate_refresh_resets_selection() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        rime(&mut m, "ni", &["你", "泥", "拟"]);
        assert!(matches!(
            &m.feed_arrow_down(),
            Action::UpdatePinyin { selected: 1, .. }
        ));
        rime(&mut m, "ni", &["你", "尼", "呢"]);
        assert!(
            matches!(
                &m.feed_char(' '),
                Action::CommitImmediate(t) if t == "你"
            ),
            "刷新后选中归 0，空格提交首选"
        );
    }

    /// `//<内容>` + Tab：提示词内容走改写管道（StartRewrite）；
    /// 空内容 Tab 无动作；Tab 字符不入提示词。
    #[test]
    fn tab_in_prompt_starts_rewrite() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        for ch in "明天发烧请假条".chars() {
            let _ = m.feed_char(ch);
        }
        assert!(matches!(m.state(), MachineState::Prompt));
        // Tab → 改写（内容清空，进 Streaming）
        assert!(matches!(
            m.feed_char('\t'),
            Action::StartRewrite { content } if content == "明天发烧请假条"
        ));
        assert!(matches!(m.state(), MachineState::Streaming));
        // 空内容 Tab → None（无改写对象）
        let mut m2 = CompositionMachine::new();
        m2.feed_char('/');
        m2.feed_char('/');
        assert_eq!(m2.feed_char('\t'), Action::None);
        assert!(matches!(m2.state(), MachineState::Prompt));
    }

    /// OCR 预览：Enter/空格/数字 1 上屏，Esc 取消，其他键退出预览。
    #[test]
    fn ocr_preview_keys() {
        let mut m = CompositionMachine::new();
        assert_eq!(
            m.begin_ocr_preview("识别文本".to_owned()),
            Some(Action::OcrPreview { text: "识别文本".to_owned() })
        );
        assert!(m.ocr_previewing());
        // Enter 上屏
        assert_eq!(
            m.feed_ocr_preview(PreviewKey::Enter),
            Some(Action::CommitImmediate("识别文本".to_owned()))
        );
        assert!(!m.ocr_previewing());
        // 空格上屏
        let _ = m.begin_ocr_preview("文本2".to_owned());
        assert_eq!(
            m.feed_ocr_preview(PreviewKey::Space),
            Some(Action::CommitImmediate("文本2".to_owned()))
        );
        // 数字 1 上屏
        let _ = m.begin_ocr_preview("文本3".to_owned());
        assert_eq!(
            m.feed_ocr_preview(PreviewKey::Digit1),
            Some(Action::CommitImmediate("文本3".to_owned()))
        );
        // Esc 取消
        let _ = m.begin_ocr_preview("文本4".to_owned());
        assert_eq!(m.feed_ocr_preview(PreviewKey::Escape), Some(Action::Cancel));
        assert!(!m.ocr_previewing());
        // 其他键退出预览（None = 交回正常路由）
        let _ = m.begin_ocr_preview("文本5".to_owned());
        assert_eq!(m.feed_ocr_preview(PreviewKey::Other), None);
        assert!(!m.ocr_previewing());
        // 非 Idle 时不进预览
        m.feed_char('n');
        assert_eq!(m.begin_ocr_preview("x".to_owned()), None);
        assert!(!m.ocr_previewing());
    }

    /// `///`：Prompt 态空提示词按第三个斜杠 → TriggerOcr（选区截图）。
    #[test]
    fn triple_slash_triggers_ocr() {
        let mut m = CompositionMachine::new();
        m.feed_char('/');
        m.feed_char('/');
        assert_eq!(m.state(), MachineState::Prompt);
        // 提示词空时第三个斜杠 → 截图
        assert_eq!(m.feed_char('/'), Action::TriggerOcr);
        // 提示词非空时斜杠按字面入提示词（不触发）
        let mut m2 = CompositionMachine::new();
        m2.feed_char('/');
        m2.feed_char('/');
        for ch in "你好".chars() {
            let _ = m2.feed_char(ch);
        }
        assert!(matches!(m2.feed_char('/'), Action::UpdatePrompt { .. }), "提示词非空时 / 字面入提示词");
    }

    /// 跨页遍历（微软拼音/手心行为）：20 条候选 = 3 页（9+9+2）。
    /// 页尾继续 Down → 下一页首项；末页末项再 Down 不动；
    /// 页首继续 Up → 上一页末项；首页首项再 Up 不动。
    #[test]
    fn arrow_keys_cross_page_at_boundaries() {
        let texts: Vec<String> = (0..20).map(|i| format!("词{i}")).collect();
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let mut m = CompositionMachine::new();
        m.feed_char('y');
        m.feed_char('i');
        rime(&mut m, "yi", &refs);

        // 第 1 页 ↓ 到页尾（selected 8），再 ↓ 翻到第 2 页首项
        for _ in 0..8 {
            assert!(matches!(
                &m.feed_arrow_down(),
                Action::UpdatePinyin { selected, .. } if *selected > 0
            ));
        }
        assert!(
            matches!(
                &m.feed_arrow_down(),
                Action::UpdatePinyin {
                    page: 1,
                    selected: 0,
                    ..
                }
            ),
            "页尾 Down 应翻到第 2 页首项"
        );
        // 第 2 页 ↓ 到页尾再翻第 3 页
        for _ in 0..8 {
            let _ = m.feed_arrow_down();
        }
        assert!(matches!(
            &m.feed_arrow_down(),
            Action::UpdatePinyin {
                page: 2,
                selected: 0,
                ..
            }
        ));
        // 第 3 页仅 2 项：↓ 到末项（selected 1）后再 ↓ 不动
        assert!(matches!(
            &m.feed_arrow_down(),
            Action::UpdatePinyin { selected: 1, .. }
        ));
        assert_eq!(m.feed_arrow_down(), Action::None, "末页末项 Down 不动");
        // ↑ 先回第 3 页首项，再翻回第 2 页末项
        assert!(matches!(
            &m.feed_arrow_up(),
            Action::UpdatePinyin {
                page: 2,
                selected: 0,
                ..
            }
        ));
        assert!(
            matches!(
                &m.feed_arrow_up(),
                Action::UpdatePinyin {
                    page: 1,
                    selected: 8,
                    ..
                }
            ),
            "页首 Up 应翻回上一页末项"
        );
        // 一路 ↑ 回到第 1 页首项：8 次页内 + 1 次翻页 + 8 次页内 = 17 次
        for _ in 0..17 {
            let _ = m.feed_arrow_up();
        }
        assert_eq!(m.feed_arrow_up(), Action::None, "首页首项 Up 不动");
    }

    /// 回归（真机：翻到第 2 页后空格上屏的却是第 1 页首项）：
    /// selected_index 是页内下标，空格提交须换算成全量列表的全局下标。
    #[test]
    fn space_after_page_turn_commits_page_global_index() {
        let texts: Vec<String> = (0..20).map(|i| format!("词{i}")).collect();
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let mut m = CompositionMachine::new();
        m.feed_char('y');
        m.feed_char('i');
        rime(&mut m, "yi", &refs);
        // 翻到第 2 页：8 次页内 + 1 次跨页
        for _ in 0..8 {
            let _ = m.feed_arrow_down();
        }
        assert!(matches!(
            &m.feed_arrow_down(),
            Action::UpdatePinyin {
                page: 1,
                selected: 0,
                ..
            }
        ));
        // 空格应提交第 2 页首项（全局下标 9 = 词9），而非第一页的词0
        assert!(
            matches!(
                m.feed_char(' '),
                Action::CommitImmediate(t) if t == "词9"
            ),
            "翻页后空格须提交当前页选中项（全局下标），而非第一页首项"
        );
        // PageDown 翻页后同样
        let mut m2 = CompositionMachine::new();
        m2.feed_char('y');
        m2.feed_char('i');
        rime(&mut m2, "yi", &refs);
        assert!(matches!(
            &m2.feed_page_down(),
            Action::UpdatePinyin { page: 1, .. }
        ));
        assert!(
            matches!(
                m2.feed_char(' '),
                Action::CommitImmediate(t) if t == "词9"
            ),
            "PageDown 翻页后空格提交第 2 页首项"
        );
    }
}
