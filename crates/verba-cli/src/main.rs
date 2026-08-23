//! Verba CLI：调试入口，直接驱动 daemon（等价于一个模拟前端）。
//!
//! 用法见 `--help`。

#![forbid(unsafe_code)]

use verba_ipc::{IpcError, VerbaClient};
use verba_protos::stream_event;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None | Some("--help") | Some("-h") => {
            print_help();
            0
        }
        Some("--version") | Some("-V") => {
            println!("verba-cli {}", verba_core::VERSION);
            0
        }
        Some("daemon") => run_daemon(),
        Some("ping") => with_client(|c| {
            let version = c.ping()?;
            println!("pong daemon v{version}");
            Ok(())
        }),
        Some("ai") => cmd_ai(&args),
        Some("candidates") => cmd_candidates(&args),
        Some("rime") => cmd_rime(&args),
        Some("tts") => cmd_tts(&args),
        Some("ocr") => cmd_ocr(&args),
        Some("asr") => cmd_asr(&args),
        Some("config") => cmd_config(&args),
        Some("mode") => cmd_mode(&args),
        Some("pinyin") => cmd_pinyin(&args),
        Some(other) => {
            eprintln!("未知命令: {other}（--help 查看用法）");
            1
        }
    };
    std::process::exit(code);
}

fn print_help() {
    println!(
        "Verba · 拾言输入法 CLI（调试工具）\n\
         用法:\n  \
         verba-cli daemon                前台运行后台守护进程\n  \
         verba-cli ping                  健康检查\n  \
         verba-cli ai <prompt>           流式调用 LLM 并打印（模拟 // AI 模式）\n  \\
         verba-cli candidates <拼音>    请求 LLM 融合候选并打印\n  \
         verba-cli rime <输入> [方案]  查询 Rime 引擎候选（需 config engine=rime）\n  \
         verba-cli tts <文本> [输出] [语音]  TTS 合成音频并写文件（config tts_provider）\n  \
         verba-cli ocr <图像>          OCR 识别图像并打印文字（config ocr_provider）\n  \
         verba-cli asr <音频>          ASR 转写音频并打印文字（config asr_provider）\n  \
         verba-cli config                查看配置\n  \
         verba-cli config set <k=v>...   修改配置\n  \
         verba-cli mode <normal|ai|...>  切换模式\n  \
         verba-cli pinyin <拼音>        查询拼音候选（本地引擎调试）\n  \
         verba-cli --version             版本\n"
    );
}

fn run_daemon() -> i32 {
    match verba_daemon::run_default() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("daemon 退出: {e}");
            1
        }
    }
}

fn with_client<F>(f: F) -> i32
where
    F: FnOnce(&mut VerbaClient) -> Result<(), IpcError>,
{
    match VerbaClient::connect() {
        Ok(mut client) => match f(&mut client) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("命令失败: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("连接 daemon 失败（先运行 `verba-cli daemon`）: {e}");
            1
        }
    }
}

fn cmd_ai(args: &[String]) -> i32 {
    let prompt = args.get(1).cloned().unwrap_or_default();
    if prompt.is_empty() {
        eprintln!("用法: verba-cli ai <prompt>");
        return 1;
    }
    with_client(|c| {
        let id = c.llm_start(&prompt, None, None, None)?;
        loop {
            let evt = c.next_event(id)?;
            match evt.kind {
                Some(stream_event::Kind::Chunk(ch)) => {
                    print!("{}", ch.text);
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
                Some(stream_event::Kind::Final(_)) => {
                    println!();
                    break;
                }
                Some(stream_event::Kind::Error(e)) => {
                    return Err(IpcError::Server {
                        code: e.code,
                        message: e.message,
                    });
                }
                _ => {}
            }
        }
        Ok(())
    })
}

/// `verba-cli candidates <拼音>`：请求 LLM 为拼音补充候选（模拟候选融合链路）。
fn cmd_candidates(args: &[String]) -> i32 {
    let pinyin = args.get(1).cloned().unwrap_or_default();
    if pinyin.is_empty() {
        eprintln!("用法: verba-cli candidates <拼音>");
        return 1;
    }
    with_client(|c| {
        let id = c.llm_candidates_start(&pinyin, &[], 6)?;
        loop {
            let evt = c.next_event(id)?;
            match evt.kind {
                Some(stream_event::Kind::Candidates(cands)) => {
                    for cand in &cands.candidates {
                        println!("候选({}): {cand}", cands.pinyin);
                    }
                    if cands.done {
                        break;
                    }
                }
                Some(stream_event::Kind::Error(e)) => {
                    return Err(IpcError::Server {
                        code: e.code,
                        message: e.message,
                    });
                }
                _ => {}
            }
        }
        Ok(())
    })
}

/// `verba-cli rime <输入> [方案]`：查询 daemon 内 Rime 引擎候选（需 engine=rime）。
fn cmd_rime(args: &[String]) -> i32 {
    let input = args.get(1).cloned().unwrap_or_default();
    if input.is_empty() {
        eprintln!("用法: verba-cli rime <输入> [方案]");
        return 1;
    }
    let schema = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "luna_pinyin_simp".into());
    with_client(|c| {
        let cands = c.rime_candidates(&input, &schema, 9)?;
        if cands.is_empty() {
            println!("（无候选）");
        } else {
            for (i, c) in cands.iter().enumerate() {
                println!("{}. {c}", i + 1);
            }
        }
        Ok(())
    })
}

