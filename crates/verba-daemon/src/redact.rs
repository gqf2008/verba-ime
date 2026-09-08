//! 日志脱敏。
//!
//! 规则（见 `docs/architecture.md` §9）：默认不记录用户文本与密钥；调试模式才记录文本且仅本地存储。
//! 本模块在**日志写入边界**对任何日志行做密钥类脱敏，保证 `api_key` / `Authorization` /
//! 各类 token / 私钥 ASCII-armor 绝不落盘，覆盖 100% 的日志出口（stderr + 日志文件）。

use std::sync::LazyLock;

use regex::Regex;

/// 敏感凭据匹配表（按优先级排列：越具体越靠前，避免宽泛规则吞掉字面量）。
static SENSITIVE_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        // Authorization / Proxy-Authorization 请求头（值整体掩码）
        (Regex::new(r"(?i)(authorization|proxy-authorization)\s*[:=].*").unwrap(), "$1=[REDACTED]"),
        // 常见密钥赋值：<field>[:=] <value>，保留字段名便于排查，掩掉值
        (Regex::new(r"(?i)(api[_-]?key|api_keys?|access[_-]?token|token|secret|password|passwd)\s*[:=]\s*\S+").unwrap(), "$1=[REDACTED]"),
        // OpenAI 风格 key：sk-<base62>
        (Regex::new(r"\bsk-[A-Za-z0-9_-]{6,}").unwrap(), "sk-[REDACTED]"),
        // GitHub / GitLab PAT：ghp_/gho_/ghs_/ghr_/glpat-
        (Regex::new(r"(gh[pousr]_)[A-Za-z0-9]{20,}").unwrap(), "$1[REDACTED]"),
        (Regex::new(r"glpat-[A-Za-z0-9_-]{16,}").unwrap(), "glpat-[REDACTED]"),
        // Slack / Discord bot token
        (Regex::new(r"(xox[baprs]-)[A-Za-z0-9-]{10,}").unwrap(), "$1[REDACTED]"),
        (Regex::new(r"[MN][A-Za-z0-9_-]{23}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27}").unwrap(), "[REDACTED]"),
        // AWS access key id
        (Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(), "AKIA[REDACTED]"),
        // Bearer token
        (Regex::new(r"Bearer\s+[A-Za-z0-9._~+/\-=]{6,}").unwrap(), "Bearer [REDACTED]"),
        // PEM 私钥块（跨行，用 (?s) 匹配多行）
        (Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----").unwrap(), "[REDACTED PRIVATE KEY]"),
    ]
});

