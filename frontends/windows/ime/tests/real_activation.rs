//! 真实 TSF 激活测试：让 TSF 通过语言档案激活我们的文本服务（加载 DLL → Activate → 挂键盘 sink），
//! 通过日志验证键盘 sink 是否挂载成功。需要 HKCU CLSID 指向本仓库 debug DLL。

use std::time::Duration;

use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::TextServices::{
    CLSID_TF_InputProcessorProfiles, CLSID_TF_ThreadMgr, ITfContext, ITfInputProcessorProfiles,
    ITfThreadMgr,
};

fn log_path() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(base).join("Verba").join("verba-ime.log")
}

#[test]
#[ignore = "本地诊断：需 HKCU CLSID 指向本仓库 DLL 且会话支持 TSF；验证真实激活下键盘 sink 挂载"]
fn real_activation_mounts_keysink() {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        assert_eq!(hr.0, 0);
        let tm: ITfThreadMgr =
            CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER).expect("tm");
        let tid = tm.Activate().expect("tm activate");

        // 建一个前台上下文（keysink 挂载需要）
        let doc = tm.CreateDocumentMgr().expect("doc");
        let mut ctx_out: Option<ITfContext> = None;
        let mut cookie = 0u32;
        doc.CreateContext(tid, 0, None, &mut ctx_out, &mut cookie)
            .expect("ctx");
        let ctx = ctx_out.expect("ctx");
        doc.Push(&ctx).expect("push");
        let _ = tm.SetFocus(&doc);

        // 清空旧日志
        let lp = log_path();
        let _ = std::fs::remove_file(&lp);

        // 通过语言档案激活 Verba（触发真实 TSF 文本服务激活）
        let profiles: ITfInputProcessorProfiles =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)
                .expect("profiles");
        for langid in [0x0409u16, 0x0804u16, 0x0404u16] {
            let r = profiles.ActivateLanguageProfile(
                &verba_ime_windows::CLSID_VERBA_TEXT_SERVICE,
                langid,
                &verba_ime_windows::PROFILE_VERBA,
            );
            println!("ActivateLanguageProfile langid=0x{langid:04x}: {r:?}");
        }

        // 等定时器重试挂载
        std::thread::sleep(Duration::from_millis(1500));

        let content = std::fs::read_to_string(&lp).unwrap_or_default();
        println!("==== verba-ime.log ====\n{content}");
        assert!(
            content.contains("键盘 sink 已挂载"),
            "日志应包含 keysink 挂载成功（实际内容见上）"
        );

        let _ = tm.Deactivate();
        CoUninitialize();
    }
}