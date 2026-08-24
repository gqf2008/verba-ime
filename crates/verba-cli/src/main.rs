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
        Some("key") => cmd_key(&args),
        Some("config") => cmd_config(&args),
        Some("mode") => cmd_mode(&args),
        Some("phrase") => cmd_phrase(&args),
        Some("diag") => cmd_diag(&args),
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
         verba-cli rime <输入> [方案]  查询 Rime 引擎候选（单引擎）\n  \
         verba-cli tts <文本> [输出] [语音]  TTS 合成音频并写文件（config tts_provider）\n  \
         verba-cli ocr <图像>          OCR 识别图像并打印文字（config ocr_provider）\n  \
         verba-cli asr <音频>          ASR 转写音频并打印文字（config asr_provider）\n  \
         verba-cli key [值]             查看/设置/清除 API Key（值省略查看；clear 清除）\n  \
         verba-cli config                查看配置\n  \
         verba-cli config set <k=v>...   修改配置\n  \
         verba-cli mode <normal|ai|...>  切换模式\n  \
         verba-cli phrase <名称>      查看快捷短语；phrase-set/list/del 管理\n  \
         verba-cli diag                 诊断：daemon 健康/配置/日志尾/相关进程\n  \
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
    let mut image: Option<(String, Vec<u8>)> = None;
    let mut prompt = String::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--image" => {
                let path = match args.get(i + 1) {
                    Some(p) => p.clone(),
                    None => {
                        eprintln!("--image 需要一个文件路径");
                        return 1;
                    }
                };
                i += 2;
                match std::fs::read(&path) {
                    Ok(d) => image = Some(("image/png".to_owned(), d)),
                    Err(e) => {
                        eprintln!("读取图像失败: {e}");
                        return 1;
                    }
                }
            }
            "--mime" => {
                let mime = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
                if let Some((_, d)) = &image {
                    image = Some((mime, d.clone()));
                }
            }
            _ => {
                if !prompt.is_empty() {
                    prompt.push(' ');
                }
                prompt.push_str(&args[i]);
                i += 1;
            }
        }
    }
    if prompt.is_empty() {
        eprintln!("用法: verba-cli ai [--image <文件>] <prompt>");
        return 1;
    }
    let image_ref = image.as_ref().map(|(m, d)| (m.as_str(), d.as_slice()));
    with_client(|c| {
        let id = c.llm_start(&prompt, None, None, None, image_ref)?;
        let mut any_chunk = false;
        loop {
            let evt = c.next_event(id)?;
            match evt.kind {
                Some(stream_event::Kind::Chunk(ch)) => {
                    any_chunk = true;
                    print!("{}", ch.text);
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
                Some(stream_event::Kind::Final(f)) => {
                    // 特别命令（重置/会话/上次OCR）无 chunk，绂结文本在 Final 里；有 chunk 时已打印，不重复。
                    if !any_chunk {
                        print!("{}", f.text);
                    }
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

/// `verba-cli rime <输入> [方案]`：查询 daemon 内 Rime 引擎候选（单引擎）。
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

/// `verba-cli key [值]`：查看/设置/清除 API Key。
/// - 无参：打印当前密钥状态（不泄露明文，掩码末 4 位）。
/// - `clear`：清除密钥（经 daemon 写密钥库 + 热更新内存）。
/// - 其它值：设置密钥（经 daemon，同设置面板路径）。
fn cmd_key(args: &[String]) -> i32 {
    match args.get(1).map(String::as_str) {
        None => match verba_config::ApiKeyStore::get() {
            Ok(Some(k)) if !k.is_empty() => {
                let tail: String = k
                    .chars()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                println!("已设置（…{tail}）");
                0
            }
            Ok(_) => {
                println!("未设置（可用 VERBA_API_KEY 环境变量）");
                0
            }
            Err(e) => {
                eprintln!("密钥读取失败: {e}");
                1
            }
        },
        Some("clear") => with_client(|c| {
            c.set_api_key("")?;
            println!("密钥已清除");
            Ok(())
        }),
        Some(value) => with_client(|c| {
            c.set_api_key(value)?;
            println!("密钥已设置");
            Ok(())
        }),
    }
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

/// `verba-cli diag`：输出 daemon 健康、关键配置、日志尾、相关进程，便于故障定位。
fn cmd_diag(_args: &[String]) -> i32 {
    println!("== Verba 诊断 ==");
    let dirs = match verba_config::VerbaDirs::locate() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("无法定位数据目录: {e}");
            return 1;
        }
    };
    println!("数据目录: {}", dirs.data_dir().display());

    match VerbaClient::connect() {
        Ok(mut c) => match c.ping() {
            Ok(ver) => {
                println!("daemon: 在线 v{ver}");
                if let Ok(cfg) = c.get_config() {
                    let get = |k: &str| cfg.get(k).cloned().unwrap_or_default();
                    for k in [
                        "llm_base_url",
                        "llm_model",
                        "llm_vision_model",
                        "rime_schema",
                        "ocr_provider",
                        "asr_provider",
                        "tts_provider",
                        "eye_enabled",
                        "eye_mode",
                    ] {
                        println!("{k} = {}", get(k));
                    }
                    if get("ocr_provider") == "rapid" {
                        let model_dir = dirs.data_dir().join("models-rapidocr");
                        let det = model_dir.join("ch_PP-OCRv5_det_mobile.onnx");
                        println!(
                            "rapid 模型目录: {} (det 就绪: {})",
                            model_dir.display(),
                            if det.exists() { "是" } else { "否" }
                        );
                    }
                }
            }
            Err(e) => println!("daemon: ping 失败（{e}）"),
        },
        Err(e) => println!("daemon: 未连接（{e}）（可先运行 `verba-cli daemon`）"),
    }

    print_log(&dirs.log_dir().join("verba-daemon.log"), "daemon 日志");
    let ilog = std::env::var("LOCALAPPDATA")
        .map(|p| std::path::PathBuf::from(format!("{p}\\Verba\\verba-ime.log")))
        .unwrap_or_default();
    print_log(&ilog, "输入法日志");
    print_procs();
    0
}

fn print_log(p: &std::path::Path, label: &str) {
    println!("== {label}: {} ==", p.display());
    match std::fs::read_to_string(p) {
        Ok(s) => {
            let lines: Vec<&str> = s.lines().collect();
            let start = lines.len().saturating_sub(40);
            for l in &lines[start..] {
                println!("{l}");
            }
        }
        Err(e) => println!("(无法读取: {e})"),
    }
}

fn print_procs() {
    println!("== 相关进程（verba/python） ==");
    #[cfg(windows)]
    {
        if let Ok(out) = std::process::Command::new("tasklist")
            .arg("/FO")
            .arg("CSV")
            .arg("/NH")
            .stdout(std::process::Stdio::piped())
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut found = false;
            for line in text.lines() {
                if line.to_lowercase().contains("verba")
                    || line.to_lowercase().contains("python.exe")
                {
                    println!("{line}");
                    found = true;
                }
            }
            if !found {
                println!("(未发现）");
            }
        }
    }
    #[cfg(not(windows))]
    {
        println!("(非 Windows，暂不列举进程)");
    }
}
/// `verba-cli phrase ...`：快捷短语管理（数据层，`//短语 <名称>` 由输入法插入）。
fn cmd_phrase(args: &[String]) -> i32 {
    let dirs = match verba_config::VerbaDirs::locate() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("定位数据目录失败: {e}");
            return 1;
        }
    };
    match args.get(1).map(String::as_str) {
        Some("set") => {
            let name = args.get(2).cloned().unwrap_or_default();
            if name.is_empty() {
                eprintln!("用法: verba-cli phrase-set <名称> <文本>");
                return 1;
            }
            let text = args[3..].join(" ");
            if text.is_empty() {
                eprintln!("文本为空（删除请用 phrase-del）");
                return 1;
            }
            match verba_config::phrases::set(&dirs, &name, &text) {
                Ok(()) => println!("已保存短语: {name}"),
                Err(e) => {
                    eprintln!("保存失败: {e}");
                    return 1;
                }
            }
        }
        Some("list") => match verba_config::phrases::load(&dirs) {
            Ok(map) => {
                if map.is_empty() {
                    println!("（无短语）");
                } else {
                    for (k, v) in &map {
                        println!("{k}: {v}");
                    }
                }
            }
            Err(e) => {
                eprintln!("读取失败: {e}");
                return 1;
            }
        },
        Some("del") => {
            let name = args.get(2).cloned().unwrap_or_default();
            if name.is_empty() {
                eprintln!("用法: verba-cli phrase-del <名称>");
                return 1;
            }
            match verba_config::phrases::set(&dirs, &name, "") {
                Ok(()) => println!("已删除短语: {name}"),
                Err(e) => {
                    eprintln!("删除失败: {e}");
                    return 1;
                }
            }
        }
        Some(name) => match verba_config::phrases::get(&dirs, name) {
            Ok(Some(text)) => println!("{text}"),
            Ok(None) => {
                println!("（未定义短语: {name}）");
            }
            Err(e) => {
                eprintln!("读取失败: {e}");
                return 1;
            }
        },
        None => {
            eprintln!("用法: verba-cli phrase <名称> | phrase-set <名称> <文本> | phrase-list | phrase-del <名称>");
            return 1;
        }
    }
    0
}
