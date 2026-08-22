//! 极简文件日志（DLL 内无 logger 时 log::info!/warn! 会被丢弃，这里落盘便于真机排查）。
//! 日志文件：%LOCALAPPDATA%\Verba\verba-ime.log

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};

struct FileLogger {
    file: Mutex<File>,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let line = format!("[{} {}] {}\n", chrono_now(), record.level(), record.args());
            if let Ok(mut f) = self.file.lock() {
                let _ = f.write_all(line.as_bytes());
                let _ = f.flush();
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
    }
}

/// 简易时间戳（避免引入 chrono 依赖）。
fn chrono_now() -> String {
    // 用系统时间：SystemTime → 秒级
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}s")
}

/// 初始化文件日志（重复调用幂等）。
pub fn init() {
    use std::sync::OnceLock;
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let path = log_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
            let logger = FileLogger {
                file: Mutex::new(file),
            };
            let _ = log::set_boxed_logger(Box::new(logger));
            log::set_max_level(LevelFilter::Info);
            log::info!("==== Verba IME DLL logger 初始化 ====");
        }
    });
}

/// 日志文件路径：%LOCALAPPDATA%\Verba\verba-ime.log
fn log_path() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(base)
        .join("Verba")
        .join("verba-ime.log")
}
