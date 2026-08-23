//! 快捷短语：用户可定义模板，`//短语 <名称>` 一键插入。
//!
//! 存储：`{config_dir}/phrases.toml`（顶层 `name = "text"` 表）。

use std::collections::BTreeMap;

use crate::config::ConfigError;
use crate::dirs::VerbaDirs;

const PHRASES_FILE: &str = "phrases.toml";

/// 读取全部短语。
pub fn load(dirs: &VerbaDirs) -> Result<BTreeMap<String, String>, ConfigError> {
    let path = dirs.config_dir().join(PHRASES_FILE);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = std::fs::read_to_string(&path)?;
    let doc: toml::Table = toml::from_str(&text)?;
    let mut map = BTreeMap::new();
    for (k, v) in doc {
        if let Some(s) = v.as_str() {
            map.insert(k, s.to_owned());
        }
    }
    Ok(map)
}

/// 读取单条短语。
pub fn get(dirs: &VerbaDirs, name: &str) -> Result<Option<String>, ConfigError> {
    Ok(load(dirs)?.get(name).cloned())
}

/// 设置/删除短语（text 为空则删除）。
pub fn set(dirs: &VerbaDirs, name: &str, text: &str) -> Result<(), ConfigError> {
    std::fs::create_dir_all(dirs.config_dir())?;
    let mut map = load(dirs)?;
    if text.is_empty() {
        map.remove(name);
    } else {
        map.insert(name.to_owned(), text.to_owned());
    }
    let mut doc = toml::Table::new();
    for (k, v) in map {
        doc.insert(k, toml::Value::String(v));
    }
    let out = toml::to_string_pretty(&doc)?;
    std::fs::write(dirs.config_dir().join(PHRASES_FILE), out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_set_get() {
        let tmp = std::env::temp_dir().join(format!("verba-phrases-test-{}", std::process::id()));
        let dirs = VerbaDirs::from_paths(tmp.clone());
        set(&dirs, "greet", "你好，我是 Verba").unwrap();
        assert_eq!(
            get(&dirs, "greet").unwrap().as_deref(),
            Some("你好，我是 Verba")
        );
        // 覆盖 + 删除
        set(&dirs, "greet", "新问候").unwrap();
        assert_eq!(get(&dirs, "greet").unwrap().as_deref(), Some("新问候"));
        set(&dirs, "greet", "").unwrap();
        assert_eq!(get(&dirs, "greet").unwrap(), None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn load_missing_is_empty() {
        let tmp =
            std::env::temp_dir().join(format!("verba-phrases-missing-{}", std::process::id()));
        let dirs = VerbaDirs::from_paths(tmp.clone());
        assert!(load(&dirs).unwrap().is_empty());
        std::fs::remove_dir_all(&tmp).ok();
    }
}
