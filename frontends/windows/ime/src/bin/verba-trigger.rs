//! Verba 触发工具（Windows）：截图→OCR、录音→ASR、TTS→合成/播放。
//!
//! 能力模块（capture / record / play）供后续 TSF 热键接线复用；
//! 本工具用于手动验证与脚本化冒烟。

use verba_ime_windows::capture::capture_primary_screen;
use verba_ime_windows::play::play_audio;
use verba_ime_windows::record::record_seconds;

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
        "Verba 触发工具（Windows）\n\
         用法:\n  \
         verba-trigger shot [输出.bmp]        截取主屏全屏为 BMP\n  \
         verba-trigger ocr [输出.txt]         截图 → daemon OCR → 打印/写文件\n  \
         verba-trigger mic [秒=3] [输出.wav]   录制麦克风为 WAV\n  \
         verba-trigger asr [秒=3]             录音 → daemon ASR → 打印\n  \
         verba-trigger tts <文本> [输出.mp3] [语音]  TTS 合成存文件\n  \
         verba-trigger speak <文本> [语音]      TTS 合成并播放\n  \
         verba-trigger --version              版本\n"
    );
}

fn connect_daemon() -> Result<verba_ipc::VerbaClient, verba_ime_windows::TriggerError> {
    verba_ime_windows::ipc::ensure_daemon()
        .map_err(|e| verba_ime_windows::TriggerError::Daemon(e.to_string()))
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
