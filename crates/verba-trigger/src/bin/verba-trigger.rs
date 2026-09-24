//! Verba 触发工具（跨平台，issue #82）：选区截图→OCR、录音→ASR、TTS→合成/播放。
//!
//! 自 v0.2.4 的 Windows 专版（frontends/windows/ime/src/bin/verba-trigger.rs）
//! 迁为共享 crate bin：Windows/macOS/Linux 同名同参，前端各自 spawn 本进程
//! （TSF DLL / IMK 进程内不承载选区 UI 与事件循环）。
//!
//! daemon 连接：验活握手失败时拉起**同目录**的 verba-daemon（安装布局两端
//! 一致：{app} 目录 / Verba.app/Contents/MacOS），Windows 加 CREATE_NO_WINDOW
//! 防控制台闪窗，随后退避重连。

use verba_trigger::capture::{
    capture_primary_screen, capture_primary_screen_png, capture_region, capture_region_png,
};
use verba_trigger::daemon::connect_daemon;
use verba_trigger::float::{run_float_button, FloatArgs, FloatOutcome};
use verba_trigger::play::play_audio;
use verba_trigger::record::record_seconds;
use verba_trigger::selection::select_region;
use verba_trigger::TriggerError;

fn main() {
    // 诊断日志（stderr）：选区 UI 的 winit 全屏/装饰问题排查（issue #83）。
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    std::panic::set_hook(Box::new(|info| {
        eprintln!("[verba-trigger] PANIC: {info}");
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None | Some("--help") | Some("-h") | Some("help") => {
            print_help();
            0
        }
        Some("--version") | Some("-V") => {
            println!("verba-trigger {}", verba_core::VERSION);
            0
        }
        Some("shot") => cmd_shot(&args),
        Some("region-shot") => cmd_region_shot(&args),
        Some("region-ocr") => cmd_region_ocr(&args),
        Some("vision-shot") => cmd_vision_shot(&args),
        Some("ocr") => cmd_ocr(&args),
        Some("mic") => cmd_mic(&args),
        Some("asr") => cmd_asr(&args),
        Some("tts") => cmd_tts(&args),
        Some("speak") => cmd_speak(&args),
        Some("float-button") => cmd_float_button(&args),
        Some("float-run") => cmd_float_run(),
        Some(other) => {
            eprintln!("未知命令: {other}（--help 查看用法）");
            1
        }
    };
    std::process::exit(code);
}

fn print_help() {
    println!(
        "Verba 触发工具（跨平台）\n\
         用法:\n  \
         verba-trigger shot [输出.bmp]        截取主屏全屏为 BMP\n  \
         verba-trigger region-shot [--rect x,y,w,h] [输出.bmp]  选区截图（交互拖选；--rect 脚本化）\n  \
         verba-trigger region-ocr [--rect x,y,w,h] [输出.txt]   选区 → daemon OCR\n  \
         verba-trigger vision-shot [--rect x,y,w,h] 截屏 → PNG 写 stdout（多模态 LLM 输入）\n  \
         verba-trigger ocr [输出.txt]         截图 → daemon OCR → 打印/写文件\n  \
         verba-trigger mic [秒=3] [输出.wav]   录制麦克风为 WAV\n  \
         verba-trigger asr [秒=3]             录音 → daemon ASR → 打印\n  \
         verba-trigger tts <文本> [输出.mp3] [语音]  TTS 合成存文件\n  \
         verba-trigger speak <文本> [语音]      TTS 合成并播放\n  \
         verba-trigger float-button --at x,y [--session-id N] [--session-key S]\n  \
         \x20 悬浮 AI 回复按钮（v1 窗模式；点击后截前台窗口→OCR→LLM 生成回复，文本写 stdout）\n  \
         verba-trigger float-run                 截前台窗口 → daemon OCR → 文本写 stdout（v2 无头，LLM 在 IME 进程内）\n  \
         verba-trigger --version              版本\n"
    );
}

fn cmd_shot(args: &[String]) -> i32 {
    let out = args.get(1).cloned().unwrap_or_else(|| "shot.bmp".into());
    match capture_primary_screen() {
        Ok(shot) => {
            if let Err(e) = std::fs::write(&out, &shot.bmp) {
                eprintln!("写文件失败 {out}: {e}");
                return 1;
            }
            println!(
                "已截图 {out}（{}x{} bmp={} bytes）",
                shot.width,
                shot.height,
                shot.bmp.len()
            );
            0
        }
        Err(e) => {
            eprintln!("截图失败: {e}");
            1
        }
    }
}

fn cmd_ocr(args: &[String]) -> i32 {
    let out = args.get(1).cloned();
    let shot = match capture_primary_screen() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("截图失败: {e}");
            return 1;
        }
    };
    let mut client = match connect_daemon() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("连接 daemon 失败: {e}");
            return 1;
        }
    };
    match client.ocr_recognize(&shot.bmp) {
        Ok(text) => {
            println!("{text}");
            if let Some(path) = out {
                if let Err(e) = std::fs::write(&path, &text) {
                    eprintln!("写文件失败 {path}: {e}");
                    return 1;
                }
            }
            0
        }
        Err(e) => {
            eprintln!("OCR 失败: {e}");
            1
        }
    }
}

