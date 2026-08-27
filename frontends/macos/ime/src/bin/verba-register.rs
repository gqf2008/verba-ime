//! verba-register：macOS 输入源安装注册助手（用户级，无需管理员）。
//!
//! 由 DMG 内的「安装.command」在 Verba.app 拷入 `~/Library/Input Methods` 后
//! 调用：走 TextInputSources C API 把 Verba 注册为输入源并启用（系统会弹一次
//! 确认，macOS 26 已真机验证），免去手动到系统设置添加输入源的步骤。
//!
//! 用法：
//! ```text
//! verba-register [--app <Verba.app 路径>]   注册并启用（默认自身所在 bundle）
//! verba-register --list                     仅列出已注册输入源（只读，CI 冒烟）
//! ```
//!
//! 说明：TIS 的注册/启用为尽力而为——`TISRegisterInputSource` 失败不阻塞
//! （app 落位后系统扫描也会注册），启用失败或源未列出时给出手动路径并返回
//! 非零退出码，让「安装.command」能如实提示。

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};
use core_foundation::url::CFURL;

/// Verba 输入法本体的 TIS 输入源 ID（与 Info.plist 的 TISInputSourceID 一致）。
const VERBA_SOURCE_ID: &str = "dev.verba.inputmethod.Verba";
/// kTISPropertyInputSourceID（TextInputSources.h 公开常量）。
const TIS_PROPERTY_INPUT_SOURCE_ID: &str = "TISPropertyInputSourceID";

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
    fn TISRegisterInputSource(location: *const c_void) -> i32;
}

// 数组遍历用裸指针 + CFArrayGetValueAtIndex（core-foundation 0.10 的
// CFArray::get 返回 ItemRef 借用包装，生命周期不适合这里）。
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> i64;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: i64) -> *const c_void;
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
fn find_and_enable_source() -> (bool, i32) {
    let raw = unsafe { TISCreateInputSourceList(std::ptr::null(), true) };
    if raw.is_null() {
        return (false, 0);
    }
    // 所有权交给 CFArray 封装（drop 时 CFRelease），元素用裸指针遍历。
    let _owned = unsafe { CFArray::<*const c_void>::wrap_under_create_rule(raw as CFArrayRef) };
    let id_key = CFString::new(TIS_PROPERTY_INPUT_SOURCE_ID);
    let id_key_ref = id_key.as_concrete_TypeRef() as *const c_void;
    let want = CFString::new(VERBA_SOURCE_ID);
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
            return (true, unsafe { TISEnableInputSource(src) });
        }
    }
    (false, 0)
}

/// 注册（app 路径 → TISRegisterInputSource）并启用（源列表匹配 → TISEnableInputSource）。
/// 返回 (注册成功, 找到并尝试启用, 启用返回码)。
fn register_and_enable(app: &Path) -> (bool, bool, i32) {
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
        let (found, rc) = find_and_enable_source();
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
        "用法: verba-register [--app <Verba.app 路径>] | --list | --help\n\
         \x20 无参数：注册并启用自身所在 bundle 的 Verba 输入源"
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("--help") {
        usage();
        return ExitCode::SUCCESS;
    }
    if args.first().map(|s| s.as_str()) == Some("--list") {
        return list_sources();
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

    let (registered, found, enable_rc) = register_and_enable(&app);
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
}
