//! IClassFactory：创建 TextService。

use std::ffi::c_void;

use windows::core::{implement, Error, IUnknown, Interface, Ref, Result, GUID};
use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_NOINTERFACE};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::UI::TextServices::{
    ITfDisplayAttributeProvider, ITfTextInputProcessor, ITfTextInputProcessorEx,
};

use crate::dll;
use crate::text_service::TextService;

#[implement(IClassFactory)]
pub struct VerbaClassFactory;

impl VerbaClassFactory {
    pub fn new() -> Self {
        Self
    }
}

impl IClassFactory_Impl for VerbaClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        if !punkouter.is_null() {
            return Err(Error::from_hresult(CLASS_E_NOAGGREGATION));
        }
        unsafe {
            *ppvobject = std::ptr::null_mut();
        }
        if riid.is_null() || ppvobject.is_null() {
            return Err(Error::from_hresult(E_NOINTERFACE));
        }
        // 放行 IUnknown（CoCreateInstance 常先请求）、TIP 基接口
        // （现代 Windows 可能直接请求 ITfTextInputProcessorEx）与
        // ITfDisplayAttributeProvider（组合下划线：TSF manager 实例化
        // provider 时 CoCreateInstance(CLSID) 后 QueryInterface 该接口）。
        let requested = unsafe { *riid };
        if requested != ITfTextInputProcessor::IID
            && requested != ITfTextInputProcessorEx::IID
            && requested != ITfDisplayAttributeProvider::IID
            && requested != IUnknown::IID
        {
            return Err(Error::from_hresult(E_NOINTERFACE));
        }

        // 按请求的 riid 返回对应接口指针（同一 TextService 对象多接口，
        // 但 COM 调用方按 riid 使用 vtable，必须返回匹配的接口类型）。
        unsafe {
            if requested == ITfDisplayAttributeProvider::IID {
                let provider: ITfDisplayAttributeProvider = TextService::new().into();
                *ppvobject = std::mem::transmute::<
                    ITfDisplayAttributeProvider,
                    *mut std::ffi::c_void,
                >(provider);
            } else {
                let text_service: ITfTextInputProcessor = TextService::new().into();
                *ppvobject = std::mem::transmute::<
                    ITfTextInputProcessor,
                    *mut std::ffi::c_void,
                >(text_service);
            }
        }
        Ok(())
    }

    fn LockServer(&self, flock: windows::core::BOOL) -> Result<()> {
        if flock != false {
            dll::add_ref();
        } else {
            dll::release_ref();
        }
        Ok(())
    }
}
