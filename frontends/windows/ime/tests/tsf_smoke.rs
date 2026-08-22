//! TSF 激活链路冒烟：真实 ITfThreadMgr + TextService::Activate/Deactivate。

use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::TextServices::{
    CLSID_TF_ThreadMgr, ITfContext, ITfTextInputProcessor, ITfThreadMgr,
};

use verba_ime_windows::text_service::TextService;

#[test]
fn tsf_activate_roundtrip() {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        assert_eq!(hr.0, 0, "CoInitializeEx 失败: {hr}");

        let tm: ITfThreadMgr = CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER)
            .expect("创建 TSF ThreadMgr");
        let tid = tm.Activate().expect("ThreadMgr.Activate");

        let doc = tm.CreateDocumentMgr().expect("CreateDocumentMgr");
        let mut ctx_out: Option<windows::Win32::UI::TextServices::ITfContext> = None;
        let mut cookie = 0u32;
        doc.CreateContext(tid, 0, None, &mut ctx_out, &mut cookie)
            .expect("CreateContext");
        let ctx = ctx_out.expect("context out");
        doc.Push(&ctx).expect("Push");
        let _ = tm.SetFocus(&doc);

        let svc: ITfTextInputProcessor = TextService::new().into();
        svc.Activate(&tm, tid)
            .expect("TextService.Activate（挂键盘 sink + 建定时器窗口）");
        svc.Deactivate().expect("TextService.Deactivate");

        let _ = tm.Deactivate();
        CoUninitialize();
    }
}

// ---- 打字上屏 / preedit→提交 集成验证 ----

use verba_core::machine::Action;
use windows::core::implement;
use windows::Win32::UI::TextServices::{
    ITfEditSession, ITfEditSession_Impl, TF_ANCHOR_END, TF_ES_READ, TF_ES_SYNC,
};

/// 读回文档全文（需读编辑会话）。
#[implement(ITfEditSession)]
struct ReadTextSession {
    context: windows::Win32::UI::TextServices::ITfContext,
    out: *mut String,
}
// SAFETY: 同步编辑会话（TF_ES_SYNC）内唯一访问 out。
unsafe impl Send for ReadTextSession {}
impl ITfEditSession_Impl for ReadTextSession_Impl {
    fn DoEditSession(&self, ec: u32) -> windows::core::Result<()> {
        unsafe {
            let start = self.context.GetStart(ec)?;
            let end = self.context.GetEnd(ec)?;
            start.ShiftEndToRange(ec, &end, TF_ANCHOR_END)?;
            let mut buf = [0u16; 512];
            let mut actual = 0u32;
            start.GetText(ec, 0, &mut buf, &mut actual)?;
            *self.out = String::from_utf16_lossy(&buf[..actual as usize]);
            Ok(())
        }
    }
}

fn read_context_text(context: &ITfContext, clientid: u32) -> String {
    let mut text = String::new();
    let session: ITfEditSession = ReadTextSession {
        context: context.clone(),
        out: &mut text,
    }
    .into();
    let hr = unsafe { context.RequestEditSession(clientid, &session, TF_ES_SYNC | TF_ES_READ) }
        .expect("RequestEditSession(read)");
    hr.ok().expect("编辑会话成功");
    text
}

#[test]
fn tsf_commit_and_preedit() {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        assert_eq!(hr.0, 0, "CoInitializeEx 失败: {hr}");
        let tm: ITfThreadMgr =
            CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER).expect("ThreadMgr");
        let tid = tm.Activate().expect("ThreadMgr.Activate");
        let doc = tm.CreateDocumentMgr().expect("DocumentMgr");
        let mut ctx_out: Option<windows::Win32::UI::TextServices::ITfContext> = None;
        let mut cookie = 0u32;
        doc.CreateContext(tid, 0, None, &mut ctx_out, &mut cookie)
            .expect("CreateContext");
        let ctx = ctx_out.expect("context");
        doc.Push(&ctx).expect("Push");
        let _ = tm.SetFocus(&doc);

        let svc_struct = verba_ime_windows::text_service::TextService::new();
        let data = svc_struct.data.clone();
        let svc: ITfTextInputProcessor = svc_struct.into();
        svc.Activate(&tm, tid).expect("Activate");

        // 1) 直接上屏
        verba_ime_windows::text_service::apply_action(
            &data,
            &ctx,
            Action::CommitImmediate("Hello".into()),
        )
        .expect("commit");
        assert_eq!(read_context_text(&ctx, tid), "Hello");

        // 2) // 进入 preedit（组合），随后提交结果
        verba_ime_windows::text_service::apply_action(
            &data,
            &ctx,
            Action::EnterPrompt {
                preedit: "//".into(),
            },
        )
        .expect("enter prompt");

        assert!(data.composition.borrow().is_some(), "preedit 应有活动组合");
        verba_ime_windows::text_service::apply_action(
            &data,
            &ctx,
            Action::CommitResult {
                text: "翻译完成".into(),
            },
        )
        .expect("commit result");
        assert_eq!(read_context_text(&ctx, tid), "Hello翻译完成");

        // 3) 真实按键路径：普通模式输入字符 'H'（走 ToUnicodeEx → machine → 上屏）
        let eaten_h = verba_ime_windows::text_service::handle_key_down(
            &data,
            windows::Win32::UI::Input::KeyboardAndMouse::VK_H.0 as u32,
            0x1E << 16,
        )
        .expect("handle_key_down(H)");
        assert_eq!(eaten_h, true, "普通模式可打印字符应被吞并上屏");
        let after_h = read_context_text(&ctx, tid);
        assert_eq!(after_h, "Hello翻译完成h", "ToUnicodeEx 无 Shift 应为小写");

        // 4) 普通模式下按 Enter → 不吞键
        let eaten = verba_ime_windows::text_service::handle_key_down(
            &data,
            windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN.0 as u32,
            0,
        )
        .expect("handle_key_down");
        assert_eq!(eaten, false, "普通模式 Enter 不应被吞");

        svc.Deactivate().expect("Deactivate");
        let _ = tm.Deactivate();
        CoUninitialize();
    }
}
