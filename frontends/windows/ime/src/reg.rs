//! 注册表与 TSF 类别注册。
//!
//! CLSID 写 HKCU\Software\Classes（免管理员，开发方便）；
//! TSF 档案与类别走 TSF 官方 API（ITfInputProcessorProfiles / ITfCategoryMgr）。

use std::path::PathBuf;

use windows::core::{Error, Result, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::Globalization::LocaleNameToLCID;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE,
    REG_CREATE_KEY_DISPOSITION, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, ITfCategoryMgr,
    ITfInputProcessorProfiles, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT, GUID_TFCAT_TIP_KEYBOARD,
};

use crate::dll::module_handle;
use crate::guids::{CLSID_VERBA_TEXT_SERVICE, PROFILE_VERBA, TEXT_SERVICE_NAME};

/// 初始化 COM（仅当本线程尚未初始化时；调用方需在结束时 CoUninitialize）。
fn co_init() -> bool {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).0 == 0 }
}

fn clsid_string() -> String {
    let g = CLSID_VERBA_TEXT_SERVICE;
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7],
    )
}

/// 获取当前 DLL 路径。
pub fn dll_path() -> Result<PathBuf> {
    unsafe {
        let mut buf = vec![0u16; 1024];
        let len = GetModuleFileNameW(Some(module_handle()), &mut buf);
        if len == 0 {
            return Err(Error::from_thread());
        }
        buf.truncate(len as usize);
        Ok(PathBuf::from(String::from_utf16_lossy(&buf)))
    }
}

fn set_reg_string(hkey: HKEY, subkey: &str, name: &str, value: &str) -> Result<()> {
    unsafe {
        let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
        let mut opened = HKEY::default();
        let mut disp = REG_CREATE_KEY_DISPOSITION(0);
        let r = RegCreateKeyExW(
            hkey,
            PCWSTR(subkey_w.as_ptr()),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut opened,
            Some(&mut disp),
        );
        r.ok()?;
        let value_w: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let data: Vec<u8> = value_w.iter().flat_map(|u| u.to_le_bytes()).collect();
        RegSetValueExW(opened, PCWSTR(name_w.as_ptr()), None, REG_SZ, Some(&data)).ok()?;
        let _ = RegCloseKey(opened);
        Ok(())
    }
}

/// 在指定 hive 注册 CLSID。
fn register_clsid_at(hkey: HKEY, module_path: &str) -> Result<()> {
    let clsid = clsid_string();
    let clsid_path = format!(r"Software\Classes\CLSID\{clsid}");
    set_reg_string(hkey, &clsid_path, "", TEXT_SERVICE_NAME)?;
    set_reg_string(
        hkey,
        &format!(r"{clsid_path}\InprocServer32"),
        "",
        module_path,
    )?;
    set_reg_string(
        hkey,
        &format!(r"{clsid_path}\InprocServer32"),
        "ThreadingModel",
        "Apartment",
    )?;
    Ok(())
}

/// 注册 CLSID：优先 HKLM（安装程序/管理员），失败回退 HKCU（非管理员开发）。
pub fn register_clsid(module_path: &str) -> Result<()> {
    match register_clsid_at(HKEY_LOCAL_MACHINE, module_path) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("HKLM CLSID 注册失败（回退 HKCU）: {e}");
            register_clsid_at(HKEY_CURRENT_USER, module_path)
        }
    }
}

/// 注销 CLSID（HKLM + HKCU 都删，忽略不存在）。
pub fn unregister_clsid() -> Result<()> {
    unsafe {
        let subkey: Vec<u16> = format!(r"Software\Classes\CLSID\{}", clsid_string())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        for hkey in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            let _ = RegDeleteTreeW(hkey, PCWSTR(subkey.as_ptr()));
        }
        Ok(())
    }
}

/// 注册语言档案（TSF API）——按用户已安装的输入语言注册。
pub fn register_profiles(module_path: &str) -> Result<()> {
    unsafe {
        let profiles: ITfInputProcessorProfiles =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| {
                    eprintln!("CoCreateInstance profiles 失败: {e}");
                    e
                })?;
        profiles.Register(&CLSID_VERBA_TEXT_SERVICE).map_err(|e| {
            eprintln!("Register 失败: {e}");
            e
        })?;
        // 字符串必须以 null 结尾，否则 TSF 会越界读取（实测 IconFile 被读进相邻内存）。
        let desc: Vec<u16> = TEXT_SERVICE_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let module: Vec<u16> = module_path.encode_utf16().chain(std::iter::once(0)).collect();
        let langids = user_lang_ids();
        let mut any_ok = false;
        let mut last_err: Option<windows::core::Error> = None;
        for langid in langids {
            match profiles.AddLanguageProfile(
                &CLSID_VERBA_TEXT_SERVICE,
                langid,
                &PROFILE_VERBA,
                &desc,
                &module,
                0,
            ) {
                Ok(()) => {
                    any_ok = true;
                    log::info!("已注册语言档案 langid=0x{langid:04x}");
                }
                Err(e) => {
                    eprintln!("AddLanguageProfile langid=0x{langid:04x} 失败: {e}");
                    last_err = Some(e);
                }
            }
        }
        if any_ok {
            Ok(())
        } else {
            Err(last_err.unwrap_or_else(|| Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32))))
        }
    }
}


