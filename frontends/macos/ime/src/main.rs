//! `verba-mac`：macOS 前端冒烟入口（CI 验证连接 daemon）。

fn main() {
    let code = match verba_ime_macos::MacIme::connect() {
        Ok(mut ime) => match ime.ping() {
            Ok(v) => {
                println!("mac 前端就绪: daemon v{v}");
                0
            }
            Err(e) => {
                eprintln!("ping daemon 失败: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("连接 daemon 失败: {e}");
            1
        }
    };
    std::process::exit(code);
}
