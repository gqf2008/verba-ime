//! edge-tts provider：微软 Edge 在线神经音色（免费，非官方接口）。
//!
//! 协议参考 rany2/edge-tts（Python）与 fairkid-ai/go-edge-tts（Go）：
//! - 连接 wss://speech.platform.bing.com/.../edge/v1，带 TrustedClientToken 与 Sec-MS-GEC 签名
//! - 先发 speech.config（输出 MP3），再发 SSML 合成请求
//! - 文本帧：Path 为 turn.start / audio.metadata / turn.end（头部 + CRLFCRLF + 数据）
//! - 二进制帧：2 字节大端头长度 | 头部（Path: audio, Content-Type: audio/mpeg）| MP3 音频
//! - 收到 turn.end 后连接结束，累积音频即合成结果

use std::fmt::Write as _;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use verba_ai::{TtsAudio, TtsProvider};

use crate::{TtsError, DEFAULT_VOICE};

/// 微软 Edge TTS 服务地址。
const WSS_BASE: &str =
    "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1";
/// 官方客户端固定令牌（社区逆向所得）。
const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
/// 与令牌配套的客户端版本。
const SEC_MS_GEC_VERSION: &str = "1-143.0.3650.75";
/// Windows 文件时间纪元（1601-01-01）到 Unix 纪元（1970-01-01）的秒差。
const WIN_EPOCH_SECS: u64 = 11_644_473_600;
/// 单次合成超时。
const SYNTHESIZE_TIMEOUT: Duration = Duration::from_secs(30);
/// speech.config 的 JSON 载荷（输出 24kHz 48kbps 单声道 MP3）。
const SPEECH_CONFIG_JSON: &str = "{\"context\":{\"synthesis\":{\"audio\":{\"metadataoptions\":{\"sentenceBoundaryEnabled\":\"true\",\"wordBoundaryEnabled\":\"false\"},\"outputFormat\":\"audio-24khz-48kbitrate-mono-mp3\"}}}}";

/// Edge 在线 TTS（真实联网，输出 MP3）。
#[derive(Debug, Clone)]
pub struct EdgeTts {
    voice: String,
}

impl EdgeTts {
    /// 创建 Edge provider；voice 为空时使用默认中文女声。
    pub fn new(voice: String) -> Self {
        let voice = if voice.is_empty() {
            DEFAULT_VOICE.to_owned()
        } else {
            voice
        };
        Self {
            voice: canonical_voice(&voice),
        }
    }
}

impl TtsProvider for EdgeTts {
    type Error = TtsError;

    async fn synthesize(&self, text: &str) -> Result<TtsAudio, TtsError> {
        let text = sanitize_text(text);
        if text.trim().is_empty() {
            return Err(TtsError::EmptyText);
        }
        let date = date_to_string();

        let mut request = build_ws_url()
            .into_client_request()
            .map_err(|e| TtsError::Edge(format!("构造 WSS 请求失败: {e}")))?;
        {
            let headers = request.headers_mut();
            headers.insert("Pragma", HeaderValue::from_static("no-cache"));
            headers.insert("Cache-Control", HeaderValue::from_static("no-cache"));
            headers.insert(
                "Origin",
                HeaderValue::from_static("chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold"),
            );
            headers.insert(
                "User-Agent",
                HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0"),
            );
            headers.insert(
                "Accept-Encoding",
                HeaderValue::from_static("gzip, deflate, br, zstd"),
            );
            headers.insert(
                "Accept-Language",
                HeaderValue::from_static("en-US,en;q=0.9"),
            );
            let cookie = format!("muid={};", new_id().to_uppercase());
            let cookie = cookie
                .parse::<HeaderValue>()
                .map_err(|e| TtsError::Edge(format!("Cookie 头非法: {e}")))?;
            headers.insert("Cookie", cookie);
        }
        let (mut ws, _) = connect_async(request)
            .await
            .map_err(|e| TtsError::Edge(format!("连接 Edge TTS 失败: {e}")))?;

        // 1) 配置合成参数（输出 MP3）。
        let speech_config = format!("X-Timestamp:{date}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n{SPEECH_CONFIG_JSON}\r\n");
        ws.send(Message::Text(speech_config))
            .await
            .map_err(|e| TtsError::Edge(format!("发送 speech.config 失败: {e}")))?;

        // 2) 发送 SSML 合成请求。
        let ssml = mkssml(&self.voice, &escape_xml(&text));
        let ssml_req = format!("X-RequestId:{}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{date}Z\r\nPath:ssml\r\n\r\n{ssml}", new_id());
        ws.send(Message::Text(ssml_req))
            .await
            .map_err(|e| TtsError::Edge(format!("发送 SSML 失败: {e}")))?;

        // 3) 收取音频直到 turn.end / 连接关闭 / 超时。
        let mut audio: Vec<u8> = Vec::new();
        let mut audio_expected = false;
        let recv = async {
            while let Some(msg) = ws.next().await {
                let msg = msg.map_err(|e| TtsError::Edge(format!("接收消息失败: {e}")))?;
                match msg {
                    Message::Text(t) => {
                        let s = t.as_str().to_owned();
                        if let Some((headers, _data)) = split_text_frame(&s) {
                            match header_path(headers) {
                                Some("turn.start") => audio_expected = true,
                                Some("turn.end") => break,
                                _ => {}
                            }
                        }
                    }
                    Message::Binary(b) => {
                        if audio_expected {
                            if let Some(chunk) = parse_binary_audio(&b)? {
                                audio.extend_from_slice(&chunk);
                            }
                        }
                    }
                    Message::Ping(p) => {
                        ws.send(Message::Pong(p))
                            .await
                            .map_err(|e| TtsError::Edge(format!("回 Pong 失败: {e}")))?;
                    }
                    Message::Close(_) => {
                        if audio.is_empty() {
                            return Err(TtsError::Edge("连接被服务端关闭且未收到音频".into()));
                        }
                        break;
                    }
                    _ => {}
                }
            }
            Ok::<(), TtsError>(())
        };
        tokio::time::timeout(SYNTHESIZE_TIMEOUT, recv)
            .await
            .map_err(|_| TtsError::Edge("合成超时（30s）".into()))??;

        if audio.is_empty() {
            return Err(TtsError::Edge("服务端未返回音频数据".into()));
        }
        Ok(TtsAudio {
            format: "mp3",
            bytes: audio,
        })
    }
}

