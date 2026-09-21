//! Verba 设置面板（Slint 1.17 跨平台桌面 UI）。
//!
//! 通过 verba-ipc 与 daemon 通信：GetConfig/SetConfig 读写配置，ApiKeySet 写密钥库并热更新。
//! 所有阻塞 IPC 都在后台线程执行，UI 更新经 slint::invoke_from_event_loop 回到事件循环线程。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

use std::collections::HashMap;

use verba_config::ApiKeyStore;
use verba_ipc::VerbaClient;

/// daemon 冷启动连接的有界重试（约 5s 上限）：绑 socket + librime 加载 + 预热
/// 在真机上耗时波动，固定 sleep 会在首次打开时报 Connection refused（v0.2.16 复现）。
const CONNECT_ATTEMPTS: u32 = 10;
const CONNECT_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// 最近一次刷新得到的模型列表（UI 下拉数据源，Rust 侧读取避免 ModelRc 存取）。
static MODELS_CACHE: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> =
    std::sync::OnceLock::new();
fn models_cache() -> &'static std::sync::Mutex<Vec<String>> {
    MODELS_CACHE.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// 合并「服务商返回的模型列表」与「当前配置的模型名」：
/// - 去空、去重，保持服务商返回的顺序；
/// - 当前模型不在列表里（自定义模型名 / 服务商没列出来）时插到**首位**，
///   这样下拉永远能显示并选中真正在用的模型，而不是一片空白；
/// - 返回 `(列表, 当前模型下标)`；当前模型为空时下标 `-1`。
///
/// 之所以独立成纯函数：UI 打开时的"播种"与刷新后的"回填"必须走同一套合并语义，
/// 否则两处实现必然漂移（见 LESSON「同一语义两处实现必然漂移」）。
fn merge_current_model(fetched: Vec<String>, current: &str) -> (Vec<String>, i32) {
    let current = current.trim();
    let mut list: Vec<String> = Vec::with_capacity(fetched.len() + 1);
    for m in fetched {
        let m = m.trim();
        if m.is_empty() || list.iter().any(|x| x == m) {
            continue;
        }
        list.push(m.to_owned());
    }
    if current.is_empty() {
        return (list, -1);
    }
    if let Some(i) = list.iter().position(|m| m == current) {
        return (list, i as i32);
    }
    list.insert(0, current.to_owned());
    (list, 0)
}

/// 把模型列表写进 UI 与 Rust 侧缓存（两者的下标↔取值必须同源）。
fn apply_models(ui: &SettingsWindow, list: Vec<String>, index: i32) {
    *models_cache().lock().unwrap() = list.clone();
    let ui_models: Vec<slint::SharedString> =
        list.into_iter().map(slint::SharedString::from).collect();
    ui.set_llm_models(slint::ModelRc::new(std::rc::Rc::new(
        slint::VecModel::from(ui_models),
    )));
    ui.set_llm_model_index(index);
}

/// 拉取模型列表并回填下拉：**按钮与「打开即加载」共用同一实现**。
/// `auto = true`：无 key 直接跳过、失败只写状态栏（不打断使用）；
/// `auto = false`：用户显式点击，文案区分。
fn spawn_fetch_models(ui: &SettingsWindow, auto: bool) {
    // 当前模型必须在 UI 线程读（Slint 类型不能跨线程）。
    let current = ui.get_llm_model().to_string();
    if auto {
        ui.set_status_text("正在获取模型列表…".into());
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        // 不做本地 key 预检：key 的权威在 daemon（启动时读密钥库/环境，之后由 SetApiKey 热更新），
        // 面板进程本地读到的值可能与之不同（如 daemon 带 VERBA_API_KEY 而面板没有）——
        // 预检会误报"未配置"但其实能拉到。直接请求，按 daemon 返回归类文案（一次本地 IPC，代价≈0）。
        let result = with_client(|c| c.llm_list_models());
        let weak2 = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak2.upgrade() else {
                return;
            };
            match result {
                Ok(models) => {
                    let (list, idx) = merge_current_model(models, &current);
                    let n = list.len();
                    apply_models(&ui, list, idx);
                    ui.set_status_text(
                        if auto {
                            format!("模型列表已加载（{n} 个）")
                        } else {
                            format!("模型列表已刷新（{n} 个）")
                        }
                        .into(),
                    );
                }
                Err(e) => {
                    let missing_key = e.to_string().contains("未配置 API Key");
                    ui.set_status_text(
                        if missing_key {
                            "未配置 API Key，模型列表未加载（可点『刷新模型』）".to_owned()
                        } else if auto {
                            format!("模型列表未加载：{e}（可点『刷新模型』重试）")
                        } else {
                            format!("刷新模型失败: {e}")
                        }
                        .into(),
                    );
                }
            }
        });
    });
}

