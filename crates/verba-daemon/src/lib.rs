//! Verba 后台守护进程。

#![forbid(unsafe_code)]

pub mod handler;

pub use handler::DaemonHandler;

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Arc, Mutex};

use verba_ai::{LlmClient, LlmConfig};
use verba_config::{ApiKeyStore, ConfigManager, VerbaDirs};

/// 日志 tee：同时写 stderr 与文件（便于故障诊断）。
struct TeeLog {
    file: Mutex<std::fs::File>,
}

impl Write for TeeLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write(buf);
        let mut f = self.file.lock().unwrap();
        let n = f.write(buf)?;
        let _ = f.flush();
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

/// 前台运行 daemon（阻塞直到退出）。
pub fn run(socket_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let dirs = VerbaDirs::locate()?;
    dirs.ensure()?;
    // IPC 信任边界（架构审查 P0-1）：Unix 下 socket 位于用户数据目录，
    // 目录收紧为 0700，其他用户不可见不可连（跨用户劫持/DoS 面关闭）。
    #[cfg(unix)]
    std::fs::set_permissions(
        dirs.data_dir(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )?;
    let log_path = dirs.log_dir().join("verba-daemon.log");
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(TeeLog {
            file: Mutex::new(log_file),
        })))
        .init();
    let mgr = ConfigManager::new(dirs);
    let config = mgr.load()?;

    let api_key = ApiKeyStore::get()?;
    let llm_config = LlmConfig::new(
        config.llm_base_url.clone(),
        api_key,
        config.llm_model.clone(),
    );
    let llm_config = LlmConfig {
        temperature: config.temperature,
        max_tokens: config.max_tokens,
        ..llm_config
    };

    let handler = Arc::new(DaemonHandler::new(
        mgr,
        config.clone(),
        llm_config,
        LlmClient::new()?,
    ));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    // 单引擎（Rime）：后台预热 Rime（触发首次部署），首次候选查询免等待。
    let h = Arc::clone(&handler);
    runtime.spawn(async move {
        h.warmup_rime();
    });
    Ok(runtime.block_on(verba_ipc::server::serve(socket_name, handler))?)
}

/// 使用默认套接字（Unix 用户数据目录全路径 / Windows per-user 管道）运行。
pub fn run_default() -> Result<(), Box<dyn std::error::Error>> {
    run(&verba_ipc::default_socket_spec())
}
