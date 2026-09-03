//! `//` 多模态命令解析（统一收口）。
//!
//! 此前命令判定内联在 Windows 前端的 StartLlm 分支（macOS 完全没有）——
//! 同一 `//截图` 在两端行为分叉；AI 结果浮层的重试（feed_ai_preview）要
//! 原样还原命令语义，也依赖同一份判定。收口到 core：两端与重试共用
//! 同一份次序与措辞（与 REWRITE_SYSTEM_PROMPT 收口同一理由）。

/// 一条 `//` 提示词解析出的命令类别。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiCommand {
    /// `//朗读 [：]文本` → TTS 合成播放（不落盘文本）。携带提取后的朗读文本。
    Tts { text: String },
    /// `//短语 名称` → 插入用户定义模板。名称能否解析由前端查配置决定，
    /// 解析失败按普通生成兜底（config 依赖留在前端，core 不引配置）。
    Phrase { name: String },
    /// `//看图` → vision 截屏生成。**不结束组合、不重置状态机**，保持流式
    /// 输出通道（与普通生成同路）。
    Vision,
    /// `//截图` → 全屏截图 OCR（结束组合 + 重置状态机 + 异步触发）。
    FullScreenOcr,
    /// `//听写` → 录音 ASR（结束组合 + 重置状态机 + 异步触发）。
    Asr,
    /// 普通文本生成。daemon 侧命令（`//重置`/`//reset`/`//会话`，见
    /// verba-daemon handler）也落此类——**前端不得拦截**，原样送 daemon。
    Llm,
}

/// 解析 `//` 提示词（不含 `//` 前缀）为命令。判定次序与原 Windows 实现
/// 一致：`朗读`/`短语` 前缀匹配 → `看图` 精确 → `截图`/`听写` 精确 →
/// 普通生成。`看图` 与 `截图`/`听写` 互为精确匹配本无冲突，但保持次序
/// 防止后续加入前缀类命令时意外遮蔽。
pub fn parse_ai_command(prompt: &str) -> AiCommand {
    let cmd = prompt.trim();
    if cmd.starts_with("朗读") {
        return AiCommand::Tts {
            text: tts_text_of(cmd),
        };
    }
    if let Some(name) = cmd.strip_prefix("短语") {
        let name = name.trim();
        if !name.is_empty() {
            return AiCommand::Phrase {
                name: name.to_owned(),
            };
        }
    }
    match cmd {
        "看图" => AiCommand::Vision,
        "截图" => AiCommand::FullScreenOcr,
        "听写" => AiCommand::Asr,
        _ => AiCommand::Llm,
    }
}

/// `//朗读 xxx` → 朗读文本（去前缀与分隔符）。
pub fn tts_text_of(prompt: &str) -> String {
    prompt
        .trim_start_matches("朗读")
        .trim_start_matches(|c| ":： \t".contains(c))
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_all_commands() {
        assert_eq!(
            parse_ai_command("朗读 你好"),
            AiCommand::Tts {
                text: "你好".into()
            }
        );
        assert_eq!(
            parse_ai_command("朗读：你好"),
            AiCommand::Tts {
                text: "你好".into()
            }
        );
        assert_eq!(
            parse_ai_command("短语 请假条"),
            AiCommand::Phrase {
                name: "请假条".into()
            }
        );
        assert_eq!(parse_ai_command("看图"), AiCommand::Vision);
        assert_eq!(parse_ai_command("截图"), AiCommand::FullScreenOcr);
        assert_eq!(parse_ai_command("听写"), AiCommand::Asr);
        assert_eq!(parse_ai_command("翻译 hello"), AiCommand::Llm);
    }

    #[test]
    fn daemon_commands_stay_llm() {
        // `//重置`/`//会话` 由 daemon 处理，前端不得拦截——解析保持 Llm。
        assert_eq!(parse_ai_command("重置"), AiCommand::Llm);
        assert_eq!(parse_ai_command("reset"), AiCommand::Llm);
        assert_eq!(parse_ai_command("会话"), AiCommand::Llm);
    }

    #[test]
    fn vision_exact_match_before_kinds() {
        // 精确匹配互不遮蔽；带参数的「看图」不是 vision（与 Windows 一致）。
        assert_eq!(parse_ai_command("看图 这个"), AiCommand::Llm);
        // 短语优先于看图（前缀类先判，与原次序一致）
        assert_eq!(
            parse_ai_command("短语看图"),
            AiCommand::Phrase {
                name: "看图".into()
            }
        );
        // 空名/裸前缀不构成短语命令
        assert_eq!(parse_ai_command("短语"), AiCommand::Llm);
        assert_eq!(parse_ai_command("短语  "), AiCommand::Llm);
    }

    #[test]
    fn tts_text_strips_prefix_and_separators() {
        assert_eq!(tts_text_of("朗读 你好"), "你好");
        assert_eq!(tts_text_of("朗读：你好"), "你好");
        assert_eq!(tts_text_of("朗读"), "");
    }
}
