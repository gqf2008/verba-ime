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
const DEFAULT_ASR_MODEL: &str = "whisper-1";
const DEFAULT_TTS_MODEL: &str = "tts-1";

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

/// 候选窗主题配置：预设（light/dark）+ 逐项覆盖。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeConfig {
    /// 预设：`light` / `dark`。
    #[serde(default = "default_theme_preset")]
    pub preset: String,
    /// 背景色（`#RRGGBB`）。
    #[serde(default)]
    pub background: Option<String>,
    /// 候选文字色。
    #[serde(default)]
    pub text_color: Option<String>,
    /// 选中项背景色。
    #[serde(default)]
    pub selected_background: Option<String>,
    /// 选中项文字色。
    #[serde(default)]
    pub selected_text_color: Option<String>,
    /// 边框色。
    #[serde(default)]
    pub border_color: Option<String>,
    /// 字号（像素）。
    #[serde(default)]
    pub font_size: Option<u32>,
    /// 圆角半径（像素）。
    #[serde(default)]
    pub corner_radius: Option<u32>,
    /// 布局：horizontal（微软拼音/手心风格）| vertical（竖向列表）。
    #[serde(default)]
    pub layout: Option<String>,
    /// 是否显示拼音组合头。
    #[serde(default)]
    pub show_preedit: Option<bool>,
    /// 组合头高度（px）。
    #[serde(default)]
    pub header_height: Option<u32>,
    /// 候选间距（horizontal，px）。
    #[serde(default)]
    pub gap: Option<u32>,
    /// horizontal 窗口最大宽度（px）。
    #[serde(default)]
    pub max_width_horizontal: Option<u32>,
    /// 候选块内左右留白（horizontal，px）。
    #[serde(default)]
    pub item_padding: Option<u32>,
    /// 页码脚高度（多页时，px）。
    #[serde(default)]
    pub footer_height: Option<u32>,
    /// 拼音组合头文字色（`#RRGGBB`）。
    #[serde(default)]
    pub header_text_color: Option<String>,
    /// 分隔线颜色（`#RRGGBB`）。
    #[serde(default)]
    pub separator_color: Option<String>,
    /// 弱化文字色（页码脚，`#RRGGBB`）。
    #[serde(default)]
    pub muted_color: Option<String>,
}

fn default_theme_preset() -> String {
    "light".to_owned()
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            preset: default_theme_preset(),
            background: None,
            text_color: None,
            selected_background: None,
            selected_text_color: None,
            border_color: None,
            font_size: None,
            corner_radius: None,
            layout: None,
            show_preedit: None,
            header_height: None,
            gap: None,
            max_width_horizontal: None,
            item_padding: None,
            footer_height: None,
            header_text_color: None,
            separator_color: None,
            muted_color: None,
        }
    }
}

