//! Verba 设置面板（Slint 1.17 跨平台桌面 UI）。
//!
//! 通过 verba-ipc 与 daemon 通信：GetConfig/SetConfig 读写配置，ApiKeySet 写密钥库并热更新。
//! 所有阻塞 IPC 都在后台线程执行，UI 更新经 slint::invoke_from_event_loop 回到事件循环线程。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

use std::collections::HashMap;

use verba_config::ApiKeyStore;
use verba_ipc::VerbaClient;

/// provider 显示标签 → 实际配置值（顺序与 settings.slint 的 ComboBox 模型一致）。
const OCR_PROVIDERS: &[(&str, &str)] = &[
    ("mock（确定性，开发/验收）", "mock"),
    ("windows（Windows 本地识别）", "windows"),
    (
        "rapid（本地 RapidOCR/PaddleOCR，需 Python rapidocr_onnxruntime）",
        "rapid",
    ),
];
const ASR_PROVIDERS: &[(&str, &str)] = &[
    ("mock（确定性，开发/验收）", "mock"),
    ("openai（在线转写）", "openai"),
];
const TTS_PROVIDERS: &[(&str, &str)] = &[
    ("mock（确定性，开发/验收）", "mock"),
    ("edge（微软在线音色）", "edge"),
    ("openai（OpenAI 兼容音色）", "openai"),
];
const ENGINES: &[(&str, &str)] = &[
    ("builtin（内置拼音）", "builtin"),
    ("rime（Rime 引擎，需部署 rime/）", "rime"),
];
const EYE_MODES: &[(&str, &str)] = &[
    ("ocr（本地/在线 OCR → 文字）", "ocr"),
    ("vision（多模态 LLM 直读图像）", "vision"),
];
const THEMES: &[(&str, &str)] = &[("light（浅色）", "light"), ("dark（深色）", "dark")];

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let ui = SettingsWindow::new().expect("创建设置窗口失败");
    wire_callbacks(&ui);
    load_into_ui(&ui);
    ui.show().expect("显示设置窗口失败");
    slint::run_event_loop().expect("事件循环失败");
}

/// 接线 UI 回调（读取字段须在 UI 线程，阻塞 IPC 放后台线程）。
fn wire_callbacks(ui: &SettingsWindow) {
    let weak = ui.as_weak();
    ui.on_save(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let values = read_fields(&ui);
        let new_key = ui.get_api_key_input().to_string();
        let weak2 = weak.clone();
        std::thread::spawn(move || {
            let status = save_fields(values, &new_key);
            let key_state = api_key_state_text();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak2.upgrade() {
                    ui.set_status_text(status.into());
                    ui.set_api_key_input(slint::SharedString::default());
                    ui.set_api_key_state(key_state.into());
                }
            });
        });
    });

    let weak = ui.as_weak();
    ui.on_refresh(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        load_into_ui(&ui);
    });

    let weak = ui.as_weak();
    ui.on_set_api_key(move |key: slint::SharedString| {
        let key_str = key.to_string();
        let weak2 = weak.clone();
        if key_str.is_empty() {
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak2.upgrade() {
                    ui.set_status_text("密钥为空，未保存".into());
                }
            });
            return;
        }
        std::thread::spawn(move || {
            let status = with_client(|c| c.set_api_key(&key_str))
                .map(|()| "密钥已保存（daemon 热生效）".to_owned())
                .unwrap_or_else(|e| format!("密钥保存失败: {e}"));
            let key_state = api_key_state_text();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak2.upgrade() {
                    ui.set_status_text(status.into());
                    ui.set_api_key_input(slint::SharedString::default());
                    ui.set_api_key_state(key_state.into());
                }
            });
        });
    });

    let weak = ui.as_weak();
    ui.on_clear_api_key(move || {
        let weak2 = weak.clone();
        std::thread::spawn(move || {
            let status = with_client(|c| c.set_api_key(""))
                .map(|()| "密钥已清除".to_owned())
                .unwrap_or_else(|e| format!("清除密钥失败: {e}"));
            let key_state = api_key_state_text();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak2.upgrade() {
                    ui.set_status_text(status.into());
                    ui.set_api_key_input(slint::SharedString::default());
                    ui.set_api_key_state(key_state.into());
                }
            });
        });
    });

    let weak = ui.as_weak();
    ui.on_save_phrase(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let name = ui.get_phrase_name().to_string();
        let text = ui.get_phrase_text().to_string();
        let status = if name.is_empty() {
            "请先填短语名称".to_owned()
        } else {
            phrase_set(&name, &text)
        };
        ui.set_phrase_status(status.into());
    });

    let weak = ui.as_weak();
    ui.on_delete_phrase(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let name = ui.get_phrase_name().to_string();
        let status = if name.is_empty() {
            "请填要删除的名称".to_owned()
        } else {
            phrase_set(&name, "")
        };
        ui.set_phrase_status(status.into());
    });

    let weak = ui.as_weak();
    ui.on_refresh_phrases(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        ui.set_phrase_status(phrase_refresh().into());
    });
}

