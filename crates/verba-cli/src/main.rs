//! Verba CLI：调试入口，直接驱动核心引擎（等价于一个模拟前端）。
//!
//! M0 阶段仅提供版本与模式查询，后续扩展为「命令驱动 core」的完整调试工具。

#![forbid(unsafe_code)]

use verba_core::{Mode, VERSION};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => println!("verba-cli {VERSION}"),
        Some("--mode") => {
            let mode = args
                .get(1)
                .map(|m| match m.as_str() {
                    "normal" => Mode::Normal,
                    "voice" => Mode::Voice,
                    "ocr" => Mode::Ocr,
                    "ai" => Mode::Ai,
                    other => {
                        eprintln!("未知模式: {other}");
                        std::process::exit(2);
                    }
                })
                .unwrap_or(Mode::Normal);
            println!("当前模式: {mode:?}");
        }
        Some("--help") | Some("-h") | None => {
            println!(
                "Verba · 拾言输入法 CLI（调试工具）\n\
                 用法:\n  \
                 verba-cli --version                      查看版本\n  \
                 verba-cli --mode <normal|voice|ocr|ai>  查看当前模式\n  \
                 verba-cli --help                         查看帮助\n"
            );
        }
        Some(other) => {
            eprintln!("未知参数: {other}（--help 查看用法）");
            std::process::exit(2);
        }
    }
}
