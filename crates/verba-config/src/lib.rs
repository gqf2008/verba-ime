//! Verba 配置与密钥管理。
//!
//! 配置文件（TOML）位于平台标准配置目录；API Key 走系统密钥库
//! （Windows DPAPI / macOS Keychain / Linux Secret Service），
//! 开发环境可用环境变量 `VERBA_API_KEY` 兜底。

#![forbid(unsafe_code)]

pub mod config;
pub mod dirs;

pub use config::{ApiKeyStore, Config, ConfigError, ConfigManager};
pub use dirs::VerbaDirs;
