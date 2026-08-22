//! TSF 端到端验收 v2：真实激活 Verba + 前台 docmgr/context + ITfKeystrokeMgr::KeyDown 喂键，
//! 验证「// → 组合 → LLM 流式 → Enter 提交」全链路（自包含，不依赖交互桌面）。

use std::time::Duration;

use windows::core::{implement, Interface};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::TextServices::{
    CLSID_TF_InputProcessorProfiles, CLSID_TF_ThreadMgr, ITfContext, ITfEditSession,
    ITfEditSession_Impl, ITfInputProcessorProfiles, ITfKeystrokeMgr, ITfThreadMgr, TF_ANCHOR_END,
    TF_ES_READ, TF_ES_SYNC,
};

fn log_path() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(base)
        .join("Verba")
        .join("verba-ime.log")
}

#[implement(ITfEditSession)]
struct ReadTextSession {
    context: ITfContext,
    out: *mut String,
}
// SAFETY: 同步编辑会话内唯一访问。
unsafe impl Send for ReadTextSession {}
impl ITfEditSession_Impl for ReadTextSession_Impl {
    fn DoEditSession(&self, ec: u32) -> windows::core::Result<()> {
        unsafe {
            let start = self.context.GetStart(ec)?;
            let end = self.context.GetEnd(ec)?;
            start.ShiftEndToRange(ec, &end, TF_ANCHOR_END)?;
            let mut buf = [0u16; 1024];
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
        .expect("RequestEditSession");
    hr.ok().expect("edit session");
    text
}

#[test]
#[ignore = "本地 e2e：需 HKCU CLSID 指向构建 DLL + mock/daemon"]
fn tsf_e2e_ai_chain() {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        assert_eq!(hr.0, 0);
        let lp = log_path();
        let _ = std::fs::remove_file(&lp);

        let tm: ITfThreadMgr =
            CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER).expect("tm");
        let tid = tm.Activate().expect("tm activate");

        // 前台 docmgr + context
        let doc = tm.CreateDocumentMgr().expect("doc");
        let mut ctx_out: Option<ITfContext> = None;
        let mut cookie = 0u32;
        doc.CreateContext(tid, 0, None, &mut ctx_out, &mut cookie)
            .expect("ctx");
        let ctx = ctx_out.expect("ctx");
        doc.Push(&ctx).expect("push");
        let _ = tm.SetFocus(&doc);

        // 真实激活 Verba
        let profiles: ITfInputProcessorProfiles =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)
                .expect("profiles");
        let r = profiles.ActivateLanguageProfile(
            &verba_ime_windows::CLSID_VERBA_TEXT_SERVICE,
            0x0409,
            &verba_ime_windows::PROFILE_VERBA,
        );
        println!("ActivateLanguageProfile: {r:?}");
        assert!(r.is_ok());

        std::thread::sleep(Duration::from_millis(800));

        // 通过 TSF 喂键
        let km: ITfKeystrokeMgr = tm.cast().expect("km");
        let keys: &[(u32, u32)] = &[
            (0xBF, 0x35),
            (0xBF, 0x35), // // (VK_OEM_2)
            (0x48, 0x23),
            (0x45, 0x12),
            (0x4C, 0x26),
            (0x4C, 0x26),
            (0x4F, 0x18), // hello
            (0x0D, 0x1C), // Enter（触发 LLM）
        ];
        for (vk, scan) in keys {
            let eaten = km
                .KeyDown(WPARAM(*vk as usize), LPARAM((*scan as isize) << 16))
                .expect("KeyDown");
            println!("KeyDown vk=0x{vk:02X} eaten={eaten:?}");
            std::thread::sleep(Duration::from_millis(150));
        }
        // 等流式
        std::thread::sleep(Duration::from_millis(2500));
        println!(
            "---- 流式后 context 内容: {:?}",
            read_context_text(&ctx, tid)
        );
        // Enter 提交
        let _ = km.KeyDown(WPARAM(0x0D), LPARAM(0x1Cisize << 16));
        std::thread::sleep(Duration::from_millis(500));
        let text = read_context_text(&ctx, tid);

        let log = std::fs::read_to_string(&lp).unwrap_or_default();
        println!("==== context 内容 ====\n{text}");
        println!("==== verba-ime.log ====\n{log}");

        assert!(
            text.contains("Mock LLM"),
            "context 应包含 LLM 回复（实际: {text:?}）"
        );
        assert!(log.contains("OnKeyDown"), "日志应显示按键进入 Verba");

        let _ = tm.Deactivate();
        CoUninitialize();
    }
}