impl ThemeConfig {
    /// 合并预设与覆盖 → 候选窗主题。
    pub fn to_candidate_theme(&self) -> verba_candidate::Theme {
        let mut t = match self.preset.as_str() {
            "dark" => verba_candidate::Theme::dark(),
            _ => verba_candidate::Theme::default(),
        };
        if let Some(v) = &self.background {
            t.background = v.clone();
        }
        if let Some(v) = &self.text_color {
            t.text_color = v.clone();
        }
        if let Some(v) = &self.selected_background {
            t.selected_background = v.clone();
        }
        if let Some(v) = &self.selected_text_color {
            t.selected_text_color = v.clone();
        }
        if let Some(v) = &self.border_color {
            t.border_color = v.clone();
        }
        if let Some(v) = self.font_size {
            t.font_size = v;
        }
        if let Some(v) = self.corner_radius {
            t.corner_radius = v;
        }
        if let Some(v) = &self.layout {
            t.layout = v.clone();
        }
        if let Some(v) = self.show_preedit {
            t.show_preedit = v;
        }
        if let Some(v) = self.header_height {
            t.header_height = v;
        }
        if let Some(v) = self.gap {
            t.gap = v;
        }
        if let Some(v) = self.max_width_horizontal {
            t.max_width_horizontal = v;
        }
        if let Some(v) = self.item_padding {
            t.item_padding = v;
        }
        if let Some(v) = self.footer_height {
            t.footer_height = v;
        }
        if let Some(v) = &self.header_text_color {
            t.header_text_color = v.clone();
        }
        if let Some(v) = &self.separator_color {
            t.separator_color = v.clone();
        }
        if let Some(v) = &self.muted_color {
            t.muted_color = v.clone();
        }
        t
    }
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
    /// 候选窗主题。
    #[serde(default)]
    pub theme: ThemeConfig,
    /// 中文引擎：builtin（默认，verba-pinyin）| rime（可选，daemon 内 librime）。
    #[serde(default = "default_engine")]
    pub engine: String,
    /// Rime 方案（engine=rime 时）：luna_pinyin_simp（默认）| wubi86 | 其它已部署方案。
    #[serde(default = "default_rime_schema")]
    pub rime_schema: String,
    /// TTS provider：mock（默认，确定性 WAV，开发/验收）| edge（微软在线神经音色）| openai（OpenAI 兼容在线音色）| …
    #[serde(default = "default_tts_provider")]
    pub tts_provider: String,
    /// TTS 语音名（如 edge 的 zh-CN-XiaoxiaoNeural；mock 忽略）。
    #[serde(default)]
    pub tts_voice: String,
    /// OCR provider：mock（默认，确定性）| windows（Windows.Media.Ocr 本地识别）。
    #[serde(default = "default_ocr_provider")]
    pub ocr_provider: String,
    /// ASR provider：mock（默认，确定性）| openai（OpenAI 兼容在线转写）。
    #[serde(default = "default_asr_provider")]
    pub asr_provider: String,
    /// ASR 在线端点基址（openai provider；空 = 复用 llm_base_url）。
    #[serde(default)]
    pub asr_base_url: String,
    /// ASR 在线模型名（openai provider；默认 whisper-1）。
    #[serde(default = "default_asr_model")]
    pub asr_model: String,
    /// TTS 在线端点基址（openai provider；空 = 复用 llm_base_url）。
    #[serde(default)]
    pub tts_base_url: String,
    /// TTS 在线模型名（openai provider；默认 tts-1）。
    #[serde(default = "default_tts_model")]
    pub tts_model: String,
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
fn default_engine() -> String {
    "builtin".to_owned()
}
fn default_rime_schema() -> String {
    "luna_pinyin_simp".to_owned()
}
fn default_tts_provider() -> String {
    "mock".to_owned()
}
fn default_ocr_provider() -> String {
    "mock".to_owned()
}
fn default_asr_provider() -> String {
    "mock".to_owned()
}

fn default_asr_model() -> String {
    DEFAULT_ASR_MODEL.to_owned()
}
fn default_tts_model() -> String {
    DEFAULT_TTS_MODEL.to_owned()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            llm_base_url: default_llm_base_url(),
            llm_model: default_llm_model(),
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            ai_system_prompt: String::new(),
            theme: ThemeConfig::default(),
            engine: default_engine(),
            rime_schema: default_rime_schema(),
            tts_provider: default_tts_provider(),
            tts_voice: String::new(),
            ocr_provider: default_ocr_provider(),
            asr_provider: default_asr_provider(),
            asr_base_url: String::new(),
            asr_model: default_asr_model(),
            tts_base_url: String::new(),
            tts_model: default_tts_model(),
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
        map.insert("engine".into(), self.engine.clone());
        map.insert("rime_schema".into(), self.rime_schema.clone());
        map.insert("tts_provider".into(), self.tts_provider.clone());
        map.insert("tts_voice".into(), self.tts_voice.clone());
        map.insert("ocr_provider".into(), self.ocr_provider.clone());
        map.insert("asr_provider".into(), self.asr_provider.clone());
        map.insert("asr_base_url".into(), self.asr_base_url.clone());
        map.insert("asr_model".into(), self.asr_model.clone());
        map.insert("tts_base_url".into(), self.tts_base_url.clone());
        map.insert("tts_model".into(), self.tts_model.clone());
        map.insert("theme.preset".into(), self.theme.preset.clone());
        if let Some(v) = &self.theme.background {
            map.insert("theme.background".into(), v.clone());
        }
        if let Some(v) = &self.theme.text_color {
            map.insert("theme.text_color".into(), v.clone());
        }
        if let Some(v) = &self.theme.selected_background {
            map.insert("theme.selected_background".into(), v.clone());
        }
        if let Some(v) = &self.theme.selected_text_color {
            map.insert("theme.selected_text_color".into(), v.clone());
        }
        if let Some(v) = &self.theme.border_color {
            map.insert("theme.border_color".into(), v.clone());
        }
        if let Some(v) = self.theme.font_size {
            map.insert("theme.font_size".into(), v.to_string());
        }
        if let Some(v) = self.theme.corner_radius {
            map.insert("theme.corner_radius".into(), v.to_string());
        }
        if let Some(v) = &self.theme.layout {
            map.insert("theme.layout".into(), v.clone());
        }
        if let Some(v) = self.theme.show_preedit {
            map.insert("theme.show_preedit".into(), v.to_string());
        }
        if let Some(v) = self.theme.header_height {
            map.insert("theme.header_height".into(), v.to_string());
        }
        if let Some(v) = self.theme.gap {
            map.insert("theme.gap".into(), v.to_string());
        }
        if let Some(v) = self.theme.max_width_horizontal {
            map.insert("theme.max_width_horizontal".into(), v.to_string());
        }
        if let Some(v) = self.theme.item_padding {
            map.insert("theme.item_padding".into(), v.to_string());
        }
        if let Some(v) = self.theme.footer_height {
            map.insert("theme.footer_height".into(), v.to_string());
        }
        if let Some(v) = &self.theme.header_text_color {
            map.insert("theme.header_text_color".into(), v.clone());
        }
        if let Some(v) = &self.theme.separator_color {
            map.insert("theme.separator_color".into(), v.clone());
        }
        if let Some(v) = &self.theme.muted_color {
            map.insert("theme.muted_color".into(), v.clone());
        }
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
                "engine" => {
                    if v != "builtin" && v != "rime" {
                        return Err(ConfigError::InvalidValue(format!(
                            "engine 仅支持 builtin|rime: {k}={v}"
                        )));
                    }
                    self.engine = v.clone();
                }
                "rime_schema" => self.rime_schema = v.clone(),
                "tts_provider" => {
                    if v != "mock" && v != "edge" && v != "openai" {
                        return Err(ConfigError::InvalidValue(format!(
                            "tts_provider 仅支持 mock|edge|openai: {k}={v}"
                        )));
                    }
                    self.tts_provider = v.clone();
                }
                "tts_voice" => self.tts_voice = v.clone(),
                "ocr_provider" => {
                    if v != "mock" && v != "windows" {
                        return Err(ConfigError::InvalidValue(format!(
                            "ocr_provider 仅支持 mock|windows: {k}={v}"
                        )));
                    }
                    self.ocr_provider = v.clone();
                }
                "asr_provider" => {
                    if v != "mock" && v != "openai" {
                        return Err(ConfigError::InvalidValue(format!(
                            "asr_provider 仅支持 mock|openai: {k}={v}"
                        )));
                    }
                    self.asr_provider = v.clone();
                }
                "asr_base_url" => self.asr_base_url = v.clone(),
                "asr_model" => self.asr_model = v.clone(),
                "tts_base_url" => self.tts_base_url = v.clone(),
                "tts_model" => self.tts_model = v.clone(),
                "theme.preset" => self.theme.preset = v.clone(),
                "theme.background" => self.theme.background = Some(v.clone()),
                "theme.text_color" => self.theme.text_color = Some(v.clone()),
                "theme.selected_background" => self.theme.selected_background = Some(v.clone()),
                "theme.selected_text_color" => self.theme.selected_text_color = Some(v.clone()),
                "theme.border_color" => self.theme.border_color = Some(v.clone()),
                "theme.font_size" => {
                    self.theme.font_size = Some(
                        v.parse()
                            .map_err(|_| ConfigError::InvalidValue(format!("{k}={v}")))?,
                    );
                }
                "theme.corner_radius" => {
                    self.theme.corner_radius = Some(
                        v.parse()
                            .map_err(|_| ConfigError::InvalidValue(format!("{k}={v}")))?,
                    );
                }
                "theme.layout" => self.theme.layout = Some(v.clone()),
                "theme.show_preedit" => {
                    self.theme.show_preedit = Some(
                        v.parse()
                            .map_err(|_| ConfigError::InvalidValue(format!("{k}={v}")))?,
                    );
                }
                "theme.header_height" => {
                    self.theme.header_height = Some(
                        v.parse()
                            .map_err(|_| ConfigError::InvalidValue(format!("{k}={v}")))?,
                    );
                }
                "theme.gap" => {
                    self.theme.gap = Some(
                        v.parse()
                            .map_err(|_| ConfigError::InvalidValue(format!("{k}={v}")))?,
                    );
                }
                "theme.max_width_horizontal" => {
                    self.theme.max_width_horizontal = Some(
                        v.parse()
                            .map_err(|_| ConfigError::InvalidValue(format!("{k}={v}")))?,
                    );
                }
                "theme.item_padding" => {
                    self.theme.item_padding = Some(
                        v.parse()
                            .map_err(|_| ConfigError::InvalidValue(format!("{k}={v}")))?,
                    );
                }
                "theme.footer_height" => {
                    self.theme.footer_height = Some(
                        v.parse()
                            .map_err(|_| ConfigError::InvalidValue(format!("{k}={v}")))?,
                    );
                }
                "theme.header_text_color" => self.theme.header_text_color = Some(v.clone()),
                "theme.separator_color" => self.theme.separator_color = Some(v.clone()),
                "theme.muted_color" => self.theme.muted_color = Some(v.clone()),
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

    /// 删除 API Key（密钥不存在时视为成功，幂等）。
    pub fn clear() -> Result<(), ConfigError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .map_err(|e| ConfigError::Keyring(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(ConfigError::Keyring(e.to_string())),
        }
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
    fn theme_default_is_light_preset() {
        let t = ThemeConfig::default();
        assert_eq!(t.preset, "light");
        // light 预设 ≈ 候选窗默认主题
        let cand = t.to_candidate_theme();
        assert_eq!(cand, verba_candidate::Theme::default());
    }

    #[test]
    fn theme_dark_preset_changes_colors() {
        let t = ThemeConfig {
            preset: "dark".into(),
            ..ThemeConfig::default()
        };
        let cand = t.to_candidate_theme();
        assert_eq!(cand.background, "#1E1E1E");
        assert_eq!(cand, verba_candidate::Theme::dark());
    }

    #[test]
    fn theme_overrides_merge_onto_preset() {
        let t = ThemeConfig {
            preset: "light".into(),
            background: Some("#112233".into()),
            font_size: Some(18),
            corner_radius: Some(0),
            ..ThemeConfig::default()
        };
        let cand = t.to_candidate_theme();
        assert_eq!(cand.background, "#112233");
        assert_eq!(cand.font_size, 18);
        assert_eq!(cand.corner_radius, 0);
        // 未覆盖的字段仍取预设值
        assert_eq!(
            cand.text_color,
            verba_candidate::Theme::default().text_color
        );
    }

    #[test]
    fn theme_keys_flow_through_map() {
        let mut cfg = Config::default();
        let mut map = std::collections::HashMap::new();
        map.insert("theme.preset".into(), "dark".into());
        map.insert("theme.corner_radius".into(), "10".into());
        cfg.apply_map(&map).unwrap();
        assert_eq!(cfg.theme.preset, "dark");
        assert_eq!(cfg.theme.corner_radius, Some(10));
        // 回读 to_map 能看到主题键
        let out = cfg.to_map();
        assert_eq!(out.get("theme.preset").map(String::as_str), Some("dark"));
        assert_eq!(
            out.get("theme.corner_radius").map(String::as_str),
            Some("10")
        );
    }

    #[test]
    fn theme_modern_layout_keys_flow_through_map() {
        let mut cfg = Config::default();
        let mut map = std::collections::HashMap::new();
        map.insert("theme.layout".into(), "horizontal".into());
        map.insert("theme.show_preedit".into(), "true".into());
        map.insert("theme.header_height".into(), "26".into());
        map.insert("theme.gap".into(), "12".into());
        map.insert("theme.max_width_horizontal".into(), "600".into());
        map.insert("theme.item_padding".into(), "6".into());
        map.insert("theme.footer_height".into(), "20".into());
        map.insert("theme.header_text_color".into(), "#777777".into());
        map.insert("theme.separator_color".into(), "#DADADA".into());
        map.insert("theme.muted_color".into(), "#999999".into());
        cfg.apply_map(&map).unwrap();
        assert_eq!(cfg.theme.layout.as_deref(), Some("horizontal"));
        assert_eq!(cfg.theme.show_preedit, Some(true));
        assert_eq!(cfg.theme.header_height, Some(26));
        assert_eq!(cfg.theme.gap, Some(12));
        assert_eq!(cfg.theme.max_width_horizontal, Some(600));
        assert_eq!(cfg.theme.item_padding, Some(6));
        assert_eq!(cfg.theme.footer_height, Some(20));
        assert_eq!(cfg.theme.header_text_color.as_deref(), Some("#777777"));
        assert_eq!(cfg.theme.separator_color.as_deref(), Some("#DADADA"));
        assert_eq!(cfg.theme.muted_color.as_deref(), Some("#999999"));
        let out = cfg.to_map();
        assert_eq!(
            out.get("theme.layout").map(String::as_str),
            Some("horizontal")
        );
        assert_eq!(
            out.get("theme.show_preedit").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            out.get("theme.header_height").map(String::as_str),
            Some("26")
        );
        let cand = cfg.theme.to_candidate_theme();
        assert_eq!(cand.layout, "horizontal");
        assert!(cand.show_preedit);
        assert_eq!(cand.header_height, 26);
        assert_eq!(cand.gap, 12);
        assert_eq!(cand.max_width_horizontal, 600);
        assert_eq!(cand.item_padding, 6);
        assert_eq!(cand.footer_height, 20);
        assert_eq!(cand.header_text_color, "#777777");
        assert_eq!(cand.separator_color, "#DADADA");
        assert_eq!(cand.muted_color, "#999999");
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

    #[test]
    fn online_modal_keys_flow_through_map() {
        let mut cfg = Config::default();
        let mut map = HashMap::new();
        map.insert("asr_provider".into(), "openai".into());
        map.insert("asr_base_url".into(), "https://asr.example.com/v1".into());
        map.insert("asr_model".into(), "whisper-1".into());
        map.insert("tts_provider".into(), "openai".into());
        map.insert("tts_base_url".into(), "https://tts.example.com/v1".into());
        map.insert("tts_model".into(), "tts-1-hd".into());
        cfg.apply_map(&map).unwrap();
        assert_eq!(cfg.asr_provider, "openai");
        assert_eq!(cfg.asr_base_url, "https://asr.example.com/v1");
        assert_eq!(cfg.asr_model, "whisper-1");
        assert_eq!(cfg.tts_provider, "openai");
        assert_eq!(cfg.tts_base_url, "https://tts.example.com/v1");
        assert_eq!(cfg.tts_model, "tts-1-hd");
        let out = cfg.to_map();
        assert_eq!(out.get("asr_model").map(String::as_str), Some("whisper-1"));
        assert_eq!(out.get("tts_model").map(String::as_str), Some("tts-1-hd"));
        let def = Config::default();
        assert_eq!(def.asr_model, "whisper-1");
        assert_eq!(def.tts_model, "tts-1");
        assert!(def.asr_base_url.is_empty());
    }

    #[test]
    fn provider_whitelist_rejects_unknown() {
        let mut cfg = Config::default();
        let mut map = HashMap::new();
        map.insert("tts_provider".into(), "piper".into());
        assert!(matches!(
            cfg.apply_map(&map),
            Err(ConfigError::InvalidValue(_))
        ));
        let mut map = HashMap::new();
        map.insert("asr_provider".into(), "whisper".into());
        assert!(matches!(
            cfg.apply_map(&map),
            Err(ConfigError::InvalidValue(_))
        ));
    }
}
