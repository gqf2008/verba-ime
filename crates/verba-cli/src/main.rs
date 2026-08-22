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
        Some("config") => cmd_config(&args),
        Some("mode") => cmd_mode(&args),
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
         verba-cli ai <prompt>           流式调用 LLM 并打印（模拟 // AI 模式）\n  \
         verba-cli config                查看配置\n  \
         verba-cli config set <k=v>...   修改配置\n  \
         verba-cli mode <normal|ai|...>  切换模式\n  \
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
                None => {}
            }
        }
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