fn cmd_mic(args: &[String]) -> i32 {
    let secs: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let out = args.get(2).cloned().unwrap_or_else(|| "rec.wav".into());
    match record_seconds(secs) {
        Ok(wav) => {
            if let Err(e) = std::fs::write(&out, &wav) {
                eprintln!("写文件失败 {out}: {e}");
                return 1;
            }
            println!("已录音 {out}（{secs}s wav={} bytes）", wav.len());
            0
        }
        Err(e) => {
            eprintln!("录音失败: {e}");
            1
        }
    }
}

fn cmd_asr(args: &[String]) -> i32 {
    let secs: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let wav = match record_seconds(secs) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("录音失败: {e}");
            return 1;
        }
    };
    let mut client = match connect_daemon() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("连接 daemon 失败: {e}");
            return 1;
        }
    };
    match client.asr_transcribe(&wav) {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("ASR 失败: {e}");
            1
        }
    }
}

fn cmd_tts(args: &[String]) -> i32 {
    let text = args.get(1).cloned().unwrap_or_default();
    if text.is_empty() {
        eprintln!("用法: verba-trigger tts <文本> [输出文件] [语音]");
        return 1;
    }
    let out = args.get(2).cloned().unwrap_or_else(|| "tts.mp3".into());
    let voice = args.get(3).cloned();
    let mut client = match connect_daemon() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("连接 daemon 失败: {e}");
            return 1;
        }
    };
    match client.tts_synthesize(&text, voice.as_deref()) {
        Ok((format, bytes)) => {
            if let Err(e) = std::fs::write(&out, &bytes) {
                eprintln!("写文件失败 {out}: {e}");
                return 1;
            }
            println!("已合成 {out}（format={format} bytes={}）", bytes.len());
            0
        }
        Err(e) => {
            eprintln!("TTS 失败: {e}");
            1
        }
    }
}

fn cmd_speak(args: &[String]) -> i32 {
    let text = args.get(1).cloned().unwrap_or_default();
    if text.is_empty() {
        eprintln!("用法: verba-trigger speak <文本> [语音]");
        return 1;
    }
    let voice = args.get(2).cloned();
    let mut client = match connect_daemon() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("连接 daemon 失败: {e}");
            return 1;
        }
    };
    let (format, bytes) = match client.tts_synthesize(&text, voice.as_deref()) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("TTS 失败: {e}");
            return 1;
        }
    };
    match play_audio(&bytes) {
        Ok(()) => {
            println!("已播放（format={format} bytes={}）", bytes.len());
            0
        }
        Err(e) => {
            eprintln!("播放失败: {e}");
            1
        }
    }
}