/// 构建 WSS 地址（令牌 + 连接 ID + 防重放签名）。
fn build_ws_url() -> String {
    format!("{WSS_BASE}?TrustedClientToken={TRUSTED_CLIENT_TOKEN}&ConnectionId={}&Sec-MS-GEC={}&Sec-MS-GEC-Version={SEC_MS_GEC_VERSION}", new_id(), generate_sec_ms_gec())
}

/// 生成 32 位小写十六进制连接/请求 ID。
fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 生成 Sec-MS-GEC 防重放签名：Windows 文件时间（100ns，向下取整到 5 分钟）+ 令牌 → SHA256 大写十六进制。
fn generate_sec_ms_gec() -> String {
    let unix = chrono::Utc::now().timestamp().max(0) as u64;
    let win_secs = unix + WIN_EPOCH_SECS;
    let rounded = win_secs - (win_secs % 300);
    let ticks = rounded * 10_000_000;
    let digest = Sha256::digest(format!("{ticks}{TRUSTED_CLIENT_TOKEN}").as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        let _ = write!(out, "{b:02X}");
    }
    out
}

/// JavaScript 风格时间串（服务端校验用）。
fn date_to_string() -> String {
    chrono::Utc::now()
        .format("%a %b %d %Y %H:%M:%S GMT+0000 (Coordinated Universal Time)")
        .to_string()
}

/// 短语音名转服务端长名：zh-CN-XiaoxiaoNeural → Microsoft Server Speech Text to Speech Voice (zh-CN, XiaoxiaoNeural)。
fn canonical_voice(voice: &str) -> String {
    if voice.starts_with("Microsoft Server Speech Text to Speech Voice") {
        return voice.to_owned();
    }
    let mut it = voice.splitn(3, "-");
    let (Some(lang), Some(region), Some(name_part)) = (it.next(), it.next(), it.next()) else {
        return voice.to_owned();
    };
    let name = name_part
        .find("-")
        .map_or(name_part, |i| &name_part[i + 1..]);
    if lang.len() >= 2
        && lang.chars().all(|c| c.is_ascii_lowercase())
        && region.len() >= 2
        && region.chars().all(|c| c.is_ascii_uppercase())
        && name_part.ends_with("Neural")
    {
        format!("Microsoft Server Speech Text to Speech Voice ({lang}-{region}, {name})")
    } else {
        voice.to_owned()
    }
}

/// SSML 请求体。
fn mkssml(voice: &str, escaped: &str) -> String {
    format!("<speak version=\"1.0\" xmlns=\"http://www.w3.org/2001/10/synthesis\" xml:lang=\"en-US\"><voice name=\"{voice}\"><prosody pitch=\"+0Hz\" rate=\"+0%\" volume=\"+0%\">{escaped}</prosody></voice></speak>")
}

