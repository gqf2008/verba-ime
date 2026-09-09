//! verba-register：macOS 输入源安装注册助手（用户级，无需管理员）。
//!
//! 由 DMG 内的「安装.command」在 Verba.app 拷入 `~/Library/Input Methods` 后
//! 调用：先走 TextInputSources C API 把 Verba 注册为输入源，再把
//! `com.apple.inputsources` 的 `AppleEnabledThirdPartyInputSources` 白名单
//! 规范化为「父源 + Pinyin mode」两条并刷新 TextInputMenuAgent。
//! macOS 12+ 只调 `TISEnableInputSource` 可能返回 noErr 但父源仍 disabled；
//! 写入白名单后系统会同时启用父源和 mode，免去用户手动到系统设置添加。
//!
//! 用法：
//! ```text
//! verba-register [--app <Verba.app 路径>]   注册并启用（默认自身所在 bundle）
//! verba-register --list                     仅列出已注册输入源（只读，CI 冒烟）
//! verba-register --write-input-sources-plist [--home <home>]
//!                                              仅写第三方输入源白名单（PKG postinstall 用）
//! ```
//!
//! 说明：TIS 的注册/启用为尽力而为——`TISRegisterInputSource` 失败不阻塞
//! （app 落位后系统扫描也会注册），启用失败或源未列出时给出手动路径并返回
//! 非零退出码，让「安装.command」能如实提示。

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};
use core_foundation::url::CFURL;

/// Verba 输入法本体的 TIS 输入源 ID（与 Info.plist 的 TISInputSourceID 一致）。
const VERBA_SOURCE_ID: &str = "dev.verba.inputmethod.Verba";
/// 可选中的输入模式 ID（TISEnable/Select 须作用于 mode 而非父源——对父源
/// select 返回 paramErr(-50)，macOS 26 实测）。
const VERBA_MODE_ID: &str = "dev.verba.inputmethod.Verba.Pinyin";
/// kTISPropertyInputSourceID（TextInputSources.h 公开常量）。
const TIS_PROPERTY_INPUT_SOURCE_ID: &str = "TISPropertyInputSourceID";
/// 第三方输入法启用白名单（macOS 12+）：父源 entry 存在时系统会把该 bundle
/// 的父源和模式一起加入 HIToolbox 的启用列表。只写 TISEnableInputSource 时
/// 在部分真机会返回 noErr 但父源仍 disabled；写入该父源 entry 并刷新
/// TextInputMenuAgent 后可稳定生效（macOS 26.5 真机复现/验证）。
const INPUT_SOURCES_PLIST: &str = "Library/Preferences/com.apple.inputsources.plist";
const THIRD_PARTY_INPUT_SOURCES_KEY: &str = "AppleEnabledThirdPartyInputSources";
const BUNDLE_ID_KEY: &str = "Bundle ID";
const INPUT_SOURCE_KIND_KEY: &str = "InputSourceKind";
const KEYBOARD_INPUT_METHOD_KIND: &str = "Keyboard Input Method";

// TextInputSources C API（符号在 Carbon.framework；OSStatus = i32）。
// FFI 签名统一用 *const c_void，配合 core-foundation 类型封装的
// as_concrete_TypeRef()/wrap_under_* 使用，不在签名里重复声明 CF 类型。
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCreateInputSourceList(
        properties: *const c_void,
        include_all_installed: bool,
    ) -> *const c_void;
    fn TISGetInputSourceProperty(source: *const c_void, key: *const c_void) -> *const c_void;
    fn TISEnableInputSource(source: *const c_void) -> i32;
    fn TISSelectInputSource(source: *const c_void) -> i32;
    fn TISRegisterInputSource(location: *const c_void) -> i32;
}

// 数组遍历用裸指针 + CFArrayGetValueAtIndex（core-foundation 0.10 的
// CFArray::get 返回 ItemRef 借用包装，生命周期不适合这里）。
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> i64;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: i64) -> *const c_void;
}

