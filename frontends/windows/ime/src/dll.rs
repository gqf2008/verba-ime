//! DLL 导出与模块引用计数。

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use windows::core::{Interface, GUID};
use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, E_FAIL, E_NOINTERFACE, HMODULE, S_OK};
use windows::Win32::System::Com::IClassFactory;

use crate::class_factory::VerbaClassFactory;
use crate::guids::CLSID_VERBA_TEXT_SERVICE;
use crate::reg;

static REF_COUNT: AtomicUsize = AtomicUsize::new(0);
static DLL_MODULE: OnceLock<usize> = OnceLock::new();

/// 模块句柄（DllMain 里记录）。
pub fn module_handle() -> HMODULE {
    HMODULE(DLL_MODULE.get().copied().unwrap_or(0) as *mut core::ffi::c_void)
}

pub fn add_ref() {
    REF_COUNT.fetch_add(1, Ordering::SeqCst);
}

pub fn release_ref() {
    REF_COUNT.fetch_sub(1, Ordering::SeqCst);
}

pub fn can_unload() -> bool {
    REF_COUNT.load(Ordering::SeqCst) == 0
}

/// # Safety
/// 标准 DLL 入口；`module` 必须来自系统加载器。
#[no_mangle]
pub unsafe extern "system" fn DllMain(module: HMODULE, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == 1 {
        // DLL_PROCESS_ATTACH
        let _ = DLL_MODULE.set(module.0 as usize);
    }
    1 // TRUE
}

/// COM 导出：根据 CLSID 创建类工厂。
///
/// # Safety
/// 三个指针参数必须指向有效内存；`ppv` 接收对象指针。
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> i32 {
    *ppv = null_mut();
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return 0x80070057u32 as i32; // E_INVALIDARG
    }
    if *rclsid != CLSID_VERBA_TEXT_SERVICE {
        return CLASS_E_CLASSNOTAVAILABLE.0;
    }
    // 0.62 中 HRESULT 为 i32 别名；用常量比较。
    if *riid != IClassFactory::IID {
        return E_NOINTERFACE.0;
    }
    let factory: IClassFactory = VerbaClassFactory::new().into();
    // SAFETY: COM 智能指针是 repr(transparent) 包装，转成裸指针移交所有权。
    *ppv = std::mem::transmute::<IClassFactory, *mut std::ffi::c_void>(factory);
    S_OK.0
}

/// 注册（regsvr32 / verba-reg 调用）。
///
/// # Safety
/// 无额外约束（无指针参数）。
#[no_mangle]
pub unsafe extern "system" fn DllRegisterServer() -> i32 {
    match reg::register() {
        Ok(()) => S_OK.0,
        Err(e) => {
            log::error!("DllRegisterServer 失败: {e}");
            E_FAIL.0
        }
    }
}

/// 注销。
///
/// # Safety
/// 无额外约束（无指针参数）。
#[no_mangle]
pub unsafe extern "system" fn DllUnregisterServer() -> i32 {
    match reg::unregister() {
        Ok(()) => S_OK.0,
        Err(e) => {
            log::error!("DllUnregisterServer 失败: {e}");
            E_FAIL.0
        }
    }
}

/// 是否可卸载。
///
/// # Safety
/// 无额外约束（无指针参数）。
#[no_mangle]
pub unsafe extern "system" fn DllCanUnloadNow() -> i32 {
    if can_unload() {
        S_OK.0
    } else {
        1 // S_FALSE
    }
}
