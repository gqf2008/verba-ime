//! Windows 剪贴板文本写入（原生 Win32，CF_UNICODETEXT）。
//!
//! 用于把 OCR / 快捷短语结果复制到剪贴板，便于复用。仅 Windows 调用。

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// 把 UTF-8 文本写入系统剪贴板（CF_UNICODETEXT，含 NUL 终止）。
///
/// # Safety
/// 使用 Win32 剪贴板 API；`GlobalAlloc`/`SetClipboardData` 在 windows 0.62 返回 Result；
/// 剪贴板被占用时也返回 Err，不阻塞。`SetClipboardData` 成功后系统接管句柄，不得再 GlobalFree。
pub fn set_text(text: &str) -> std::result::Result<(), String> {
    let err = |e: windows::core::Error| e.to_string();
    unsafe {
        OpenClipboard(None).map_err(err)?;
        let _ = EmptyClipboard();

        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);
        let bytes = wide.len() * 2;

        let h: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(err)?;
        let ptr = GlobalLock(h);
        if ptr.is_null() {
            let _ = GlobalFree(Some(h));
            let _ = CloseClipboard();
            return Err("GlobalLock 失败".into());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, bytes);
        let _ = GlobalUnlock(h);

        SetClipboardData(CF_UNICODETEXT.0.into(), Some(HANDLE(h.0))).map_err(|e| {
            let _ = GlobalFree(Some(h));
            let _ = CloseClipboard();
            format!("SetClipboardData 失败: {e}")
        })?;
        let _ = CloseClipboard();
    }
    Ok(())
}

/// 便捷：忽略失败，仅日志。
pub fn set_text_quiet(text: &str) {
    if let Err(e) = set_text(text) {
        log::warn!("复制到剪贴板失败: {e}");
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn utf16_encoding_has_null_terminator() {
        let mut wide: Vec<u16> = "你好A".encode_utf16().collect();
        wide.push(0);
        assert_eq!(wide.len(), 4); // 你好(2) + A(1) + NUL(1)
        assert_eq!(wide[wide.len() - 1], 0);
    }
}
