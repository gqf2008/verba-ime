//! TSF 编辑会话：直接上屏 / 组合（start/update/end）。
//!
//! 采用 TF_ES_SYNC 同步编辑会话。组合起点取当前选区（ITfContext::GetSelection）。

use std::mem::ManuallyDrop;

use windows::core::{implement, Error, Interface, Param, Result};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::UI::TextServices::{
    ITfComposition, ITfCompositionSink, ITfContext, ITfContextComposition, ITfEditSession,
    ITfEditSession_Impl, ITfInsertAtSelection, INSERT_TEXT_AT_SELECTION_FLAGS, TF_ANCHOR_END,
    TF_CONTEXT_EDIT_CONTEXT_FLAGS, TF_ES_READWRITE, TF_ES_SYNC, TF_IAS_QUERYONLY, TF_SELECTION,
    TF_SELECTIONSTYLE,
};

/// 无组合状态下的直接上屏。
#[implement(ITfEditSession)]
pub struct CommitSession {
    pub text: Vec<u16>,
    pub context: ITfContext,
}

impl ITfEditSession_Impl for CommitSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        unsafe {
            let ins: ITfInsertAtSelection = self.context.cast()?;
            let range =
                ins.InsertTextAtSelection(ec, INSERT_TEXT_AT_SELECTION_FLAGS(0), &self.text)?;
            // InsertTextAtSelection 会让选区覆盖插入文本；把光标移到插入文本末尾，
            // 否则下一次组合会从错误位置开始（实测会替换掉已提交文本）。
            range.ShiftStartToRange(ec, &range, TF_ANCHOR_END)?;
            let selection = [TF_SELECTION {
                range: ManuallyDrop::new(Some(range)),
                style: TF_SELECTIONSTYLE::default(),
            }];
            self.context.SetSelection(ec, &selection)?;
            Ok(())
        }
    }
}

/// 结束组合并上屏。
#[implement(ITfEditSession)]
pub struct CommitCompositionSession {
    pub text: Vec<u16>,
    pub composition: ITfComposition,
    pub context: ITfContext,
}

impl ITfEditSession_Impl for CommitCompositionSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        unsafe {
            let range = self.composition.GetRange()?;
            range.SetText(ec, 0, &self.text)?;
            // 结束组合后把光标移到提交文本末尾（否则选区覆盖提交文本，后续输入会替换它）。
            range.ShiftStartToRange(ec, &range, TF_ANCHOR_END)?;
            let selection = [TF_SELECTION {
                range: ManuallyDrop::new(Some(range)),
                style: TF_SELECTIONSTYLE::default(),
            }];
            self.context.SetSelection(ec, &selection)?;
            self.composition.EndComposition(ec)?;
            Ok(())
        }
    }
}

/// 把新建的组合回写到栈上的 `Option<ITfComposition>`。
///
/// # SAFETY
/// `TF_ES_SYNC` 保证 `DoEditSession` 在 `RequestEditSession` 返回前完成，
/// 因此 `out` 指向的栈值在会话执行期间存活。
pub struct WriteBack(pub *mut Option<ITfComposition>);
// SAFETY: 见上，同步会话期间唯一访问。
unsafe impl Send for WriteBack {}

/// 新建组合并设置 preedit 文本。
#[implement(ITfEditSession)]
pub struct StartPreeditSession {
    pub text: Vec<u16>,
    pub context: ITfContext,
    pub sink: ITfCompositionSink,
    pub out: WriteBack,
}

impl ITfEditSession_Impl for StartPreeditSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        unsafe {
            let ins: ITfInsertAtSelection = self.context.cast()?;
            let range = ins.InsertTextAtSelection(ec, TF_IAS_QUERYONLY, &[])?;
            let comp_ctx: ITfContextComposition = self.context.cast()?;
            let new_comp = comp_ctx.StartComposition(ec, &range, &self.sink)?;
            new_comp.GetRange()?.SetText(ec, 0, &self.text)?;
            *self.out.0 = Some(new_comp);
            Ok(())
        }
    }
}

/// 更新既有组合的 preedit 文本。
#[implement(ITfEditSession)]
pub struct UpdatePreeditSession {
    pub text: Vec<u16>,
    pub composition: ITfComposition,
}

impl ITfEditSession_Impl for UpdatePreeditSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        unsafe { self.composition.GetRange()?.SetText(ec, 0, &self.text) }
    }
}

fn request_sync<P: Param<ITfEditSession>>(
    context: &ITfContext,
    clientid: u32,
    session: P,
    flags: TF_CONTEXT_EDIT_CONTEXT_FLAGS,
) -> Result<()> {
    unsafe {
        let hr = context.RequestEditSession(clientid, session, flags)?;
        hr.ok()
    }
}

/// 直接上屏（无组合）。
pub fn commit_text(context: &ITfContext, clientid: u32, text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let text: Vec<u16> = text.encode_utf16().collect();
    let session: ITfEditSession = CommitSession {
        text,
        context: context.clone(),
    }
    .into();
    request_sync(context, clientid, &session, TF_ES_SYNC | TF_ES_READWRITE)
}

/// 新建组合并设置 preedit。
pub fn start_composition(
    context: &ITfContext,
    clientid: u32,
    sink: &ITfCompositionSink,
    text: &str,
) -> Result<ITfComposition> {
    let mut out: Option<ITfComposition> = None;
    let text: Vec<u16> = text.encode_utf16().collect();
    let session: ITfEditSession = StartPreeditSession {
        text,
        context: context.clone(),
        sink: sink.clone(),
        out: WriteBack(&mut out),
    }
    .into();
    request_sync(context, clientid, &session, TF_ES_SYNC | TF_ES_READWRITE)?;
    out.ok_or_else(|| Error::from_hresult(E_FAIL))
}

/// 更新组合 preedit。
pub fn update_composition(
    context: &ITfContext,
    clientid: u32,
    composition: &ITfComposition,
    text: &str,
) -> Result<()> {
    let text: Vec<u16> = text.encode_utf16().collect();
    let session: ITfEditSession = UpdatePreeditSession {
        text,
        composition: composition.clone(),
    }
    .into();
    request_sync(context, clientid, &session, TF_ES_SYNC | TF_ES_READWRITE)
}

/// 结束组合（替换为最终文本）。
pub fn end_composition(
    context: &ITfContext,
    clientid: u32,
    composition: &ITfComposition,
    text: &str,
) -> Result<()> {
    let text: Vec<u16> = text.encode_utf16().collect();
    let session: ITfEditSession = CommitCompositionSession {
        text,
        composition: composition.clone(),
        context: context.clone(),
    }
    .into();
    request_sync(context, clientid, &session, TF_ES_SYNC | TF_ES_READWRITE)
}
