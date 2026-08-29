//! TSF 显示属性（组合下划线）：ITfDisplayAttributeProvider + ITfDisplayAttributeInfo。
//!
//! 应用（记事本/编辑框）通过 GUID_PROP_ATTRIBUTE 属性值（TfGuidAtom）查询
//! 显示属性，据此绘制组合文本的下划线。注册流程按微软「Providing Display
//! Attributes」规范：
//! 1. ITfCategoryMgr::RegisterCategory(CLSID, GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER, CLSID)
//! 2. 文本服务对象实现 ITfDisplayAttributeProvider（类工厂 CoCreateInstance 复用）
//! 3. 组合范围上设 GUID_PROP_ATTRIBUTE = VT_I4(TfGuidAtom(RegisterGUID(属性 GUID)))

use std::cell::Cell;

use windows::core::{implement, Error, GUID, Result};
use windows::Win32::Foundation::{COLORREF, E_INVALIDARG, E_NOTIMPL};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4};
use windows::Win32::UI::TextServices::{
    IEnumTfDisplayAttributeInfo, IEnumTfDisplayAttributeInfo_Impl, ITfCategoryMgr, ITfComposition,
    ITfContext, ITfDisplayAttributeInfo, ITfDisplayAttributeInfo_Impl, ITfDisplayAttributeProvider,
    ITfDisplayAttributeProvider_Impl, ITfProperty, CLSID_TF_CategoryMgr, GUID_PROP_ATTRIBUTE,
    GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER, TF_ATTR_TARGET_CONVERTED, TF_CT_COLORREF, TF_CT_NONE,
    TF_DA_COLOR, TF_DA_COLOR_0, TF_DISPLAYATTRIBUTE, TF_LS_SOLID,
};

/// 组合串显示属性 GUID（自定义；TfGuidAtom 的来源）。
pub const GUID_ATTR_VERBA_COMPOSITION: GUID = GUID::from_u128(0x3a2f5b8c_1d4e_4f6a_9b7c_2e8d0f1a3b4c);

/// 组合串显示属性描述：普通文本色 + 实线下划线（微软拼音风格组合标记）。
pub fn composition_attribute() -> TF_DISPLAYATTRIBUTE {
    TF_DISPLAYATTRIBUTE {
        crText: TF_DA_COLOR {
            r#type: TF_CT_COLORREF,
            Anonymous: TF_DA_COLOR_0 { cr: COLORREF(0x000000) },
        },
        crBk: TF_DA_COLOR {
            r#type: TF_CT_NONE,
            Anonymous: TF_DA_COLOR_0 { nIndex: 0 },
        },
        lsStyle: TF_LS_SOLID,
        fBoldLine: false.into(),
        crLine: TF_DA_COLOR {
            r#type: TF_CT_COLORREF,
            Anonymous: TF_DA_COLOR_0 { cr: COLORREF(0x000000) },
        },
        bAttr: TF_ATTR_TARGET_CONVERTED,
    }
}

/// 显示属性信息对象（组合下划线）。
#[implement(ITfDisplayAttributeInfo)]
struct CompositionAttributeInfo;

impl ITfDisplayAttributeInfo_Impl for CompositionAttributeInfo_Impl {
    fn GetGUID(&self) -> Result<GUID> {
        Ok(GUID_ATTR_VERBA_COMPOSITION)
    }

    fn GetDescription(&self) -> Result<windows::core::BSTR> {
        Ok(windows::core::BSTR::from("Verba 组合串下划线"))
    }

    fn GetAttributeInfo(&self, pda: *mut TF_DISPLAYATTRIBUTE) -> Result<()> {
        unsafe {
            *pda = composition_attribute();
        }
        Ok(())
    }

    fn SetAttributeInfo(&self, _pda: *const TF_DISPLAYATTRIBUTE) -> Result<()> {
        Err(Error::from_hresult(E_NOTIMPL))
    }

    fn Reset(&self) -> Result<()> {
        Ok(())
    }
}