/// 清理服务端不支持的字符（0-8、11-12、14-31 替换为空格）。
fn sanitize_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let code = c as u32;
        if (0..=8).contains(&code) || (11..=12).contains(&code) || (14..=31).contains(&code) {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// XML 转义文本内容（含引号与撇号）。
fn escape_xml(text: &str) -> String {
    text.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace("\"", "&quot;")
        .replace(char::from(39), "&apos;")
}

/// 拆分文本帧为（头部, 数据）。
fn split_text_frame(s: &str) -> Option<(&str, &str)> {
    let idx = s.find("\r\n\r\n")?;
    Some((&s[..idx], &s[idx + 4..]))
}

/// 取头部 Path 值。
fn header_path(headers: &str) -> Option<&str> {
    headers.lines().find_map(|line| {
        let (k, v) = line.split_once(":")?;
        if k.trim().eq_ignore_ascii_case("Path") {
            Some(v.trim())
        } else {
            None
        }
    })
}

/// 解析二进制音频帧：2 字节大端头长 + 头部 + 音频。
/// Ok(None) 表示流结束标记（无数据），Ok(Some(v)) 为音频块。
fn parse_binary_audio(data: &[u8]) -> Result<Option<Vec<u8>>, TtsError> {
    if data.len() < 2 {
        return Err(TtsError::Edge("二进制帧过短".into()));
    }
    let header_len = u16::from_be_bytes([data[0], data[1]]) as usize;
    if data.len() < header_len + 2 {
        return Err(TtsError::Edge("二进制帧缺音频数据".into()));
    }
    let header = std::str::from_utf8(&data[2..2 + header_len])
        .map_err(|_| TtsError::Edge("音频帧头部非 UTF-8".into()))?;
    let audio = &data[2 + header_len..];
    let is_audio = header.lines().any(|l| {
        l.split_once(":").is_some_and(|(k, v)| {
            k.trim().eq_ignore_ascii_case("Path") && v.trim().eq_ignore_ascii_case("audio")
        })
    });
    if !is_audio {
        return Ok(None);
    }
    let content_type = header.lines().find_map(|l| {
        l.split_once(":").and_then(|(k, v)| {
            if k.trim().eq_ignore_ascii_case("Content-Type") {
                Some(v.trim().to_owned())
            } else {
                None
            }
        })
    });
    match content_type.as_deref() {
        Some("audio/mpeg") => {
            if audio.is_empty() {
                Err(TtsError::Edge("音频帧数据为空".into()))
            } else {
                Ok(Some(audio.to_vec()))
            }
        }
        None => {
            if audio.is_empty() {
                Ok(None)
            } else {
                Err(TtsError::Edge("无 Content-Type 却携带数据".into()))
            }
        }
        Some(other) => Err(TtsError::Edge(format!("未知音频 Content-Type: {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_removes_control_chars() {
        let s = sanitize_text("a\u{0}\u{0b}\u{0c}\u{1f}b");
        assert_eq!(s, "a    b");
    }

    #[test]
    fn escape_escapes_xml_specials() {
        assert_eq!(escape_xml("a&b<c>d\"e"), "a&amp;b&lt;c&gt;d&quot;e");
    }

    #[test]
    fn escape_escapes_apostrophe() {
        let ap = char::from(39);
        let out = escape_xml(&format!("f{ap}g"));
        assert_eq!(out, format!("f{}{}g", "&", "apos;"));
    }

    #[test]
    fn canonical_voice_short_to_long() {
        assert_eq!(
            canonical_voice("zh-CN-XiaoxiaoNeural"),
            "Microsoft Server Speech Text to Speech Voice (zh-CN, XiaoxiaoNeural)"
        );
        assert_eq!(
            canonical_voice("Microsoft Server Speech Text to Speech Voice (zh-CN, XiaoxiaoNeural)"),
            "Microsoft Server Speech Text to Speech Voice (zh-CN, XiaoxiaoNeural)"
        );
        assert_eq!(canonical_voice("plain"), "plain");
    }

    #[test]
    fn sec_ms_gec_is_uppercase_hex() {
        let gec = generate_sec_ms_gec();
        assert_eq!(gec.len(), 64);
        assert!(gec.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(gec, gec.to_uppercase());
    }

    #[test]
    fn date_string_matches_js_style() {
        let d = date_to_string();
        assert!(
            d.ends_with("GMT+0000 (Coordinated Universal Time)"),
            "got {d}"
        );
        assert!(d
            .split_whitespace()
            .next()
            .unwrap()
            .chars()
            .all(|c| c.is_ascii_alphabetic()));
    }

    #[test]
    fn split_text_frame_extracts_path() {
        let frame = "Path: turn.start\r\nX-RequestId:abc\r\n\r\n{\"data\":1}";
        let (headers, data) = split_text_frame(frame).unwrap();
        assert_eq!(header_path(headers), Some("turn.start"));
        assert_eq!(data, "{\"data\":1}");
    }

    #[test]
    fn parse_binary_audio_frame() {
        let header = b"Path: audio\r\nContent-Type: audio/mpeg\r\n\r\n";
        let mut frame = Vec::new();
        frame.extend_from_slice(&(header.len() as u16).to_be_bytes());
        frame.extend_from_slice(header);
        frame.extend_from_slice(&[0xFF, 0xFB, 0x90]);
        let parsed = parse_binary_audio(&frame).unwrap().unwrap();
        assert_eq!(parsed, vec![0xFF, 0xFB, 0x90]);
    }

    #[test]
    fn parse_binary_audio_terminal_marker() {
        let header = b"Path: audio\r\n\r\n";
        let mut frame = Vec::new();
        frame.extend_from_slice(&(header.len() as u16).to_be_bytes());
        frame.extend_from_slice(header);
        assert!(parse_binary_audio(&frame).unwrap().is_none());
    }
}
