//! 配置模型（TOML）与密钥库。

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::dirs::VerbaDirs;

const CONFIG_FILE: &str = "config.toml";
const KEYRING_SERVICE: &str = "verba";
const KEYRING_USER: &str = "llm_api_key";
const ENV_API_KEY: &str = "VERBA_API_KEY";

const DEFAULT_LLM_BASE_URL: &str = "https://api.deepseek.com/v1";
const DEFAULT_LLM_MODEL: &str = "deepseek-chat";
const DEFAULT_TEMPERATURE: f32 = 0.7;
const DEFAULT_MAX_TOKENS: i32 = 1024;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML 解析错误: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("TOML 序列化错误: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("密钥库错误: {0}")]
    Keyring(String),
    #[error("未知配置项: {0}")]
    UnknownKey(String),
    #[error("配置值非法: {0}")]
    InvalidValue(String),
}

/// 可持久化配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// OpenAI 兼容 API 基址（如 `https://api.deepseek.com/v1`）。
    #[serde(default = "default_llm_base_url")]
    pub llm_base_url: String,
    /// 模型名。
    #[serde(default = "default_llm_model")]
    pub llm_model: String,
    /// 采样温度。
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// 最大生成 token 数。
    #[serde(default = "default_max_tokens")]
    pub max_tokens: i32,
    /// AI 模式系统提示词（可覆盖默认）。
    #[serde(default)]
    pub ai_system_prompt: String,
}

fn default_llm_base_url() -> String {
    DEFAULT_LLM_BASE_URL.to_owned()
}
fn default_llm_model() -> String {
    DEFAULT_LLM_MODEL.to_owned()
}
fn default_temperature() -> f32 {
    DEFAULT_TEMPERATURE
}
fn default_max_tokens() -> i32 {
    DEFAULT_MAX_TOKENS
}

impl Default for Config {
    fn default() -> Self {
        Self {
            llm_base_url: default_llm_base_url(),
            llm_model: default_llm_model(),
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            ai_system_prompt: String::new(),
        }
    }
}

impl Config {
    /// 转成键值表（用于 IPC `Config` 消息）。
    pub fn to_map(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        map.insert("llm_base_url".into(), self.llm_base_url.clone());
        map.insert("llm_model".into(), self.llm_model.clone());
        map.insert("temperature".into(), self.temperature.to_string());
        map.insert("max_tokens".into(), self.max_tokens.to_string());
        map.insert("ai_system_prompt".into(), self.ai_system_prompt.clone());
        map
    }

    /// 应用键值表（非法键/值报错，不部分生效）。
    pub fn apply_map(&mut self, values: &HashMap<String, String>) -> Result<(), ConfigError> {
        for (k, v) in values {
            match k.as_str() {
                "llm_base_url" => self.llm_base_url = v.clone(),
                "llm_model" => self.llm_model = v.clone(),
                "temperature" => {
                    self.temperature = v
                        .parse()
                        .map_err(|_| ConfigError::InvalidValue(format!("{k}={v}")))?;
                }
                "max_tokens" => {
                    self.max_tokens = v
                        .parse()
                        .map_err(|_| ConfigError::InvalidValue(format!("{k}={v}")))?;
                }
                "ai_system_prompt" => self.ai_system_prompt = v.clone(),
                other => return Err(ConfigError::UnknownKey(other.to_owned())),
            }
        }
        Ok(())
    }
}

/// 配置读写。
pub struct ConfigManager {
    dirs: VerbaDirs,
    path: PathBuf,
}

impl ConfigManager {
    pub fn new(dirs: VerbaDirs) -> Self {
        let path = dirs.config_dir().join(CONFIG_FILE);
        Self { dirs, path }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// 加载配置：文件不存在时写入默认值并返回默认。
    pub fn load(&self) -> Result<Config, ConfigError> {
        self.dirs.ensure()?;
        if !self.path.exists() {
            let cfg = Config::default();
            self.save(&cfg)?;
            return Ok(cfg);
        }
        let raw = std::fs::read_to_string(&self.path)?;
        Ok(toml::from_str(&raw)?)
    }

    pub fn save(&self, cfg: &Config) -> Result<(), ConfigError> {
        self.dirs.ensure()?;
        let raw = toml::to_string_pretty(cfg)?;
        std::fs::write(&self.path, raw)?;
        Ok(())
    }
}

/// API Key 密钥库。
pub struct ApiKeyStore;

impl ApiKeyStore {
    /// 读取 API Key：优先系统密钥库，其次环境变量（开发）。
    pub fn get() -> Result<Option<String>, ConfigError> {
        if let Ok(Some(key)) = Self::from_keyring() {
            return Ok(Some(key));
        }
        Ok(std::env::var(ENV_API_KEY).ok().filter(|s| !s.is_empty()))
    }

    /// 写入 API Key 到系统密钥库。
    pub fn set(key: &str) -> Result<(), ConfigError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .map_err(|e| ConfigError::Keyring(e.to_string()))?;
        entry
            .set_password(key)
            .map_err(|e| ConfigError::Keyring(e.to_string()))
    }

    /// 删除 API Key。
    pub fn delete() -> Result<(), ConfigError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .map_err(|e| ConfigError::Keyring(e.to_string()))?;
        entry
            .delete_credential()
            .map_err(|e| ConfigError::Keyring(e.to_string()))
    }

    fn from_keyring() -> Result<Option<String>, ConfigError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .map_err(|e| ConfigError::Keyring(e.to_string()))?;
        match entry.get_password() {
            Ok(p) => Ok(Some(p)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(ConfigError::Keyring(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_roundtrip_toml() {
        let cfg = Config::default();
        let raw = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&raw).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn apply_map_updates_fields() {
        let mut cfg = Config::default();
        let mut map = HashMap::new();
        map.insert("llm_base_url".into(), "https://api.example.com/v1".into());
        map.insert("temperature".into(), "0.1".into());
        map.insert("max_tokens".into(), "256".into());
        cfg.apply_map(&map).unwrap();
        assert_eq!(cfg.llm_base_url, "https://api.example.com/v1");
        assert_eq!(cfg.temperature, 0.1);
        assert_eq!(cfg.max_tokens, 256);
    }

    #[test]
    fn apply_map_rejects_unknown_key() {
        let mut cfg = Config::default();
        let mut map = HashMap::new();
        map.insert("bogus".into(), "1".into());
        assert!(matches!(
            cfg.apply_map(&map),
            Err(ConfigError::UnknownKey(_))
        ));
    }

    #[test]
    fn apply_map_rejects_bad_value() {
        let mut cfg = Config::default();
        let mut map = HashMap::new();
        map.insert("max_tokens".into(), "not-a-number".into());
        assert!(matches!(
            cfg.apply_map(&map),
            Err(ConfigError::InvalidValue(_))
        ));
    }

    #[test]
    fn config_manager_persists_to_temp_dir() {
        let tmp = std::env::temp_dir().join(format!("verba-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let dirs = crate::dirs::VerbaDirs::from_paths(tmp.clone());
        let mgr = ConfigManager::new(dirs);
        let mut cfg = mgr.load().unwrap();
        assert_eq!(cfg, Config::default());
        cfg.llm_model = "qwen-max".into();
        mgr.save(&cfg).unwrap();
        let reloaded = mgr.load().unwrap();
        assert_eq!(reloaded.llm_model, "qwen-max");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn api_key_falls_back_to_env() {
        std::env::set_var(ENV_API_KEY, "sk-test-env");
        assert_eq!(ApiKeyStore::get().unwrap(), Some("sk-test-env".into()));
        std::env::remove_var(ENV_API_KEY);
    }
}
