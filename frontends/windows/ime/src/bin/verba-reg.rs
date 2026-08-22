//! verba-reg：Verba Windows 输入法注册/注销工具。
//! 用法: verba-reg register [dll路径] | unregister

use std::path::PathBuf;

use verba_ime_windows::reg;

fn dll_default() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dll = exe.with_file_name("verba_ime_windows.dll");
    dll.exists().then_some(dll)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("register") => {
            let dll = args.get(2).map(PathBuf::from).or_else(dll_default);
            match dll {
                Some(path) => match reg::register_with_path(&path.to_string_lossy()) {
                    Ok(()) => println!("已注册 Verba 输入法（DLL: {}）", path.display()),
                    Err(e) => {
                        eprintln!("注册失败: {e}");
                        std::process::exit(1);
                    }
                },
                None => {
                    eprintln!(
                        "找不到 verba_ime_windows.dll，请指定路径: verba-reg register <dll路径>"
                    );
                    std::process::exit(1);
                }
            }
        }
        Some("unregister") => match reg::unregister() {
            Ok(()) => println!("已注销 Verba 输入法"),
            Err(e) => {
                eprintln!("注销失败: {e}");
                std::process::exit(1);
            }
        },
        _ => {
            println!(
                "Verba Windows 输入法注册工具\n\
                 用法:\n  \
                 verba-reg register [dll路径]   注册（默认同目录 DLL）\n  \
                 verba-reg unregister           注销"
            );
        }
    }
}
