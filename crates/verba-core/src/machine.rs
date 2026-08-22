//! 输入组合状态机。
//!
//! 状态流转：
//! ```text
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
    /// AI 提示词（不含 `//` 前缀）。
    prompt: String,
    /// LLM 流式结果。
    result: String,
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
            prompt: String::new(),
            result: String::new(),
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
            MachineState::Prompt => format!("//{}", self.prompt),
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
            MachineState::Prompt => {
                self.prompt.push(c);
                Action::UpdatePrompt {
                    preedit: self.preedit(),
                }
            }
            MachineState::Streaming | MachineState::ResultReady => Action::None,
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
            MachineState::Prompt => {
                if self.prompt.pop().is_some() {
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
            MachineState::Prompt => {
                let prompt = std::mem::take(&mut self.prompt);
                self.state = MachineState::Streaming;
                self.result.clear();
                Action::StartLlm {
                    prompt,
                    system: None,
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
                self.prompt.clear();
                self.result.clear();
                Action::Cancel
            }
        }
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
    fn direct_input_commits_immediately() {
        let mut m = CompositionMachine::new();
        assert_eq!(m.feed_char('h'), Action::CommitImmediate("h".into()));
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
}
