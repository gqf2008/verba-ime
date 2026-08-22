//! TSF 激活链路冒烟：真实 ITfThreadMgr + TextService::Activate/Deactivate。

use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::TextServices::{CLSID_TF_ThreadMgr, ITfTextInputProcessor, ITfThreadMgr};

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