fn phrase_set(name: &str, text: &str) -> String {
    match verba_config::VerbaDirs::locate() {
        Ok(dirs) => match verba_config::phrases::set(&dirs, name, text) {
            Ok(()) => format!("已保存快捷短语: {name}"),
            Err(e) => format!("保存失败: {e}"),
        },
        Err(e) => format!("定位目录失败: {e}"),
    }
}

fn phrase_refresh() -> String {
    match verba_config::VerbaDirs::locate() {
        Ok(dirs) => match verba_config::phrases::load(&dirs) {
            Ok(map) => {
                if map.is_empty() {
                    "（暂无短语）".to_owned()
                } else {
                    format!(
                        "已有短语: {}",
                        map.keys().cloned().collect::<Vec<_>>().join(", ")
                    )
                }
            }
            Err(e) => format!("读取失败: {e}"),
        },
        Err(e) => format!("定位目录失败: {e}"),
    }
}

/// 在 UI 线程读取全部字段，生成配置键值表。
fn read_fields(ui: &SettingsWindow) -> HashMap<String, String> {
    let mut values = HashMap::new();
    values.insert("llm_base_url".into(), ui.get_llm_base_url().to_string());
    values.insert("llm_model".into(), ui.get_llm_model().to_string());
    values.insert("temperature".into(), ui.get_temperature().to_string());
    values.insert("max_tokens".into(), ui.get_max_tokens().to_string());
    values.insert(
        "ai_system_prompt".into(),
        ui.get_ai_system_prompt().to_string(),
    );
    values.insert(
        "ai_context_turns".into(),
        ui.get_ai_context_turns().to_string(),
    );
    values.insert(
        "ocr_provider".into(),
        pick(OCR_PROVIDERS, ui.get_ocr_provider_index()),
    );
    values.insert(
        "llm_vision_model".into(),
        ui.get_llm_vision_model().to_string(),
    );
    values.insert("eye_mode".into(), pick(EYE_MODES, ui.get_eye_mode_index()));
    values.insert(
        "asr_provider".into(),
        pick(ASR_PROVIDERS, ui.get_asr_provider_index()),
    );
    values.insert("asr_base_url".into(), ui.get_asr_base_url().to_string());
    values.insert("asr_model".into(), ui.get_asr_model().to_string());
    values.insert(
        "tts_provider".into(),
        pick(TTS_PROVIDERS, ui.get_tts_provider_index()),
    );
    values.insert("tts_base_url".into(), ui.get_tts_base_url().to_string());
    values.insert("tts_model".into(), ui.get_tts_model().to_string());
    values.insert("tts_voice".into(), ui.get_tts_voice().to_string());
    values.insert("engine".into(), pick(ENGINES, ui.get_engine_index()));
    values.insert("rime_schema".into(), ui.get_rime_schema().to_string());
    values.insert("theme.preset".into(), pick(THEMES, ui.get_theme_index()));
    values
}

/// 后台线程：保存配置 + 可选新密钥，返回状态文本。
fn save_fields(values: HashMap<String, String>, new_key: &str) -> String {
    match with_client(|c| {
        c.set_config(values)?;
        if !new_key.is_empty() {
            c.set_api_key(new_key)?;
        }
        Ok(())
    }) {
        Ok(()) => "已保存（daemon 热生效）".to_owned(),
        Err(e) => format!("保存失败: {e}"),
    }
}

/// 后台线程：连接 daemon 并执行一次阻塞 IPC 操作。
fn with_client<T>(
    f: impl FnOnce(&mut VerbaClient) -> Result<T, verba_ipc::IpcError>,
) -> Result<T, String> {
    let mut client = VerbaClient::connect().map_err(|e| format!("连接 daemon 失败: {e}"))?;
    f(&mut client).map_err(|e| e.to_string())
}