/// `float-button --at x,y [--session-id N] [--session-key S]`：悬浮 AI 回复
/// 按钮（winit 非激活小窗，等点击）；点击后截前台窗口 → OCR → LLM 生成
/// 回复写 stdout、退出 0。取消（右键/失活被 kill）→ stdout 空、退出 0；
/// 管线失败/空回复（`FloatOutcome::Failed`）→ stderr 带原因 + 退出 1
/// （前端捕获 stderr 记日志，首跑缺屏幕录制权限不再无声消失）；超时 →
/// 退出 3。前端按「stdout 非空 = 有结果」消费（与 region-ocr 同一契约）。
///
/// ⚠ v1 窗模式：macOS 已改进程内 Panel（v2，见 float-run），保留本子命令
/// 供 Windows TSF v1 接线使用；Windows v2 跟进后下线。
fn cmd_float_button(args: &[String]) -> i32 {
    let Some(at) = parse_pair(args, "--at") else {
        eprintln!("用法: verba-trigger float-button --at x,y [--session-id N] [--session-key S]");
        return 1;
    };
    let session_id = args
        .iter()
        .position(|a| a == "--session-id")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let session_key = args
        .iter()
        .position(|a| a == "--session-key")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    match run_float_button(FloatArgs {
        at,
        session_id,
        session_key,
    }) {
        Ok(FloatOutcome::Reply(text)) => {
            println!("{text}");
            0
        }
        Ok(FloatOutcome::Cancelled) => 0,
        Ok(FloatOutcome::Failed(msg)) => {
            eprintln!("悬浮按钮管线失败: {msg}");
            1
        }
        Ok(FloatOutcome::Expired) => {
            eprintln!("悬浮按钮超时（等待点击或管线执行超过上限）");
            3
        }
        Err(e) => {
            eprintln!("悬浮按钮失败: {e}");
            1
        }
    }
}

/// `float-run`：v2 无头管道——截前台窗口 → daemon OCR → 识别文本写
/// stdout、退出 0（LLM 生成在 IME 进程内做，helper 只出 OCR 文本）。
/// 失败（无屏录权限/前台窗无文字/OCR/daemon 异常）→ stderr 带原因 +
/// 退出 1。由 IME 在用户点击气泡或 `//`+TAB 时 spawn（点击频率级，
/// 非激活频率级——v1 的 per-activation spawn 在飞书/终端类客户端引发
/// IME 会话自激抖动，见 walgit verba-float-button-d2-session-churn）。
fn cmd_float_run() -> i32 {
    match verba_trigger::float::run_capture_ocr() {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("float-run 失败: {e}");
            1
        }
    }
}

/// 解析 `--at x,y`（两个整数），未提供或非法返回 None。
fn parse_pair(args: &[String], flag: &str) -> Option<(i32, i32)> {
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        if a == flag {
            if let Some(s) = it.next() {
                let parts: Vec<i32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                if parts.len() == 2 {
                    return Some((parts[0], parts[1]));
                }
            }
        }
    }
    None
}

/// 解析 --rect x,y,w,h（全局坐标），未提供返回 None。
fn parse_rect(args: &[String]) -> Option<(i32, i32, i32, i32)> {
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        if a == "--rect" {
            if let Some(s) = it.next() {
                let parts: Vec<i32> = s.split(",").filter_map(|p| p.trim().parse().ok()).collect();
                if parts.len() == 4 {
                    return Some((parts[0], parts[1], parts[2], parts[3]));
                }
            }
        }
    }
    None
}

/// 取输出路径（跳过 --rect 及其值）。
fn region_output_path(args: &[String]) -> Option<String> {
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        if a == "--rect" {
            it.next();
        } else if !a.starts_with("--") {
            return Some(a.clone());
        }
    }
    None
}

/// 选区截图：--rect 脚本化，否则交互拖选（Esc/右键取消 → Ok(None)）。
/// 显式传了 --rect 但解析失败 → 报错退出（脚本化场景静默落交互会挂死管道）。
fn region_capture(args: &[String]) -> Result<Option<verba_trigger::bmp::ScreenShot>, TriggerError> {
    let rect_given = args.iter().any(|a| a == "--rect");
    match parse_rect(args) {
        Some((x, y, w, h)) => capture_region(x, y, w, h).map(Some),
        None if rect_given => Err(TriggerError::Capture(
            "--rect 参数非法（应为 x,y,w,h 四个整数）".into(),
        )),
        None => match select_region()? {
            Some(r) => capture_region(r.x, r.y, r.width, r.height).map(Some),
            None => Ok(None),
        },
    }
}