/// 单元素枚举器（provider 仅提供组合下划线一种属性）。
#[implement(IEnumTfDisplayAttributeInfo)]
struct AttributeEnum {
    done: Cell<bool>,
}

impl IEnumTfDisplayAttributeInfo_Impl for AttributeEnum_Impl {
    fn Clone(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(AttributeEnum {
            done: Cell::new(self.done.get()),
        }
        .into())
    }

    fn Next(
        &self,
        ulcount: u32,
        rginfo: *mut Option<ITfDisplayAttributeInfo>,
        pcfetched: *mut u32,
    ) -> Result<()> {
        unsafe {
            if ulcount >= 1 && !self.done.get() {
                *rginfo = Some(CompositionAttributeInfo.into());
                if !pcfetched.is_null() {
                    *pcfetched = 1;
                }
                self.done.set(true);
            } else if !pcfetched.is_null() {
                *pcfetched = 0;
            }
        }
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        self.done.set(false);
        Ok(())
    }

    fn Skip(&self, _ulcount: u32) -> Result<()> {
        Ok(())
    }
}

/// 显示属性提供者。挂在 TextService 同一 COM 对象上（TextService 的
/// implement 块包含 ITfDisplayAttributeProvider），TSF manager 经类工厂
/// CoCreateInstance(CLSID) 后 QueryInterface 获得。
#[implement(ITfDisplayAttributeProvider)]
pub struct DisplayAttributeProvider;

impl ITfDisplayAttributeProvider_Impl for DisplayAttributeProvider_Impl {
    fn EnumDisplayAttributeInfo(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(AttributeEnum {
            done: Cell::new(false),
        }
        .into())
    }

    fn GetDisplayAttributeInfo(&self, guid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
        unsafe {
            if *guid == GUID_ATTR_VERBA_COMPOSITION {
                Ok(CompositionAttributeInfo.into())
            } else {
                Err(Error::from_hresult(E_INVALIDARG))
            }
        }
    }
}

/// 注册为显示属性提供者（幂等；注册失败仅告警，不影响文本服务激活）。
pub fn register_provider() {
    unsafe {
        match CoCreateInstance::<_, ITfCategoryMgr>(
            &CLSID_TF_CategoryMgr,
            None,
            CLSCTX_INPROC_SERVER,
        ) {
            Ok(cat) => {
                let r = cat.RegisterCategory(
                    &crate::guids::CLSID_VERBA_TEXT_SERVICE,
                    &GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
                    &crate::guids::CLSID_VERBA_TEXT_SERVICE,
                );
                if let Err(e) = r {
                    log::warn!("显示属性提供者注册失败: {e}");
                }
            }
            Err(e) => log::warn!("显示属性提供者注册失败（CategoryMgr）: {e}"),
        }
    }
}

/// 给组合范围设置显示属性（须在读写编辑会话内调用）。
/// 值 = 属性 GUID 的 TfGuidAtom（VT_I4）。组合结束时范围销毁，属性自动消失。
pub fn apply_composition_attribute(
    context: &ITfContext,
    ec: u32,
    comp: &ITfComposition,
) -> Result<()> {
    unsafe {
        let cat: ITfCategoryMgr =
            CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
        let atom = cat.RegisterGUID(&GUID_ATTR_VERBA_COMPOSITION)?;
        let prop: ITfProperty = context.GetProperty(&GUID_PROP_ATTRIBUTE)?;
        let range = comp.GetRange()?;
        let mut var = VARIANT::default();
        // VT_I4（TfGuidAtom）：嵌套 union 经 ptr::write 整体构造（避免
        // ManuallyDrop 字段的 DerefMut 写入触发析构语义）。
        unsafe {
            core::ptr::write(
                &mut var.Anonymous,
                VARIANT_0 {
                    Anonymous: core::mem::ManuallyDrop::new(VARIANT_0_0 {
                        vt: VT_I4,
                        wReserved1: 0,
                        wReserved2: 0,
                        wReserved3: 0,
                        Anonymous: VARIANT_0_0_0 { lVal: atom as i32 },
                    }),
                },
            );
        }
        prop.SetValue(ec, &range, &var)
    }
}
