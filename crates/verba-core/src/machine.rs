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
    /// `llm_request`：拼音变更后是否需向 LLM 请求融合候选（前端负责防抖与取消）。
    UpdatePinyin {
        preedit: String,
        candidates: Vec<String>,
        page: usize,
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
    /// 取消当前组合（Esc / 清空）。
    Cancel,
    /// LLM 出错，已回到 Idle。
    LlmFailed { message: String },
}

/// 拼音候选融合请求：在词库候选基础上请 LLM 补充语境候选。
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
    /// 词库候选（由拼音引擎查询得到）。
    dictionary_candidates: Vec<String>,
    /// LLM 补充候选（候选融合，可为空）。
    llm_candidates: Vec<String>,
    /// 融合后的展示候选（词库 + LLM，去重），供选择/分页/提交。
    pinyin_candidates: Vec<String>,
    /// 已发起 LLM 候选请求的拼音（避免同一拼音重复请求）。
    last_candidates_request: Option<String>,
    /// 当前候选页码（0 起）。
    pinyin_page: usize,
    /// AI 提示词（不含 `//` 前缀）。
    prompt: String,
    /// LLM 流式结果。
    result: String,
    /// 拼音引擎。
    engine: verba_pinyin::PinyinEngine,
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
            last_candidates_request: None,
            pinyin_page: 0,
            prompt: String::new(),
            result: String::new(),
            engine: verba_pinyin::PinyinEngine::new(),
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
        Action::UpdatePinyin {
            preedit: self.pinyin_composition_preedit(),
            candidates: self.pinyin_candidates.clone(),
            page: self.pinyin_page,
            llm_request: None,
        }
    }

    /// 输入一个可打印字符。
    pub fn feed_char(&mut self, c: char) -> Action {
        match self.state {
            MachineState::Idle => {
                if c == '/' {
                    self.state = MachineState::PendingSlash;
                    Action::UpdatePrompt {
                        preedit: "/".to_owned(),
                    }
                } else if c.is_ascii_alphabetic() {
                    // 字母开始拼音组合
                    self.state = MachineState::Pinyin;
                    self.pinyin_buffer.clear();
                    self.pinyin_buffer.push(c.to_ascii_lowercase());
                    self.refresh_candidates();
                    Action::UpdatePinyin {
                        preedit: self.pinyin_composition_preedit(),
                        candidates: self.pinyin_candidates.clone(),
                        page: self.pinyin_page,
                        llm_request: self.request_llm_candidates_if_needed(),
                    }
                } else {
                    Action::CommitImmediate(c.to_string())
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
            if let Some(text) = self.pinyin_candidates.get(idx) {
                let text = text.clone();
                self.reset_pinyin();
                return Action::CommitImmediate(text);
            }
            // 候选不存在：忽略该数字（不吞后文）
            return Action::None;
        }
        if c.is_ascii_alphabetic() {
            self.pinyin_buffer.push(c.to_ascii_lowercase());
            self.refresh_candidates();
            return Action::UpdatePinyin {
                preedit: self.pinyin_composition_preedit(),
                candidates: self.pinyin_candidates.clone(),
                page: self.pinyin_page,
                llm_request: self.request_llm_candidates_if_needed(),
            };
        }
        if c == ' ' {
            let text = self.commit_pinyin_text();
            self.reset_pinyin();
            return Action::CommitImmediate(text);
        }
        // 其它可打印字符：提交候选 0 + 该字符，避免吞字
        let text = format!("{}{c}", self.commit_pinyin_text());
        self.reset_pinyin();
        Action::CommitImmediate(text)
    }

    /// 提示词态的字符输入：支持拼音组合（字母→候选→选中上屏到提示词）。
    /// 无候选时按原文提交（保住 `//translate hello` 这类英文提示词流程）。
    fn feed_prompt_char(&mut self, c: char) -> Action {
        if self.pinyin_composing() {
            if c.is_ascii_alphabetic() {
                self.pinyin_buffer.push(c.to_ascii_lowercase());
                self.refresh_candidates();
                return Action::UpdatePrompt {
                    preedit: self.preedit(),
                };
            }
            if c.is_ascii_digit() && c != '0' {
                let idx = (c as u8 - b'1') as usize;
                if let Some(text) = self.pinyin_candidates.get(idx) {
                    self.prompt.push_str(text);
                    self.clear_pinyin();
                    return Action::UpdatePrompt {
                        preedit: self.preedit(),
                    };
                }
                return Action::None;
            }
            if c == ' ' || c == '/' {
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
        // 未组合：字母开始拼音；其它字符直接入提示词
        if c.is_ascii_alphabetic() {
            self.pinyin_buffer.clear();
            self.pinyin_buffer.push(c.to_ascii_lowercase());
            self.refresh_candidates();
            return Action::UpdatePrompt {
                preedit: self.preedit(),
            };
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
                if self.pinyin_buffer.pop().is_some() {
                    self.refresh_candidates();
                    if self.pinyin_buffer.is_empty() {
                        self.reset_pinyin();
                        Action::Cancel
                    } else {
                        Action::UpdatePinyin {
                            preedit: self.pinyin_composition_preedit(),
                            candidates: self.pinyin_candidates.clone(),
                            page: self.pinyin_page,
                            llm_request: self.request_llm_candidates_if_needed(),
                        }
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
                let text = self.commit_pinyin_text();
                self.reset_pinyin();
                Action::CommitImmediate(text)
            }
            MachineState::Prompt => {
                if self.pinyin_composing() {
                    // 组合中：先提交拼音（候选或原文）到提示词，不触发 LLM
                    let text = self.commit_pinyin_text();
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
                self.last_candidates_request = None;
                self.prompt.clear();
                self.result.clear();
                Action::Cancel
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
        self.last_candidates_request = None;
        self.pinyin_page = 0;
    }

    /// 拼音组合区的 preedit（`buffer 1.候选 2.候选…`），无候选时仅缓冲。
    fn pinyin_preedit(&self) -> String {
        if self.pinyin_candidates.is_empty() {
            self.pinyin_buffer.clone()
        } else {
            let mut out = self.pinyin_buffer.clone();
            for (i, cand) in self.pinyin_candidates.iter().enumerate() {
                out.push_str(&format!(" {}.{cand}", i + 1));
            }
            out
        }
    }

    /// 纯拼音组合 preedit（不含内联候选；候选窗接管显示时使用）。
    /// 提示词态带 `//` 与已提交提示词前缀。
    pub fn pinyin_composition_preedit(&self) -> String {
        match self.state {
            MachineState::Pinyin => self.pinyin_buffer.clone(),
            MachineState::Prompt => format!("//{}{}", self.prompt, self.pinyin_buffer),
            _ => String::new(),
        }
    }

    /// 当前拼音提交文本：有候选取候选 0，否则取原始缓冲。
    fn commit_pinyin_text(&self) -> String {
        if let Some(first) = self.pinyin_candidates.first() {
            first.clone()
        } else {
            self.pinyin_buffer.clone()
        }
    }

    /// 重置拼音状态回 Idle。
    fn reset_pinyin(&mut self) {
        self.state = MachineState::Idle;
        self.pinyin_buffer.clear();
        self.dictionary_candidates.clear();
        self.llm_candidates.clear();
        self.pinyin_candidates.clear();
        self.last_candidates_request = None;
        self.pinyin_page = 0;
    }

    /// 用当前缓冲刷新候选（缓冲变化时回到第 1 页，并丢弃旧 LLM 候选）。
    fn refresh_candidates(&mut self) {
        self.pinyin_page = 0;
        self.dictionary_candidates = self
            .engine
            .lookup(&self.pinyin_buffer)
            .into_iter()
            .map(|c| c.text)
            .collect();
        self.llm_candidates.clear();
        self.fuse_candidates();
    }

    /// 融合展示候选 = 词库候选 ++ LLM 候选（LLM 侧已去重，此处再兜底）。
    fn fuse_candidates(&mut self) {
        self.pinyin_candidates = self.dictionary_candidates.clone();
        for cand in &self.llm_candidates {
            if !self.pinyin_candidates.contains(cand) {
                self.pinyin_candidates.push(cand.clone());
            }
        }
    }

    /// 拼音变更后是否需要发起 LLM 候选请求（同一拼音只请求一次）。
    fn request_llm_candidates_if_needed(&mut self) -> Option<LlmCandidateRequest> {
        if self.state != MachineState::Pinyin || self.pinyin_buffer.is_empty() {
            self.last_candidates_request = None;
            return None;
        }
        if self.last_candidates_request.as_deref() == Some(self.pinyin_buffer.as_str()) {
            return None;
        }
        self.last_candidates_request = Some(self.pinyin_buffer.clone());
        Some(LlmCandidateRequest {
            pinyin: self.pinyin_buffer.clone(),
            dictionary: self.dictionary_candidates.clone(),
        })
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
        let _ = done;
        if self.state != MachineState::Pinyin || self.pinyin_buffer != pinyin {
            return Action::None;
        }
        let mut changed = false;
        for cand in candidates {
            let cand = cand.trim();
            if cand.is_empty() {
                continue;
            }
            if self.llm_candidates.iter().any(|c| c == cand)
                || self.dictionary_candidates.iter().any(|c| c == cand)
            {
                continue;
            }
            self.llm_candidates.push(cand.to_owned());
            changed = true;
        }
        if !changed {
            return Action::None;
        }
        self.fuse_candidates();
        Action::UpdatePinyin {
            preedit: self.pinyin_composition_preedit(),
            candidates: self.pinyin_candidates.clone(),
            page: self.pinyin_page,
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
        assert_eq!(m.feed_char('.'), Action::CommitImmediate(".".into()));
        assert_eq!(m.feed_char('5'), Action::CommitImmediate("5".into()));
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn pinyin_space_commits_first_candidate() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        assert_eq!(m.state(), MachineState::Pinyin);
        let first = m.commit_pinyin_text();
        assert_eq!(first, "你", "ni 首选应为 你，实际 {first:?}");
        let a = m.feed_char(' ');
        assert_eq!(a, Action::CommitImmediate("你".into()));
        assert_eq!(m.state(), MachineState::Idle);
    }

    #[test]
    fn pinyin_digit_selects_candidate() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        // 候选索引 1（第二个）
        if m.pinyin_candidates.len() > 1 {
            let expected = m.pinyin_candidates[1].clone();
            let a = m.feed_char('2');
            assert_eq!(a, Action::CommitImmediate(expected));
            assert_eq!(m.state(), MachineState::Idle);
        }
    }

    #[test]
    fn pinyin_engine_returns_more_than_one_page() {
        // 引擎候选数须多于 1 页，否则分页无意义。
        let mut m = CompositionMachine::new();
        let a = m.feed_char('n');
        match a {
            Action::UpdatePinyin {
                candidates, page, ..
            } => {
                assert_eq!(page, 0);
                assert!(
                    candidates.len() > CompositionMachine::PINYIN_PAGE_SIZE,
                    "候选应多于一页（{} > {}），实际 {} 个",
                    candidates.len(),
                    CompositionMachine::PINYIN_PAGE_SIZE,
                    candidates.len()
                );
            }
            other => panic!("应进入拼音，实际 {other:?}"),
        }
    }

    #[test]
    fn pinyin_page_down_advances_and_wraps() {
        let mut m = CompositionMachine::new();
        let full = match m.feed_char('n') {
            Action::UpdatePinyin {
                candidates, page, ..
            } => {
                assert_eq!(page, 0);
                candidates
            }
            other => panic!("应进入拼音，实际 {other:?}"),
        };
        let total = full.len().div_ceil(CompositionMachine::PINYIN_PAGE_SIZE);
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
        assert_eq!(m.feed_char('/'), Action::CommitImmediate("你".into()));
        assert_eq!(m.state(), MachineState::PendingSlash);
        m.feed_char('/');
        assert_eq!(m.state(), MachineState::Prompt);
    }

    #[test]
    fn pinyin_enter_commits_and_escape_cancels() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        assert_eq!(m.feed_enter(), Action::CommitImmediate("你".into()));
        assert_eq!(m.state(), MachineState::Idle);

        let mut m2 = CompositionMachine::new();
        m2.feed_char('n');
        m2.feed_char('i');
        assert_eq!(m2.feed_escape(), Action::Cancel);
        assert_eq!(m2.state(), MachineState::Idle);
    }

    #[test]
    fn pinyin_punctuation_commits_candidate_plus_char() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        m.feed_char('i');
        let a = m.feed_char(',');
        assert!(
            matches!(a, Action::CommitImmediate(_)),
            "标点应提交候选+标点，实际 {a:?}"
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
        // 第一次 Enter：提交拼音到提示词，不触发 LLM
        let a1 = m.feed_enter();
        assert!(
            matches!(a1, Action::UpdatePrompt { .. }),
            "组合中 Enter 应先提交拼音: {a1:?}"
        );
        assert_eq!(m.prompt(), "你好");
        // 第二次 Enter：无组合 → 提交 LLM
        assert_eq!(
            m.feed_enter(),
            Action::StartLlm {
                prompt: "你好".into(),
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
        let req = llm_req.expect("拼音变更后应请求 LLM 候选");
        assert_eq!(req.pinyin, "n");
        assert!(!req.dictionary.is_empty(), "词库候选应非空");
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
        let dict = match m.feed_char('n') {
            Action::UpdatePinyin { candidates, .. } => candidates,
            other => panic!("应进入拼音，实际 {other:?}"),
        };
        // 与词库候选重复的忽略，新候选追加到尾部
        let a = m.on_llm_candidates("n", &[dict[0].clone(), "你是".into()], false);
        match a {
            Action::UpdatePinyin {
                candidates, page, ..
            } => {
                assert_eq!(page, 0);
                assert_eq!(candidates.len(), dict.len() + 1);
                assert_eq!(candidates[dict.len()], "你是");
            }
            other => panic!("融合应返回更新，实际 {other:?}"),
        }
        // 已存在的 LLM 候选不重复
        let a = m.on_llm_candidates("n", &["你是".into(), "你好".into()], false);
        match a {
            Action::UpdatePinyin { candidates, .. } => {
                assert_eq!(candidates.len(), dict.len() + 2);
                assert_eq!(candidates[dict.len() + 1], "你好");
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
            .position(|c| c == "你是")
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
    fn page_flip_does_not_re_request_llm() {
        let mut m = CompositionMachine::new();
        m.feed_char('n');
        assert!(matches!(
            m.feed_page_down(),
            Action::UpdatePinyin {
                llm_request: None,
                ..
            }
        ));
    }
}