/// `region-shot [--rect x,y,w,h] [输出.bmp]`：选区截图存 BMP。
fn cmd_region_shot(args: &[String]) -> i32 {
    let out = region_output_path(args).unwrap_or_else(|| "region.bmp".into());
    match region_capture(args) {
        Ok(Some(shot)) => {
            if let Err(e) = std::fs::write(&out, &shot.bmp) {
                eprintln!("写文件失败 {out}: {e}");
                return 1;
            }
            println!(
                "已截图 {out}（{}x{} bmp={} bytes）",
                shot.width,
                shot.height,
                shot.bmp.len()
            );
            0
        }
        Ok(None) => {
            eprintln!("已取消");
            0
        }
        Err(e) => {
            eprintln!("截图失败: {e}");
            1
        }
    }
}

/// `vision-shot [--rect x,y,w,h]`：截图 → PNG 字节写 stdout，供多模态 LLM
/// 的 image_url 输入。截图与编码复用 verba-trigger 共享实现；前端不重复。
fn vision_shot_help_requested(args: &[String]) -> bool {
    args.iter().any(|a| a == "-h" || a == "--help")
}

fn cmd_vision_shot(args: &[String]) -> i32 {
    if vision_shot_help_requested(args) {
        println!(
            "用法: verba-trigger vision-shot [--rect x,y,w,h]\n  截屏 → PNG 写 stdout（多模态 LLM 输入）"
        );
        return 0;
    }
    let rect_given = args.iter().any(|a| a == "--rect");
    let png = match parse_rect(args) {
        Some((x, y, w, h)) => capture_region_png(x, y, w, h),
        None if rect_given => {
            eprintln!("--rect 参数非法（应为 x,y,w,h 四个整数）");
            return 1;
        }
        None => capture_primary_screen_png(),
    };
    match png {
        Ok(bytes) => {
            use std::io::Write;
            if let Err(e) = std::io::stdout().write_all(&bytes) {
                eprintln!("写 stdout 失败: {e}");
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!("vision 截图失败: {e}");
            1
        }
    }
}

/// `region-ocr [--rect x,y,w,h] [输出.txt]`：选区截图 → daemon OCR。
fn cmd_region_ocr(args: &[String]) -> i32 {
    let out = region_output_path(args);
    let shot = match region_capture(args) {
        Ok(Some(s)) => s,
        Ok(None) => {
            eprintln!("已取消");
            return 0;
        }
        Err(e) => {
            eprintln!("截图失败: {e}");
            return 1;
        }
    };
    let mut client = match connect_daemon() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("连接 daemon 失败: {e}");
            return 1;
        }
    };
    match client.ocr_recognize(&shot.bmp) {
        Ok(text) => {
            println!("{text}");
            if let Some(path) = out {
                if let Err(e) = std::fs::write(&path, &text) {
                    eprintln!("写文件失败 {path}: {e}");
                    return 1;
                }
            }
            0
        }
        Err(e) => {
            eprintln!("OCR 失败: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rect_accepts_and_rejects() {
        let ok = vec!["vision-shot".into(), "--rect".into(), "1,2,3,4".into()];
        assert_eq!(parse_rect(&ok), Some((1, 2, 3, 4)));
        let bad = vec!["vision-shot".into(), "--rect".into(), "bad".into()];
        assert!(parse_rect(&bad).is_none());
    }

    #[test]
    fn parse_at_accepts_and_rejects() {
        let ok = vec!["float-button".into(), "--at".into(), "10,-20".into()];
        assert_eq!(parse_pair(&ok, "--at"), Some((10, -20)));
        let bad = vec!["float-button".into(), "--at".into(), "1,2,3".into()];
        assert!(parse_pair(&bad, "--at").is_none());
        let missing = vec!["float-button".into()];
        assert!(parse_pair(&missing, "--at").is_none());
    }

    #[test]
    fn vision_shot_help_is_detected() {
        assert!(vision_shot_help_requested(&[
            "vision-shot".into(),
            "--help".into()
        ]));
        assert!(vision_shot_help_requested(&[
            "vision-shot".into(),
            "-h".into()
        ]));
        assert!(!vision_shot_help_requested(&["vision-shot".into()]));
    }
}
