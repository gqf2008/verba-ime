//! Verba 触发工具（跨平台，issue #82）：选区截图→OCR、录音→ASR、TTS→合成/播放。
//!
//! 自 v0.2.4 的 Windows 专版（frontends/windows/ime/src/bin/verba-trigger.rs）
//! 迁为共享 crate bin：Windows/macOS/Linux 同名同参，前端各自 spawn 本进程
//! （TSF DLL / IMK 进程内不承载选区 UI 与事件循环）。
//!
//! daemon 连接：验活握手失败时拉起**同目录**的 verba-daemon（安装布局两端
//! 一致：{app} 目录 / Verba.app/Contents/MacOS），Windows 加 CREATE_NO_WINDOW
//! 防控制台闪窗，随后退避重连。

use std::process::Command;
use std::time::Duration;

use verba_ipc::VerbaClient;
use verba_trigger::capture::{capture_primary_screen, capture_region};
use verba_trigger::play::play_audio;
use verba_trigger::record::record_seconds;
use verba_trigger::selection::select_region;
use verba_trigger::TriggerError;

fn main() {
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
        Some("ocr") => cmd_ocr(&args),
        Some("mic") => cmd_mic(&args),
        Some("asr") => cmd_asr(&args),
        Some("tts") => cmd_tts(&args),
        Some("speak") => cmd_speak(&args),
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
         verba-trigger ocr [输出.txt]         截图 → daemon OCR → 打印/写文件\n  \
         verba-trigger mic [秒=3] [输出.wav]   录制麦克风为 WAV\n  \
         verba-trigger asr [秒=3]             录音 → daemon ASR → 打印\n  \
         verba-trigger tts <文本> [输出.mp3] [语音]  TTS 合成存文件\n  \
         verba-trigger speak <文本> [语音]      TTS 合成并播放\n  \
         verba-trigger --version              版本\n"
    );
}

/// 连接 daemon；不在运行则拉起同目录 verba-daemon 并退避重连。
/// （替代原 verba_ime_windows::ipc::ensure_daemon 的跨平台版。）
fn connect_daemon() -> Result<VerbaClient, TriggerError> {
    if let Ok(c) = VerbaClient::connect_verified() {
        return Ok(c);
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_owned()))
        .ok_or_else(|| TriggerError::Daemon("无法定位自身目录".into()))?;
    let daemon = exe_dir.join(if cfg!(windows) {
        "verba-daemon.exe"
    } else {
        "verba-daemon"
    });
    if !daemon.is_file() {
        return Err(TriggerError::Daemon(format!(
            "未找到 daemon（{}），无法自动拉起",
            daemon.display()
        )));
    }
    let mut cmd = Command::new(&daemon);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：daemon 为控制台子系统构建（debug），防闪窗
        cmd.creation_flags(0x08000000);
    }
    cmd.spawn()
        .map_err(|e| TriggerError::Daemon(format!("拉起 daemon 失败: {e}")))?;
    // 退避重连：daemon 启动 + 首次部署预热期间 socket 就绪需要时间
    for attempt in 0..20 {
        std::thread::sleep(Duration::from_millis(if attempt < 10 { 150 } else { 400 }));
        if let Ok(c) = VerbaClient::connect_verified() {
            return Ok(c);
        }
    }
    Err(TriggerError::Daemon("daemon 拉起后连接失败".into()))
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
fn region_capture(args: &[String]) -> Result<Option<verba_trigger::bmp::ScreenShot>, TriggerError> {
    match parse_rect(args) {
        Some((x, y, w, h)) => capture_region(x, y, w, h).map(Some),
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
