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
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_WRITE, REG_CREATE_KEY_DISPOSITION, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, ITfCategoryMgr,
    ITfInputProcessorProfiles, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT, GUID_TFCAT_TIP_KEYBOARD,
};

use crate::dll::module_handle;
use crate::guids::{CLSID_VERBA_TEXT_SERVICE, LANG_ID, PROFILE_VERBA, TEXT_SERVICE_NAME};

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

/// 注册 CLSID（HKCU）。
pub fn register_clsid(module_path: &str) -> Result<()> {
    let clsid = clsid_string();
    set_reg_string(
        HKEY_CURRENT_USER,
        &format!(r"Software\Classes\CLSID\{clsid}"),
        "",
        TEXT_SERVICE_NAME,
    )?;
    set_reg_string(
        HKEY_CURRENT_USER,
        &format!(r"Software\Classes\CLSID\{clsid}\InprocServer32"),
        "",
        module_path,
    )?;
    set_reg_string(
        HKEY_CURRENT_USER,
        &format!(r"Software\Classes\CLSID\{clsid}\InprocServer32"),
        "ThreadingModel",
        "Apartment",
    )?;
    Ok(())
}

/// 注销 CLSID。
pub fn unregister_clsid() -> Result<()> {
    unsafe {
        let subkey: Vec<u16> = format!(r"Software\Classes\CLSID\{}", clsid_string())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(subkey.as_ptr())).ok()?;
        Ok(())
    }
}

/// 注册语言档案（TSF API）。
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
        let desc: Vec<u16> = TEXT_SERVICE_NAME.encode_utf16().collect();
        let module: Vec<u16> = module_path.encode_utf16().collect();
        profiles
            .AddLanguageProfile(
                &CLSID_VERBA_TEXT_SERVICE,
                LANG_ID,
                &PROFILE_VERBA,
                &desc,
                &module,
                0,
            )
            .map_err(|e| {
                eprintln!("AddLanguageProfile 失败: {e}");
                e
            })?;
        Ok(())
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
