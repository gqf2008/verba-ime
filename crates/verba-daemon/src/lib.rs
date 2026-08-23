//! Verba 后台守护进程。

#![forbid(unsafe_code)]

pub mod handler;

pub use handler::DaemonHandler;

use std::sync::Arc;

use verba_ai::{LlmClient, LlmConfig};
use verba_config::{ApiKeyStore, ConfigManager, VerbaDirs};
use verba_ipc::DEFAULT_SOCKET_NAME;

/// 前台运行 daemon（阻塞直到退出）。
pub fn run(socket_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let dirs = VerbaDirs::locate()?;
    dirs.ensure()?;
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
    // engine=rime 时后台预热 Rime（触发首次部署），首次候选查询免等待。
    if config.engine == "rime" {
        let h = Arc::clone(&handler);
        runtime.spawn(async move {
            h.warmup_rime();
        });
    }
    Ok(runtime.block_on(verba_ipc::server::serve(socket_name, handler))?)
}

/// 使用默认套接字名运行。
pub fn run_default() -> Result<(), Box<dyn std::error::Error>> {
    run(DEFAULT_SOCKET_NAME)
}
