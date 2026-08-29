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

        // 3) 真实按键路径：字母 'H' 进入拼音组合（被吞、进 preedit，不直接上屏）
        let eaten_h = verba_ime_windows::text_service::handle_key_down(
            &data,
            windows::Win32::UI::Input::KeyboardAndMouse::VK_H.0 as u32,
            0x1E << 16,
        )
        .expect("handle_key_down(H)");
        assert_eq!(eaten_h, true, "普通模式字母应被吞并进入拼音组合");
        assert_eq!(
            data.machine.borrow().state(),
            verba_core::machine::MachineState::Pinyin,
            "字母应进入拼音态"
        );
        let ctx_text = read_context_text(&ctx, tid);
        assert!(
            ctx_text.starts_with("Hello翻译完成"),
            "已提交部分应保持，实际 {ctx_text:?}"
        );
        // 候选已移入候选窗：preedit 只含拼音
        assert!(
            !ctx_text.contains(" 1."),
            "候选应移入候选窗（preedit 不含内联候选），实际 {ctx_text:?}"
        );
        assert!(data.composition.borrow().is_some(), "拼音应有 preedit 组合");

        // 4) 拼音态按 Esc → 取消（回 Idle）
        let eaten_esc = verba_ime_windows::text_service::handle_key_down(
            &data,
            windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE.0 as u32,
            0,
        )
        .expect("handle_key_down(Esc)");
        assert_eq!(eaten_esc, true, "拼音态 Esc 应被吞并取消组合");
        assert_eq!(
            data.machine.borrow().state(),
            verba_core::machine::MachineState::Idle
        );

        // 5) Idle 下按 Enter → 不吞键
        let eaten = verba_ime_windows::text_service::handle_key_down(
            &data,
            windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN.0 as u32,
            0,
        )
        .expect("handle_key_down");
        assert_eq!(eaten, false, "Idle 下 Enter 不应被吞");

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
            q.push_back((
                0,
                verba_protos::StreamEvent {
                    id: 1,
                    kind: Some(verba_protos::stream_event::Kind::Chunk(
                        verba_protos::Chunk { text: "你".into() },
                    )),
                },
            ));
            q.push_back((
                0,
                verba_protos::StreamEvent {
                    id: 1,
                    kind: Some(verba_protos::stream_event::Kind::Chunk(
                        verba_protos::Chunk { text: "好".into() },
                    )),
                },
            ));
            q.push_back((
                0,
                verba_protos::StreamEvent {
                    id: 1,
                    kind: Some(verba_protos::stream_event::Kind::Final(
                        verba_protos::Final {
                            text: "你好".into(),
                        },
                    )),
                },
            ));
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

/// 回归（复审 V6 / P2-2）：on_timer 的流代际过滤必须丢弃旧代际事件——
/// 已作废流的 chunk/Final 不得混入当前流会话（epoch=0 的无代际事件恒保留）。
#[test]
fn tsf_stream_epoch_filter_drops_stale_events() {
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

        // 状态机直接进入 Streaming（模拟新流已发起）
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

        // 当前代际=2：注入旧代际(1)的残留 chunk 与当前代际(2)的 chunk+Final
        data.stream_epoch
            .store(2, std::sync::atomic::Ordering::SeqCst);
        {
            let mut q = data.chunks.lock().unwrap();
            q.push_back((
                1,
                verba_protos::StreamEvent {
                    id: 1,
                    kind: Some(verba_protos::stream_event::Kind::Chunk(
                        verba_protos::Chunk { text: "旧".into() },
                    )),
                },
            ));
            q.push_back((
                2,
                verba_protos::StreamEvent {
                    id: 1,
                    kind: Some(verba_protos::stream_event::Kind::Chunk(
                        verba_protos::Chunk { text: "新".into() },
                    )),
                },
            ));
            q.push_back((
                2,
                verba_protos::StreamEvent {
                    id: 1,
                    kind: Some(verba_protos::stream_event::Kind::Final(
                        verba_protos::Final { text: "新".into() },
                    )),
                },
            ));
        }
        data.on_timer();
        assert_eq!(
            read_context_text(&ctx, tid),
            "新",
            "旧代际事件应被过滤（若混入则结果为 旧新）"
        );

        svc.Deactivate().expect("Deactivate");
        let _ = tm.Deactivate();
        CoUninitialize();
    }
}

#[test]
fn should_claim_key_idle_slash_and_letters() {
    use verba_core::machine::MachineState;
    use verba_ime_windows::text_service::should_claim_key;
    // Idle：认领 `/`（AI 触发）与字母（进入拼音组合）；数字/空格/控制键不认领（直通）
    assert!(should_claim_key(MachineState::Idle, 0xBF, 0x35 << 16)); // '/'
    assert!(should_claim_key(MachineState::Idle, 0x48, 0x23 << 16)); // 'h'
    assert!(!should_claim_key(MachineState::Idle, 0x32, 0x03 << 16)); // '2'
    assert!(!should_claim_key(MachineState::Idle, 0x20, 0x39 << 16)); // Space
    assert!(!should_claim_key(MachineState::Idle, 0x0D, 0x1C << 16)); // Enter
    assert!(!should_claim_key(MachineState::Idle, 0x08, 0x0E << 16)); // Backspace
    assert!(!should_claim_key(MachineState::Idle, 0x11, 0x1D << 16)); // Ctrl
}