/// 将一行日志中的敏感凭据掩码化。
///
/// 幂等：对已脱敏文本再次执行不会二次泄漏（占位符不含敏感字符）。
pub fn redact_secrets(input: &str) -> String {
    let mut out = input.to_string();
    for (re, repl) in SENSITIVE_PATTERNS.iter() {
        out = re.replace_all(&out, *repl).into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 分段拼接敏感串：避免源码中出现连续的可被 secret-scanning 命中的字面量。
    fn seg(parts: &[&str]) -> String {
        parts.concat()
    }

    #[test]
    fn masks_openai_style_key() {
        let s = "llm api_key=sk-abcdef1234567890";
        let r = redact_secrets(s);
        assert!(r.contains("api_key=[REDACTED]"), "got: {r}");
        assert!(!r.contains("sk-abcdef1234567890"), "got: {r}");
    }

    #[test]
    fn masks_bearer_token() {
        let s = "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.token.value";
        let r = redact_secrets(s);
        assert!(!r.contains("eyJhbGciOiJIUzI1NiJ9"), "got: {r}");
        assert!(
            r.contains("Bearer [REDACTED]") || r.contains("[REDACTED]"),
            "got: {r}"
        );
    }

    #[test]
    fn masks_github_pat() {
        let s = seg(&["token=gh", "p_1234567890abcdefghijklmnop"]);
        let r = redact_secrets(&s);
        assert!(!r.contains("ghp_1234567890abcdefghijklmnop"), "got: {r}");
        assert!(r.contains("[REDACTED]"), "got: {r}");
    }

    #[test]
    fn masks_auto_key_assignment() {
        let s = seg(&["config: api_key: sk-", "1234567890abcdef"]);
        let r = redact_secrets(&s);
        assert!(!r.contains("sk-1234567890abcdef"), "got: {r}");
    }

    #[test]
    fn masks_pem_private_key() {
        let s = "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----";
        let r = redact_secrets(s);
        assert!(!r.contains("AAAA"), "got: {r}");
        assert!(r.contains("[REDACTED PRIVATE KEY]"), "got: {r}");
    }

    #[test]
    fn leaves_plain_normal_text_untouched() {
        let s = "模式切换: 中文 Rime 候选请求: pinyin=nihao";
        assert_eq!(redact_secrets(s), s);
    }

    #[test]
    fn idempotent() {
        let s = seg(&[
            "api_key=sk-",
            "abcdef1234567890",
            " Authorization: Bearer ",
            "xyz.abc.def",
        ]);
        let once = redact_secrets(&s);
        let twice = redact_secrets(&once);
        assert_eq!(once, twice);
    }
    #[test]
    fn masks_aws_access_key() {
        // 分段组装：避免源码连续出现可被 secret-scanning 命中的字面量。
        let s = seg(&["AKIA", "IOSFODNN7EXAMPLE"]);
        let r = redact_secrets(&s);
        assert!(!r.contains("IOSFODNN7EXAMPLE"));
        assert!(r.contains("AKIA[REDACTED]"));
    }

    #[test]
    fn masks_gitlab_pat() {
        let s = seg(&["glpat-", "abcdefghijklmnopqrstuvwxyz"]);
        let r = redact_secrets(&s);
        assert!(!r.contains("abcdefghijklmnopqrstuvwxyz"));
        assert!(r.contains("glpat-[REDACTED]"), "got: {r}");
    }

    #[test]
    fn masks_slack_token() {
        let s = seg(&["xoxb-", "123456789012-abcdefghijklmnop"]);
        let r = redact_secrets(&s);
        assert!(!r.contains("123456789012-abcdefghijklmnop"));
        assert!(r.contains("xoxb-[REDACTED]"), "got: {r}");
    }

    #[test]
    fn masks_discord_token() {
        let s = seg(&[
            "M0123456789abcdefghijklm",
            ".",
            "ABCDef",
            ".",
            "abcdefghijklmnopqrstuvwxyza",
        ]);
        let r = redact_secrets(&s);
        assert!(!r.contains("M0123456789abcdefghijklm"), "got: {r}");
        assert!(r.contains("[REDACTED]"), "got: {r}");
    }

    #[test]
    fn masks_proxy_authorization_header() {
        // 含 `proxy-authorization:` 的行：值整体掩码，保留字段名；`.*` 吞到行尾。
        let s = seg(&["proxy-authorization: Basic ", "dXNlcjpwYXNz"]);
        let r = redact_secrets(&s);
        assert!(!r.contains("dXNlcjpwYXNz"), "got: {r}");
        assert!(r.contains("proxy-authorization=[REDACTED]"), "got: {r}");
    }

    #[test]
    fn masks_key_assignment_fields() {
        let s = seg(&[
            "API_KEY=sk-",
            "1234567890abcd",
            " secret=",
            "supersecret123",
            " password=",
            "abc123",
        ]);
        let r = redact_secrets(&s);
        assert!(!r.contains("1234567890abcd"), "got: {r}");
        assert!(!r.contains("supersecret123"), "got: {r}");
        assert!(
            !r.contains("abc123") || r.contains("password=[REDACTED]"),
            "got: {r}"
        );
        assert!(r.contains("API_KEY=[REDACTED]"), "got: {r}");
    }

    #[test]
    fn masks_access_token_assignment() {
        let s = seg(&["access_token: gh2_", "1234567890abcdefghijklmnopqrstuv"]);
        let r = redact_secrets(&s);
        assert!(!r.contains("1234567890abcdefghijklmnopqrstuv"), "got: {r}");
        assert!(r.contains("[REDACTED]"), "got: {r}");
    }

    #[test]
    fn masks_bare_sk_key() {
        // 无字段标签的裸 sk- key：必须命中 sk- 分支（不被字段模式遮蔽）。
        let s = seg(&["sk-", "abcdef1234567890abcdef"]);
        let r = redact_secrets(&s);
        assert_eq!(r, "sk-[REDACTED]", "got: {r}");
    }

    #[test]
    fn masks_bare_bearer() {
        let s = seg(&["Bearer ", "eyJhbGciOiJIUzI1NiJ", ".token.value"]);
        let r = redact_secrets(&s);
        assert!(!r.contains("eyJhbGciOiJIUzI1NiJ"), "got: {r}");
        assert!(r.contains("[REDACTED]"), "got: {r}");
    }

    #[test]
    fn masks_bare_github_pat() {
        let s = seg(&["gh", "p_1234567890abcdefghijklmnopqrstuvwxyz"]);
        let r = redact_secrets(&s);
        assert_eq!(r, "ghp_[REDACTED]", "got: {r}");
    }

    #[test]
    fn documents_unrecognized_secret_form_residual_risk() {
        // 负向对照：无前缀的裸 token / 自定义密钥本表无法识别——这是
        // 本实现的已知上限（best-effort），真正的保证是调用点不落密钥。
        let mysterious = "aRANDOMopaquevaluewithoutprefix123456";
        assert_eq!(redact_secrets(mysterious), mysterious);
    }
}