/// 读取用户已安装的输入语言 LANGID 列表（HKCU\Control Panel\International\User Profile\Languages）。
/// 读取失败时回退常用语言：en-US / zh-CN / zh-TW。
fn user_lang_ids() -> Vec<u16> {
    let mut ids: Vec<u16> = Vec::new();
    unsafe {
        let subkey: Vec<u16> = r"Control Panel\International\User Profile"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let name: Vec<u16> = "Languages".encode_utf16().chain(std::iter::once(0)).collect();
        let mut hkey = HKEY::default();
        let opened = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            Some(0),
            KEY_READ,
            &mut hkey,
        )
        .is_ok();
        if opened {
            let mut size = 0u32;
            if RegQueryValueExW(hkey, PCWSTR(name.as_ptr()), None, None, None, Some(&mut size)).is_ok()
            {
                let mut buf = vec![0u16; (size as usize).div_ceil(2) + 2];
                let mut actual = size;
                if RegQueryValueExW(
                    hkey,
                    PCWSTR(name.as_ptr()),
                    None,
                    None,
                    Some(buf.as_mut_ptr() as *mut u8),
                    Some(&mut actual),
                )
                .is_ok()
                {
                    let all = String::from_utf16_lossy(&buf);
                    for tag in all.split('\0') {
                        if tag.is_empty() {
                            continue;
                        }
                        if let Some(langid) = language_tag_to_langid(tag) {
                            if !ids.contains(&langid) {
                                ids.push(langid);
                            }
                        }
                    }
                }
            }
            let _ = RegCloseKey(hkey);
        }
    }
    if ids.is_empty() {
        vec![0x0409, 0x0804, 0x0404] // en-US / zh-CN / zh-TW 兜底
    } else {
        ids
    }
}

/// 语言标签 → LANGID（低 16 位 LCID）。
fn language_tag_to_langid(tag: &str) -> Option<u16> {
    unsafe {
        let tag_w: Vec<u16> = tag.encode_utf16().chain(std::iter::once(0)).collect();
        let lcid = LocaleNameToLCID(PCWSTR(tag_w.as_ptr()), 0);
        (lcid != 0).then_some((lcid & 0xFFFF) as u16)
    }
}

/// 注册 TSF 类别（键盘输入法 + 沉浸式支持）。
pub fn register_categories() -> Result<()> {
    unsafe {
        let catmgr: ITfCategoryMgr =
            CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER).map_err(|e| {
                eprintln!("CoCreateInstance catmgr 失败: {e}");
                e
            })?;
        catmgr
            .RegisterCategory(
                &CLSID_VERBA_TEXT_SERVICE,
                &GUID_TFCAT_TIP_KEYBOARD,
                &CLSID_VERBA_TEXT_SERVICE,
            )
            .map_err(|e| {
                eprintln!("RegisterCategory keyboard 失败: {e}");
                e
            })?;
        catmgr.RegisterCategory(
            &CLSID_VERBA_TEXT_SERVICE,
            &GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
            &CLSID_VERBA_TEXT_SERVICE,
        )?;
        Ok(())
    }
}

/// 全部注册（DllRegisterServer 使用）。
pub fn register() -> Result<()> {
    let module = dll_path()?;
    let module = module.to_string_lossy().into_owned();
    register_clsid(&module)?;
    let initialized = co_init();
    let r = register_profiles(&module).and_then(|_| register_categories());
    if initialized {
        unsafe {
            CoUninitialize();
        }
    }
    r
}

/// 全部注销。
pub fn unregister() -> Result<()> {
    unregister_clsid()?;
    let initialized = co_init();
    let r = unsafe {
        let profiles: ITfInputProcessorProfiles =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?;
        profiles.Unregister(&CLSID_VERBA_TEXT_SERVICE)
    };
    if initialized {
        unsafe {
            CoUninitialize();
        }
    }
    r
}

/// 供 verba-reg 工具使用：按给定 DLL 路径注册。
pub fn register_with_path(module_path: &str) -> Result<()> {
    register_clsid(module_path)?;
    let initialized = co_init();
    let r = register_profiles(module_path).and_then(|_| register_categories());
    if initialized {
        unsafe {
            CoUninitialize();
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_tag_to_langid_maps_common_tags() {
        assert_eq!(language_tag_to_langid("en-US"), Some(0x0409));
        assert_eq!(language_tag_to_langid("zh-CN"), Some(0x0804));
        assert_eq!(language_tag_to_langid("zh-Hans-CN"), Some(0x0804));
        assert_eq!(language_tag_to_langid("zh-TW"), Some(0x0404));
        assert_eq!(language_tag_to_langid("bogus-tag"), None);
    }

    #[test]
    fn user_lang_ids_reads_or_falls_back() {
        let ids = user_lang_ids();
        assert!(!ids.is_empty(), "语言列表不应为空");
        // 本机语言列表含 zh-Hans-CN → 应含 0x0804
        if ids.len() > 1 {
            assert!(
                ids.contains(&0x0804),
                "多语言环境应包含 zh-CN(0x0804)，实际: {ids:?}"
            );
        }
    }
}