/// 构造 `com.apple.inputsources` 的 Verba 父源 + Pinyin mode entry。
fn verba_entries() -> Vec<plist::Value> {
    let mut parent = plist::Dictionary::new();
    parent.insert(
        BUNDLE_ID_KEY.to_owned(),
        plist::Value::String(VERBA_SOURCE_ID.to_owned()),
    );
    parent.insert(
        INPUT_SOURCE_KIND_KEY.to_owned(),
        plist::Value::String(KEYBOARD_INPUT_METHOD_KIND.to_owned()),
    );

    let mut mode = plist::Dictionary::new();
    mode.insert(
        BUNDLE_ID_KEY.to_owned(),
        plist::Value::String(VERBA_SOURCE_ID.to_owned()),
    );
    mode.insert(
        "Input Mode".to_owned(),
        plist::Value::String(VERBA_MODE_ID.to_owned()),
    );
    mode.insert(
        INPUT_SOURCE_KIND_KEY.to_owned(),
        plist::Value::String("Input Mode".to_owned()),
    );

    vec![
        plist::Value::Dictionary(parent),
        plist::Value::Dictionary(mode),
    ]
}

/// 把 `com.apple.inputsources` plist 规范化为「Verba 父源 + Pinyin mode」两条。
///
/// 只写父源时用户级安装可用，但系统级 app 的 mode 可能仍 disabled；两条都写
/// 才能让父源/mode 同时 enable（macOS 26.5 真机复现）。先移除所有历史 Verba
/// 条目再追加，避免重复 mode。
fn ensure_verba_entries(root: &mut plist::Value) -> bool {
    if !root.as_dictionary().is_some() {
        *root = plist::Value::Dictionary(plist::Dictionary::new());
    }
    let root_dict = root
        .as_dictionary_mut()
        .expect("root 已规范化为 dictionary");
    if !root_dict.contains_key(THIRD_PARTY_INPUT_SOURCES_KEY) {
        root_dict.insert(
            THIRD_PARTY_INPUT_SOURCES_KEY.to_owned(),
            plist::Value::Array(Vec::new()),
        );
    }
    let list = root_dict
        .get_mut(THIRD_PARTY_INPUT_SOURCES_KEY)
        .expect("刚插入的 key 应存在");
    if !list.as_array().is_some() {
        *list = plist::Value::Array(Vec::new());
    }
    let entries = list.as_array_mut().expect("list 已规范化为 array");
    let before = entries.len();
    entries.retain(|value| {
        value
            .as_dictionary()
            .and_then(|d| d.get(BUNDLE_ID_KEY))
            .and_then(plist::Value::as_string)
            != Some(VERBA_SOURCE_ID)
    });
    let after_verba_removed = entries.len();
    entries.extend(verba_entries());
    before != after_verba_removed + 2
}

/// 写入指定用户 home 下的 `com.apple.inputsources` 白名单。
///
/// 只写文件、不刷新 agent：PKG postinstall 会以 root 调用本函数（package
/// sandbox 下以用户身份写 prefs 会被 PermissionDenied），随后再以 console
/// user 身份 killall cfprefsd/TextInputMenuAgent 完成刷新。
fn write_third_party_input_source_at_home(home: &Path) -> Result<bool, String> {
    let path = home.join(INPUT_SOURCES_PLIST);
    let mut root = if path.exists() {
        plist::Value::from_file(&path).map_err(|e| format!("读取 {} 失败: {e}", path.display()))?
    } else {
        plist::Value::Dictionary(plist::Dictionary::new())
    };
    let changed = ensure_verba_entries(&mut root);
    plist::to_file_binary(&path, &root)
        .map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
    Ok(changed)
}

fn refresh_input_source_agents() {
    let _ = Command::new("/usr/bin/killall").arg("cfprefsd").status();
    let _ = Command::new("/usr/bin/killall")
        .arg("TextInputMenuAgent")
        .status();
    std::thread::sleep(Duration::from_millis(500));
}

/// 写入当前用户白名单并刷新 cfprefsd / TextInputMenuAgent。
fn enable_third_party_input_source() -> Result<bool, String> {
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME 未设置".to_owned())?;
    let changed = write_third_party_input_source_at_home(Path::new(&home))?;
    refresh_input_source_agents();
    Ok(changed)
}