/// provider 显示标签 → 实际配置值（顺序与 settings.slint 的 ComboBox 模型一致）。
const ASR_PROVIDERS: &[(&str, &str)] = &[
    ("mock（确定性，开发/验收）", "mock"),
    ("openai（在线转写）", "openai"),
];
const TTS_PROVIDERS: &[(&str, &str)] = &[
    ("mock（确定性，开发/验收）", "mock"),
    ("edge（微软在线音色）", "edge"),
    ("openai（OpenAI 兼容音色）", "openai"),
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
        // 这次保存是否刚填了新密钥：是的话保存成功后顺手拉一次模型列表
        // （刚配好 key 的用户下一步就是想看到真实模型列表，不该再逼他找刷新按钮）。
        let key_just_set = !new_key.trim().is_empty();
        let weak2 = weak.clone();
        std::thread::spawn(move || {
            let status = save_fields(values, &new_key);
            let key_state = api_key_state_text();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak2.upgrade() {
                    ui.set_api_key_input(slint::SharedString::default());
                    ui.set_api_key_state(key_state.into());
                    if key_just_set {
                        // 先触发拉取（它会写"正在获取模型列表…"），再把"已保存"盖上，
                        // 避免保存成功的提示被 in-flight 文案瞬即顶掉（审查 F1）。
                        spawn_fetch_models(&ui, true);
                    }
                    ui.set_status_text(status.into());
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

    // 刷新模型列表（服务商官方 API，需已配置 API Key）——与"打开即加载"共用同一实现
    let weak = ui.as_weak();
    ui.on_refresh_models(move || {
        if let Some(ui) = weak.upgrade() {
            spawn_fetch_models(&ui, false);
        }
    });

    let weak = ui.as_weak();
    ui.on_install_rare_chars(move || {
        let weak2 = weak.clone();
        std::thread::spawn(move || {
            let status = with_client(|c| c.rime_install_extra())
                .map(|()| "生僻字扩展已安装并重新部署（biang 拼音即刻可用）".to_owned())
                .unwrap_or_else(|e| format!("安装生僻字扩展失败: {e}"));
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak2.upgrade() {
                    ui.set_status_text(status.into());
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
    {
        // 模型下拉选中优先（未匹配/未刷新时回退 llm-model 原值）
        let idx = ui.get_llm_model_index();
        let cache = models_cache().lock().unwrap();
        let model = if idx >= 0 && (idx as usize) < cache.len() {
            cache[idx as usize].clone()
        } else {
            ui.get_llm_model().to_string()
        };
        drop(cache);
        values.insert("llm_model".into(), model);
    }
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

/// daemon 可执行文件的候选位置（按优先级）：
/// `VERBA_DAEMON_PATH` → 用户级/系统级输入法 bundle 内。
///
/// 第 2 步（verba-settings-standalone-app）：设置面板装到 /Applications 后是独立
/// 进程，**不能假设 daemon 已经在跑**（只有输入法被激活时才会拉起它）。这里给出
/// 它与输入法 bundle 的约定位置。
fn daemon_candidates() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var("VERBA_DAEMON_PATH") {
        out.push(std::path::PathBuf::from(p));
    }
    if let Some(home) = std::env::var_os("HOME") {
        out.push(
            std::path::PathBuf::from(home)
                .join("Library/Input Methods/Verba.app/Contents/MacOS/verba-daemon"),
        );
    }
    out.push(std::path::PathBuf::from(
        "/Library/Input Methods/Verba.app/Contents/MacOS/verba-daemon",
    ));
    out
}

/// 连不上 daemon 时**最多尝试拉起一次**（进程内去重，避免每个操作都 spawn）。
///
/// 注意：这里只负责"把进程起起来"，**不等固定时长**。daemon 冷启动要绑 socket +
/// 加载 librime + 预热，实测 800ms 不够——固定 sleep 会让首次打开设置面板报
/// `Connection refused`（v0.2.16 真机验收复现）。等待交给下面的 `connect_with_retry`。
fn ensure_daemon_started() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static TRIED: AtomicBool = AtomicBool::new(false);
    if TRIED.swap(true, Ordering::SeqCst) {
        return false;
    }
    for candidate in daemon_candidates() {
        if candidate.is_file() && std::process::Command::new(&candidate).spawn().is_ok() {
            return true;
        }
    }
    false
}

/// 有界重试连接：`attempts` 次、每次间隔 `delay`。返回最后一次客户端或最后一次错误。
///
/// 抽成泛型便于单测（真机 daemon 冷启动耗时随 rime 预热波动，不能靠固定 sleep）。
fn connect_with_retry<T, C>(
    mut attempts: u32,
    delay: std::time::Duration,
    mut connect: C,
) -> Result<T, String>
where
    C: FnMut() -> Result<T, String>,
{
    if attempts == 0 {
        attempts = 1;
    }
    let mut last = String::from("未尝试连接");
    for i in 0..attempts {
        match connect() {
            Ok(c) => return Ok(c),
            Err(e) => last = e,
        }
        if i + 1 < attempts {
            std::thread::sleep(delay);
        }
    }
    Err(last)
}

/// 后台线程：连接 daemon 并执行一次阻塞 IPC 操作。
fn with_client<T>(
    f: impl FnOnce(&mut VerbaClient) -> Result<T, verba_ipc::IpcError>,
) -> Result<T, String> {
    // connect_verified：验活握手后才信任对端（本面板会发送 API key——
    // 全仓库最敏感的调用方，架构审查 P0-1 不得缺席）。
    let mut client = match VerbaClient::connect_verified() {
        Ok(c) => c,
        Err(first) => {
            // 独立安装的设置面板可能先于输入法被打开：尝试自己拉起 daemon，
            // 然后**有界重试**连接（冷启动要加载 rime，固定等待不够）。
            if ensure_daemon_started() {
                connect_with_retry(CONNECT_ATTEMPTS, CONNECT_DELAY, || {
                    VerbaClient::connect_verified().map_err(|e| e.to_string())
                })
                .map_err(|e| format!("已启动 daemon 但仍连不上: {e}（首次错误: {first}）"))?
            } else {
                return Err(format!(
                    "连接 daemon 失败: {first}\n未找到拾言输入法 daemon——请先安装拾言输入法，或设置 VERBA_DAEMON_PATH"
                ));
            }
        }
    };
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
                        // 打开即加载模型列表（无 key 时函数内部会跳过并给出提示），
                        // 不再要求用户先点一次「刷新模型」才知道有哪些模型。
                        spawn_fetch_models(&ui, true);
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
    {
        // 播种：把当前配置的模型立即放进下拉并选中——不依赖"刷新"，
        // 这样空列表、自定义模型名都不会显示成空白（观感=没配置）。
        let cur = ui.get_llm_model().to_string();
        let (list, idx) = merge_current_model(Vec::new(), &cur);
        apply_models(ui, list, idx);
    }
    ui.set_temperature(get("temperature").into());
    ui.set_max_tokens(get("max_tokens").into());
    ui.set_ai_system_prompt(get("ai_system_prompt").into());
    ui.set_ai_context_turns(get("ai_context_turns").into());
    ui.set_asr_provider_index(index_of(ASR_PROVIDERS, &get("asr_provider")));
    ui.set_asr_base_url(get("asr_base_url").into());
    ui.set_asr_model(get("asr_model").into());
    ui.set_tts_provider_index(index_of(TTS_PROVIDERS, &get("tts_provider")));
    ui.set_tts_base_url(get("tts_base_url").into());
    ui.set_tts_model(get("tts_model").into());
    ui.set_tts_voice(get("tts_voice").into());
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
        assert_eq!(index_of(THEMES, "dark"), 1);
    }

    #[test]
    fn pick_maps_index_to_value() {
        assert_eq!(pick(TTS_PROVIDERS, 2), "openai");
        assert_eq!(pick(THEMES, 99), "light", "越界回退首个");
    }

    #[test]
    fn hidden_provider_lists_cover_config_values() {
        // 与 config 白名单保持一致，防止隐藏入口的 ASR/TTS 枚举漂移；
        // OCR provider 已不暴露给用户，由 daemon 内置默认 + CLI/验收覆盖。
        let asr: Vec<&str> = ASR_PROVIDERS.iter().map(|(_, v)| *v).collect();
        assert!(asr.contains(&"mock") && asr.contains(&"openai"));
        let tts: Vec<&str> = TTS_PROVIDERS.iter().map(|(_, v)| *v).collect();
        assert!(tts.contains(&"mock") && tts.contains(&"edge") && tts.contains(&"openai"));
    }

    #[test]
    fn merge_current_model_seeds_configured_model_when_list_empty() {
        // 打开面板不刷新时：下拉里必须有当前配置的模型（旧行为是空列表 + 下标 -1 = 观感空白）
        let (list, idx) = merge_current_model(Vec::new(), "deepseek-flash");
        assert_eq!(list, vec!["deepseek-flash"]);
        assert_eq!(idx, 0);
    }

    #[test]
    fn merge_current_model_selects_hit_in_fetched_list() {
        let fetched = vec![
            "deepseek-v4-flash".to_string(),
            "deepseek-v4-pro".to_string(),
        ];
        let (list, idx) = merge_current_model(fetched.clone(), "deepseek-v4-pro");
        assert_eq!(list, fetched, "命中时保持服务商返回的顺序");
        assert_eq!(idx, 1);
    }

    #[test]
    fn merge_current_model_keeps_custom_model_visible() {
        // 自定义模型名（服务商列表里没有）不能被"吃掉"：插首位并选中
        let (list, idx) = merge_current_model(vec!["deepseek-v4-flash".to_string()], "gpt-4o");
        assert_eq!(list, vec!["gpt-4o", "deepseek-v4-flash"]);
        assert_eq!(idx, 0);
    }

    #[test]
    fn merge_current_model_empty_current_keeps_list_and_no_index() {
        let (list, idx) = merge_current_model(vec!["a".to_string()], "   ");
        assert_eq!(list, vec!["a"]);
        assert_eq!(idx, -1, "当前模型为空时不应强行选中");
    }

    #[test]
    fn merge_current_model_dedups_and_trims() {
        let (list, idx) = merge_current_model(
            vec![
                " a ".to_string(),
                "a".to_string(),
                "".to_string(),
                "b".to_string(),
            ],
            "a",
        );
        assert_eq!(list, vec!["a", "b"]);
        assert_eq!(idx, 0);
    }
}