/// 后台线程加载配置并回填 UI。
fn load_into_ui(ui: &SettingsWindow) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let loaded = with_client(|c| {
            let version = c.ping()?;
            let cfg = c.get_config()?;
            Ok((cfg, version))
        });
        let key_state = api_key_state_text();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                match loaded {
                    Ok((cfg, version)) => {
                        populate(&ui, &cfg);
                        ui.set_version_text(format!("v{version}").into());
                        ui.set_status_text("已连接 daemon".into());
                    }
                    Err(e) => {
                        ui.set_status_text(format!("{e}（可先运行 verba-cli daemon）").into());
                    }
                }
                ui.set_api_key_state(key_state.into());
            }
        });
    });
}

/// 用配置键值表回填 UI 字段。
fn populate(ui: &SettingsWindow, cfg: &HashMap<String, String>) {
    let get = |k: &str| cfg.get(k).cloned().unwrap_or_default();
    ui.set_llm_base_url(get("llm_base_url").into());
    ui.set_llm_model(get("llm_model").into());
    ui.set_temperature(get("temperature").into());
    ui.set_max_tokens(get("max_tokens").into());
    ui.set_ai_system_prompt(get("ai_system_prompt").into());
    ui.set_ai_context_turns(get("ai_context_turns").into());
    ui.set_ocr_provider_index(index_of(OCR_PROVIDERS, &get("ocr_provider")));
    ui.set_llm_vision_model(get("llm_vision_model").into());
    ui.set_eye_mode_index(index_of(EYE_MODES, &get("eye_mode")));
    ui.set_asr_provider_index(index_of(ASR_PROVIDERS, &get("asr_provider")));
    ui.set_asr_base_url(get("asr_base_url").into());
    ui.set_asr_model(get("asr_model").into());
    ui.set_tts_provider_index(index_of(TTS_PROVIDERS, &get("tts_provider")));
    ui.set_tts_base_url(get("tts_base_url").into());
    ui.set_tts_model(get("tts_model").into());
    ui.set_tts_voice(get("tts_voice").into());
    ui.set_engine_index(index_of(ENGINES, &get("engine")));
    ui.set_rime_schema(get("rime_schema").into());
    ui.set_theme_index(index_of(THEMES, &get("theme.preset")));
}

fn index_of(list: &[(&str, &str)], value: &str) -> i32 {
    list.iter()
        .position(|(_, v)| *v == value)
        .map(|i| i as i32)
        .unwrap_or(0)
}

fn pick(list: &[(&str, &str)], index: i32) -> String {
    list.get(index as usize)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| list[0].1.to_string())
}

/// 密钥状态展示文本（掩码末 4 位）。
fn api_key_state_text() -> String {
    match ApiKeyStore::get() {
        Ok(Some(key)) if !key.is_empty() => {
            let tail: String = key
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            format!("已设置（…{tail}）")
        }
        Ok(_) => "未设置（VERBA_API_KEY 环境变量或上方保存密钥）".to_owned(),
        Err(e) => format!("密钥读取失败: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_of_maps_values() {
        assert_eq!(index_of(ASR_PROVIDERS, "openai"), 1);
        assert_eq!(index_of(ASR_PROVIDERS, "mock"), 0);
        assert_eq!(index_of(ASR_PROVIDERS, "unknown"), 0, "未知值回退 0");
        assert_eq!(index_of(TTS_PROVIDERS, "edge"), 1);
        assert_eq!(index_of(TTS_PROVIDERS, "openai"), 2);
        assert_eq!(index_of(ENGINES, "rime"), 1);
        assert_eq!(index_of(THEMES, "dark"), 1);
    }

    #[test]
    fn pick_maps_index_to_value() {
        assert_eq!(pick(TTS_PROVIDERS, 2), "openai");
        assert_eq!(pick(OCR_PROVIDERS, 1), "windows");
        assert_eq!(pick(THEMES, 99), "light", "越界回退首个");
    }

    #[test]
    fn provider_lists_cover_config_values() {
        // 与 config 白名单保持一致，防止 UI 漂移
        let asr: Vec<&str> = ASR_PROVIDERS.iter().map(|(_, v)| *v).collect();
        assert!(asr.contains(&"mock") && asr.contains(&"openai"));
        let tts: Vec<&str> = TTS_PROVIDERS.iter().map(|(_, v)| *v).collect();
        assert!(tts.contains(&"mock") && tts.contains(&"edge") && tts.contains(&"openai"));
        let ocr: Vec<&str> = OCR_PROVIDERS.iter().map(|(_, v)| *v).collect();
        assert!(ocr.contains(&"mock") && ocr.contains(&"windows"));
    }
}
