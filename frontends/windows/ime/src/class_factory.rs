//! IClassFactory：创建 TextService。

use std::ffi::c_void;

use windows::core::{implement, Error, IUnknown, Interface, Ref, Result, GUID};
use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_NOINTERFACE};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::UI::TextServices::ITfTextInputProcessor;

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
        // 放行 IUnknown（CoCreateInstance 常先请求）与 ITfTextInputProcessor（TSF 基接口）。
        let requested = unsafe { *riid };
        if requested != ITfTextInputProcessor::IID && requested != IUnknown::IID {
            return Err(Error::from_hresult(E_NOINTERFACE));
        }

        let text_service: ITfTextInputProcessor = TextService::new().into();
        // SAFETY: COM 智能指针转裸指针，所有权移交 COM 调用方。
        unsafe {
            *ppvobject =
                std::mem::transmute::<ITfTextInputProcessor, *mut std::ffi::c_void>(text_service);
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