/// `verba-cli tts <文本> [输出文件] [语音]`：daemon TTS 合成（provider 由 config tts_provider 决定）。
fn cmd_tts(args: &[String]) -> i32 {
    let text = args.get(1).cloned().unwrap_or_default();
    if text.is_empty() {
        eprintln!("用法: verba-cli tts <文本> [输出文件] [语音]");
        return 1;
    }
    let out = args.get(2).cloned();
    let voice = args.get(3).cloned();
    with_client(|c| {
        let (format, data) = c.tts_synthesize(&text, voice.as_deref())?;
        match out {
            Some(path) => {
                std::fs::write(&path, &data)?;
                println!("已写入 {path}（format={format} bytes={}）", data.len());
            }
            None => {
                println!("format={format} bytes={}", data.len());
            }
        }
        Ok(())
    })
}

/// `verba-cli ocr <图像>`：daemon OCR 识别（provider 由 config ocr_provider 决定）。
fn cmd_ocr(args: &[String]) -> i32 {
    let path = args.get(1).cloned().unwrap_or_default();
    if path.is_empty() {
        eprintln!("用法: verba-cli ocr <图像>");
        return 1;
    }
    let image = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("读取图像失败 {path}: {e}");
            return 1;
        }
    };
    with_client(|c| {
        let text = c.ocr_recognize(&image)?;
        println!("{text}");
        Ok(())
    })
}

/// `verba-cli asr <音频>`：daemon ASR 转写（provider 由 config asr_provider 决定）。
fn cmd_asr(args: &[String]) -> i32 {
    let path = args.get(1).cloned().unwrap_or_default();
    if path.is_empty() {
        eprintln!("用法: verba-cli asr <音频>");
        return 1;
    }
    let audio = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("读取音频失败 {path}: {e}");
            return 1;
        }
    };
    with_client(|c| {
        let text = c.asr_transcribe(&audio)?;
        println!("{text}");
        Ok(())
    })
}

fn cmd_config(args: &[String]) -> i32 {
    match args.get(1).map(String::as_str) {
        None => with_client(|c| {
            let map = c.get_config()?;
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            for k in keys {
                println!("{k} = {}", map[&k]);
            }
            Ok(())
        }),
        Some("set") => {
            let mut values = std::collections::HashMap::new();
            for kv in &args[2..] {
                let Some((k, v)) = kv.split_once('=') else {
                    eprintln!("参数格式应为 k=v: {kv}");
                    return 1;
                };
                values.insert(k.to_owned(), v.to_owned());
            }
            if values.is_empty() {
                eprintln!("用法: verba-cli config set <k=v>...");
                return 1;
            }
            with_client(|c| {
                c.set_config(values)?;
                println!("配置已更新");
                Ok(())
            })
        }
        Some(other) => {
            eprintln!("未知 config 子命令: {other}");
            1
        }
    }
}

fn cmd_mode(args: &[String]) -> i32 {
    let mode = args.get(1).cloned().unwrap_or_default();
    if mode.is_empty() {
        eprintln!("用法: verba-cli mode <normal|ai|voice|ocr>");
        return 1;
    }
    with_client(|c| {
        c.set_mode(&mode)?;
        println!("模式: {mode}");
        Ok(())
    })
}

/// `verba-cli pinyin <拼音>`：本地拼音引擎候选查询（不依赖 daemon）。
fn cmd_pinyin(args: &[String]) -> i32 {
    let input = args.get(1).map(String::as_str).unwrap_or("");
    let engine = verba_pinyin::PinyinEngine::new();
    let cands = engine.lookup(input);
    if cands.is_empty() {
        println!("（无候选）");
    } else {
        for (i, c) in cands.iter().enumerate() {
            println!("{}. {} ({:?})", i + 1, c.text, c.kind);
        }
    }
    0
}