#[test]
fn should_claim_key_pinyin_claims_letters_digits_space() {
    use verba_core::machine::MachineState;
    use verba_ime_windows::text_service::should_claim_key;
    // 拼音态：字母/数字/空格/控制键都认领；方向键不认领
    assert!(should_claim_key(MachineState::Pinyin, 0x48, 0x23 << 16)); // 'h'
    assert!(should_claim_key(MachineState::Pinyin, 0x32, 0x03 << 16)); // '2'
    assert!(should_claim_key(MachineState::Pinyin, 0x20, 0x39 << 16)); // Space
    assert!(should_claim_key(MachineState::Pinyin, 0x08, 0x0E << 16)); // Backspace
    assert!(should_claim_key(MachineState::Pinyin, 0x0D, 0x1C << 16)); // Enter
    assert!(!should_claim_key(MachineState::Pinyin, 0x25, 0x4B << 16)); // 方向键
    assert!(!should_claim_key(MachineState::Pinyin, 0x11, 0x1D << 16)); // Ctrl
                                                                        // Idle：字母与 `/` 认领，数字/空格不认领
    assert!(should_claim_key(MachineState::Idle, 0x48, 0x23 << 16)); // 'h'
    assert!(should_claim_key(MachineState::Idle, 0xBF, 0x35 << 16)); // '/'
    assert!(!should_claim_key(MachineState::Idle, 0x32, 0x03 << 16)); // '2'
    assert!(!should_claim_key(MachineState::Idle, 0x20, 0x39 << 16)); // Space
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
        assert!(
            should_claim_key(st, 0x48, 0x23 << 16),
            "state {st:?} letter"
        );
        assert!(should_claim_key(st, 0x0D, 0x1C << 16), "state {st:?} Enter");
        assert!(
            should_claim_key(st, 0x08, 0x0E << 16),
            "state {st:?} Backspace"
        );
        assert!(
            !should_claim_key(st, 0x11, 0x1D << 16),
            "state {st:?} Ctrl 不认领"
        );
        assert!(
            !should_claim_key(st, 0x25, 0x4B << 16),
            "state {st:?} 方向键不认领"
        );
    }
}

/// 显示属性链路冒烟（真实 TSF 环境）：TextService::Activate 注册 provider 后，
/// ITfDisplayAttributeMgr 能按属性 GUID 查到属性对象（owner = 文本服务 CLSID）
/// 且属性描述为实线下划线——组合下划线的完整注册/查询链路。
#[test]
fn tsf_display_attribute_roundtrip() {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        assert_eq!(hr.0, 0, "CoInitializeEx 失败: {hr}");

        // 激活 TextService（触发 register_provider）
        let tm: ITfThreadMgr = CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER)
            .expect("创建 TSF ThreadMgr");
        let tid = tm.Activate().expect("ThreadMgr.Activate");
        let svc: ITfTextInputProcessor = TextService::new().into();
        svc.Activate(&tm, tid)
            .expect("TextService.Activate（含显示属性 provider 注册）");

        // 经 TSF manager 查询组合属性（首次调用会 CoCreateInstance 文本服务
        // 并 QueryInterface ITfDisplayAttributeProvider）
        let mgr: windows::Win32::UI::TextServices::ITfDisplayAttributeMgr =
            CoCreateInstance(
                &windows::Win32::UI::TextServices::CLSID_TF_DisplayAttributeMgr,
                None,
                CLSCTX_INPROC_SERVER,
            )
            .expect("创建 DisplayAttributeMgr");
        let mut info: Option<windows::Win32::UI::TextServices::ITfDisplayAttributeInfo> = None;
        let mut owner = windows::core::GUID::zeroed();
        let r = mgr.GetDisplayAttributeInfo(
            &verba_ime_windows::display_attribute::GUID_ATTR_VERBA_COMPOSITION,
            &mut info,
            &mut owner,
        );
        if let Err(e) = r {
            // provider 类别注册（GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER）写
            // HKLM\SOFTWARE\Microsoft\CTF\TIP\{CLSID}\Category，普通权限
            // 下 RegisterCategory 返回 E_FAIL（真机实测 0x80004005）——类别
            // 由安装器 verba-reg（管理员）注册；未注册环境跳过（权限限制，
            // 非代码缺陷），已注册环境（安装版）继续验证完整链路。
            eprintln!("provider 类别未注册（需 verba-reg register 管理员注册）: {e}");
            svc.Deactivate().expect("TextService.Deactivate");
            let _ = tm.Deactivate();
            CoUninitialize();
            return;
        }
        let info = info.expect("属性对象");
        assert_eq!(
            owner,
            verba_ime_windows::guids::CLSID_VERBA_TEXT_SERVICE,
            "属性 owner 应为文本服务 CLSID"
        );
        let mut da = windows::Win32::UI::TextServices::TF_DISPLAYATTRIBUTE::default();
        info.GetAttributeInfo(&mut da).expect("GetAttributeInfo");
        assert_eq!(
            da.lsStyle,
            windows::Win32::UI::TextServices::TF_LS_SOLID,
            "组合属性应为实线下划线"
        );
        assert_eq!(
            da.bAttr,
            windows::Win32::UI::TextServices::TF_ATTR_TARGET_CONVERTED,
            "组合属性应为 TARGET_CONVERTED 标记"
        );

        svc.Deactivate().expect("TextService.Deactivate");
        let _ = tm.Deactivate();
        CoUninitialize();
    }
}