/// 从 verba-register 自身路径推导 Verba.app 根目录（Contents/MacOS 上两级）。
fn app_root_from_exe(exe: &Path) -> PathBuf {
    exe.parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .unwrap_or(exe)
        .to_owned()
}

/// 在全部已安装输入源中查找 Verba 并启用；返回 (是否找到, 启用返回码)。
///
/// **生命周期约束（独立审查发现并修复）**：TISInputSourceRef 是其所属
/// CFArray 的元素（get-rule），数组 CFRelease 后引用即失效——因此
/// TISEnableInputSource 必须在数组存活期内调用。本函数内完成「查找→启用」，
/// 不把源引用带出作用域，杜绝 use-after-free。
fn find_and_enable_source(select: bool) -> (bool, i32) {
    let raw = unsafe { TISCreateInputSourceList(std::ptr::null(), true) };
    if raw.is_null() {
        return (false, 0);
    }
    // 所有权交给 CFArray 封装（drop 时 CFRelease），元素用裸指针遍历。
    let _owned = unsafe { CFArray::<*const c_void>::wrap_under_create_rule(raw as CFArrayRef) };
    let id_key = CFString::new(TIS_PROPERTY_INPUT_SOURCE_ID);
    let id_key_ref = id_key.as_concrete_TypeRef() as *const c_void;
    let want = CFString::new(if select {
        VERBA_MODE_ID
    } else {
        VERBA_SOURCE_ID
    });
    for i in 0..unsafe { CFArrayGetCount(raw) } {
        let src = unsafe { CFArrayGetValueAtIndex(raw, i) };
        let prop = unsafe { TISGetInputSourceProperty(src, id_key_ref) };
        if prop.is_null() {
            continue;
        }
        // get-rule 引用经 wrap_under_get_rule 转为临时持有（crate 语义为
        // CFRetain + create-rule，drop 即释放，净零引用计数）。
        let id = unsafe { CFString::wrap_under_get_rule(prop as CFStringRef) };
        if id == want {
            let rc = unsafe { TISEnableInputSource(src) };
            if select {
                // TISSelectInputSource：把系统当前输入源切到 Verba Pinyin
                // （菜单刷新失效时的程序化兜底，修复「启用未选中」状态）。
                let sel = unsafe { TISSelectInputSource(src) };
                if sel != 0 {
                    eprintln!("警告: TISSelectInputSource 返回 {sel}");
                }
            }
            return (true, rc);
        }
    }
    (false, 0)
}

