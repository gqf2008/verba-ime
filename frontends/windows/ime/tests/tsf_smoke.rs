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

/// 验证「// 发起 AI → 流式 preedit → Enter 上屏」的定时器链路（不经 daemon，直接注入事件）。
#[test]
fn tsf_streaming_preedit() {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        assert_eq!(hr.0, 0);
        let tm: ITfThreadMgr =
            CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER).expect("ThreadMgr");
        let tid = tm.Activate().expect("Activate");
        let doc = tm.CreateDocumentMgr().expect("DocumentMgr");
        let mut ctx_out: Option<ITfContext> = None;
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
        *data.context.borrow_mut() = Some(ctx.clone());

        // 状态机直接进入 Streaming（// 翻译 → Enter），不触发真实 daemon 线程
        {
            let mut m = data.machine.borrow_mut();
            m.feed_char('/');
            m.feed_char('/');
            m.feed_char('翻');
            m.feed_char('译');
            m.feed_enter();
            assert!(matches!(
                m.state(),
                verba_core::machine::MachineState::Streaming
            ));
        }

        // 注入流式事件（定时器路径）
        {
            let mut q = data.chunks.lock().unwrap();
            q.push_back(verba_protos::StreamEvent {
                id: 1,
                kind: Some(verba_protos::stream_event::Kind::Chunk(
                    verba_protos::Chunk { text: "你".into() },
                )),
            });
            q.push_back(verba_protos::StreamEvent {
                id: 1,
                kind: Some(verba_protos::stream_event::Kind::Chunk(
                    verba_protos::Chunk { text: "好".into() },
                )),
            });
            q.push_back(verba_protos::StreamEvent {
                id: 1,
                kind: Some(verba_protos::stream_event::Kind::Final(
                    verba_protos::Final {
                        text: "你好".into(),
                    },
                )),
            });
        }
        data.on_timer();
        assert_eq!(
            read_context_text(&ctx, tid),
            "你好",
            "流式 preedit 应实时进入组合"
        );

        // Enter 提交
        let eaten = verba_ime_windows::text_service::handle_key_down(
            &data,
            windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN.0 as u32,
            0,
        )
        .expect("handle_key_down(Enter)");
        assert_eq!(eaten, true, "流式结果就绪后 Enter 应提交");
        assert_eq!(read_context_text(&ctx, tid), "你好", "Enter 后结果上屏");

        svc.Deactivate().expect("Deactivate");
        let _ = tm.Deactivate();
        CoUninitialize();
    }
}

#[test]
fn should_claim_key_idle_only_slash() {
    use verba_core::machine::MachineState;
    use verba_ime_windows::text_service::should_claim_key;
    // Idle：只认领 `/`（VK_OEM_2=0xBF），其它字母/数字不认领（直通）
    assert!(should_claim_key(MachineState::Idle, 0xBF, 0x35 << 16));
    assert!(!should_claim_key(MachineState::Idle, 0x48, 0x23 << 16)); // 'h'
    assert!(!should_claim_key(MachineState::Idle, 0x0D, 0x1C << 16)); // Enter
    assert!(!should_claim_key(MachineState::Idle, 0x08, 0x0E << 16)); // Backspace
    assert!(!should_claim_key(MachineState::Idle, 0x11, 0x1D << 16)); // Ctrl
}

#[test]
fn should_claim_key_composition_claims_all() {
    use verba_core::machine::MachineState;
    use verba_ime_windows::text_service::should_claim_key;
    // 组合/提示词态：可打印字符 + 控制键都认领
    for st in [
        MachineState::PendingSlash,
        MachineState::Prompt,
        MachineState::Streaming,
        MachineState::ResultReady,
    ] {
        assert!(should_claim_key(st, 0xBF, 0x35 << 16), "state {st:?} slash");
        assert!(should_claim_key(st, 0x48, 0x23 << 16), "state {st:?} letter");
        assert!(should_claim_key(st, 0x0D, 0x1C << 16), "state {st:?} Enter");
        assert!(should_claim_key(st, 0x08, 0x0E << 16), "state {st:?} Backspace");
        assert!(!should_claim_key(st, 0x11, 0x1D << 16), "state {st:?} Ctrl 不认领");
        assert!(!should_claim_key(st, 0x25, 0x4B << 16), "state {st:?} 方向键不认领");
    }
}
