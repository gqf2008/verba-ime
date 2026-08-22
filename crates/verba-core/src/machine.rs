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
    UpdatePinyin {
        preedit: String,
        candidates: Vec<String>,
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
    /// 当前拼音候选（由引擎查询得到，供选择/展示）。
    pinyin_candidates: Vec<String>,
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
            pinyin_candidates: Vec::new(),
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
        if c.is_ascii_digit() && c != '0' {
            let idx = (c as u8 - b'1') as usize;
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
                self.pinyin_candidates.clear();
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
        self.pinyin_candidates.clear();
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
        self.pinyin_candidates.clear();
    }

    /// 用当前缓冲刷新候选。
    fn refresh_candidates(&mut self) {
        self.pinyin_candidates = self
            .engine
            .lookup(&self.pinyin_buffer)
            .into_iter()
            .map(|c| c.text)
            .collect();
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
}