/// 注册（app 路径 → TISRegisterInputSource）并启用（源列表匹配 → TISEnableInputSource）。
/// 返回 (注册成功, 找到并尝试启用, 启用返回码)。
fn register_and_enable(app: &Path, select: bool) -> (bool, bool, i32) {
    let url = CFURL::from_path(app, true).expect("app 路径应可构造 CFURL");
    let url_ref = url.as_concrete_TypeRef() as *const c_void;
    let status = unsafe { TISRegisterInputSource(url_ref) };
    let registered = status == 0;
    if !registered {
        eprintln!(
            "警告: TISRegisterInputSource 返回 {status}（app 已就位，系统扫描注册通常仍会生效，继续尝试启用）"
        );
    }
    // 注册后源列表刷新可能有延迟：最多重试 3 次 × 1s，找到即启用。
    for attempt in 1..=3 {
        let (found, rc) = find_and_enable_source(select);
        if found {
            return (registered, true, rc);
        }
        if attempt < 3 {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    (registered, false, 0)
}

/// `--list`：只读列出全部输入源 ID 中的 Verba 匹配项（CI 冒烟，不改系统）。
fn list_sources() -> ExitCode {
    let raw = unsafe { TISCreateInputSourceList(std::ptr::null(), true) };
    if raw.is_null() {
        eprintln!("错误: TISCreateInputSourceList 失败");
        return ExitCode::from(2);
    }
    let _owned = unsafe { CFArray::<*const c_void>::wrap_under_create_rule(raw as CFArrayRef) };
    let id_key = CFString::new(TIS_PROPERTY_INPUT_SOURCE_ID);
    let id_key_ref = id_key.as_concrete_TypeRef() as *const c_void;
    let (mut total, mut matched) = (0usize, 0usize);
    for i in 0..unsafe { CFArrayGetCount(raw) } {
        let src = unsafe { CFArrayGetValueAtIndex(raw, i) };
        let prop = unsafe { TISGetInputSourceProperty(src, id_key_ref) };
        if prop.is_null() {
            continue;
        }
        let id = unsafe { CFString::wrap_under_get_rule(prop as CFStringRef) };
        let s = id.to_string();
        total += 1;
        if s.to_lowercase().contains("verba") {
            println!("{s}");
            matched += 1;
        }
    }
    println!("（共 {total} 个输入源，Verba 匹配 {matched} 个）");
    ExitCode::SUCCESS
}

fn usage() {
    eprintln!(
        "用法: verba-register [--app <Verba.app 路径>] [--select] | --list | --write-input-sources-plist [--home <用户 home>] | --help\n\
         \x20 --select：注册启用后把系统当前输入源切到 Verba\n\
         \x20 --write-input-sources-plist：仅写第三方输入源白名单（PKG postinstall 用）\n\
         \x20 无参数：注册并启用自身所在 bundle 的 Verba 输入源"
    );
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let select = args.contains(&"--select".to_owned());
    args.retain(|a| a != "--select");
    if args.first().map(|s| s.as_str()) == Some("--help") {
        usage();
        return ExitCode::SUCCESS;
    }
    if args.first().map(|s| s.as_str()) == Some("--list") {
        return list_sources();
    }
    if args.iter().any(|arg| arg == "--write-input-sources-plist") {
        let home = args
            .iter()
            .position(|arg| arg == "--home")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
        let Some(home) = home else {
            eprintln!("错误: --write-input-sources-plist 需要 --home 或 HOME");
            return ExitCode::from(2);
        };
        return match write_third_party_input_source_at_home(&home) {
            Ok(true) => {
                println!("已更新第三方输入源启用列表: {}", home.display());
                ExitCode::SUCCESS
            }
            Ok(false) => {
                println!("第三方输入源启用列表已是最新: {}", home.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("错误: {e}");
                ExitCode::from(1)
            }
        };
    }

    // 解析 --app 路径；缺省为自身所在 bundle（Contents/MacOS 上两级）。
    let app = match args.first().map(|s| s.as_str()) {
        Some("--app") => match args.get(1) {
            Some(p) => PathBuf::from(p),
            None => {
                eprintln!("错误: --app 需要路径参数");
                usage();
                return ExitCode::from(2);
            }
        },
        Some(other) => {
            eprintln!("错误: 未知参数 {other}");
            usage();
            return ExitCode::from(2);
        }
        None => match std::env::current_exe() {
            Ok(exe) => app_root_from_exe(&exe),
            Err(e) => {
                eprintln!("错误: 无法定位自身路径: {e}");
                return ExitCode::from(2);
            }
        },
    };
    if !app.join("Contents").join("Info.plist").is_file() {
        eprintln!(
            "错误: 不是 Verba.app（缺 Contents/Info.plist）: {}",
            app.display()
        );
        return ExitCode::from(2);
    }

    // 先注册一次，让 TIS 知道当前 bundle；再写第三方输入源启用白名单并刷新
    // HIToolbox，最后重新 enable/select。只调 TISEnableInputSource 在部分真机
    // 返回 noErr 但父源仍 disabled。
    let _ = register_and_enable(&app, false);
    match enable_third_party_input_source() {
        Ok(true) => println!("已更新第三方输入源启用列表（com.apple.inputsources）"),
        Ok(false) => println!("第三方输入源启用列表已是最新"),
        Err(e) => eprintln!("警告: 更新第三方输入源启用列表失败: {e}"),
    }
    let (registered, found, enable_rc) = register_and_enable(&app, select);
    if registered {
        println!("已注册输入源（TISRegisterInputSource）");
    }
    match (found, enable_rc) {
        (true, 0) => {
            println!("已启用「拾言输入法」（系统可能弹出确认，请允许）");
            ExitCode::SUCCESS
        }
        (true, rc) => {
            eprintln!(
                "警告: TISEnableInputSource 返回 {rc}，请到 系统设置 → 键盘 → 输入法 手动启用"
            );
            ExitCode::from(1)
        }
        (false, _) => {
            eprintln!(
                "未在输入源列表中找到 Verba（app 已安装到 ~/Library/Input Methods）。\n\
                 请注销并重新登录后重试，或到 系统设置 → 键盘 → 输入法 手动添加「拾言输入法」。"
            );
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_root_from_exe_walks_up_to_bundle() {
        let exe = Path::new("/x/Verba.app/Contents/MacOS/verba-register");
        assert_eq!(
            app_root_from_exe(exe),
            PathBuf::from("/x/Verba.app"),
            "应上跳两级到 Verba.app 根"
        );
    }

    #[test]
    fn app_root_from_exe_falls_back_to_exe() {
        // 非标准布局：退化为原路径，由 Info.plist 校验兜底报错。
        let exe = Path::new("/tmp/verba-register");
        assert_eq!(app_root_from_exe(exe), PathBuf::from("/tmp/verba-register"));
    }

    #[test]
    fn third_party_input_sources_adds_verba_parent_and_mode() {
        let mut root = plist::Value::Dictionary(plist::Dictionary::new());
        assert!(ensure_verba_entries(&mut root));
        let entries = root
            .as_dictionary()
            .and_then(|d| d.get(THIRD_PARTY_INPUT_SOURCES_KEY))
            .and_then(plist::Value::as_array)
            .expect("应有第三方输入源数组");
        assert_eq!(entries.len(), 2, "父源 + Pinyin mode");
        assert!(entries.iter().any(|value| {
            value
                .as_dictionary()
                .and_then(|entry| entry.get(INPUT_SOURCE_KIND_KEY))
                .and_then(plist::Value::as_string)
                == Some(KEYBOARD_INPUT_METHOD_KIND)
        }));
        assert!(entries.iter().any(|value| {
            let Some(entry) = value.as_dictionary() else {
                return false;
            };
            entry.get(BUNDLE_ID_KEY).and_then(plist::Value::as_string) == Some(VERBA_SOURCE_ID)
                && entry.get("Input Mode").and_then(plist::Value::as_string) == Some(VERBA_MODE_ID)
        }));
    }

    #[test]
    fn third_party_input_sources_preserves_others_and_deduplicates_verba() {
        let mut root = plist::Value::Dictionary(plist::Dictionary::new());
        let mut other = plist::Dictionary::new();
        other.insert(
            BUNDLE_ID_KEY.to_owned(),
            plist::Value::String("example.other.inputmethod".to_owned()),
        );
        other.insert(
            INPUT_SOURCE_KIND_KEY.to_owned(),
            plist::Value::String(KEYBOARD_INPUT_METHOD_KIND.to_owned()),
        );
        let mut old_mode = plist::Dictionary::new();
        old_mode.insert(
            BUNDLE_ID_KEY.to_owned(),
            plist::Value::String(VERBA_SOURCE_ID.to_owned()),
        );
        old_mode.insert(
            "Input Mode".to_owned(),
            plist::Value::String(VERBA_MODE_ID.to_owned()),
        );
        old_mode.insert(
            INPUT_SOURCE_KIND_KEY.to_owned(),
            plist::Value::String("Input Mode".to_owned()),
        );
        root.as_dictionary_mut().unwrap().insert(
            THIRD_PARTY_INPUT_SOURCES_KEY.to_owned(),
            plist::Value::Array(vec![
                plist::Value::Dictionary(other),
                plist::Value::Dictionary(old_mode),
            ]),
        );
        assert!(ensure_verba_entries(&mut root));
        let entries = root
            .as_dictionary()
            .and_then(|d| d.get(THIRD_PARTY_INPUT_SOURCES_KEY))
            .and_then(plist::Value::as_array)
            .unwrap();
        assert_eq!(entries.len(), 3, "其他输入源保留，Verba 规范化为父源+mode");
        assert!(entries.iter().any(|v| {
            v.as_dictionary()
                .and_then(|d| d.get(BUNDLE_ID_KEY))
                .and_then(plist::Value::as_string)
                == Some("example.other.inputmethod")
        }));
    }
}
