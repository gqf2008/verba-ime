//! Windows 候选窗（置顶 popup）：`CpuCandidateRenderer`（tiny-skia+cosmic-text）
//! 渲染为 RGBA 缓冲，GDI StretchDIBits blit 到窗口。

use std::sync::OnceLock;

use verba_candidate::renderer::{window_size, CpuCandidateRenderer};
use verba_candidate::CandidateWindowController;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetDC, ReleaseDC, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SetWindowPos, ShowWindow,
    HWND_TOPMOST, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WINDOW_EX_STYLE, WINDOW_STYLE,
    WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::dll;

const CAND_CLASS: &str = "VerbaCandidateWindow";
static CLASS_REGISTERED: OnceLock<()> = OnceLock::new();

/// # Safety
/// 标准窗口过程：全部消息交给默认处理（GDI 直接绘制，不走 WM_PAINT）。
unsafe extern "system" fn cand_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// 置顶候选窗。
pub struct CandidateWindow {
    hwnd: HWND,
    renderer: CpuCandidateRenderer,
    width: u32,
    height: u32,
}

impl CandidateWindow {
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            let hmodule = if dll::module_handle().0.is_null() {
                windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?
            } else {
                dll::module_handle()
            };
            // HMODULE 与 HINSTANCE 同为模块基址句柄，位模式一致。
            let hinstance: windows::Win32::Foundation::HINSTANCE = std::mem::transmute::<
                windows::Win32::Foundation::HMODULE,
                windows::Win32::Foundation::HINSTANCE,
            >(hmodule);
            CLASS_REGISTERED.get_or_init(|| {
                let class: Vec<u16> = CAND_CLASS
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(cand_wndproc),
                    hInstance: hinstance,
                    lpszClassName: PCWSTR(class.as_ptr()),
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
            });
            let class: Vec<u16> = CAND_CLASS
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0),
                PCWSTR(class.as_ptr()),
                windows::core::w!("VerbaCand"),
                WINDOW_STYLE(WS_POPUP.0),
                0,
                0,
                10,
                10,
                None,
                None,
                Some(hinstance),
                None,
            )?;
            Ok(Self {
                hwnd,
                renderer: CpuCandidateRenderer::new(),
                width: 10,
                height: 10,
            })
        }
    }

    /// 更新候选窗：需要显示则渲染 + 定位 + 显示，否则隐藏。
    pub fn update(&mut self, ctrl: &CandidateWindowController, x: i32, y: i32) {
        if !ctrl.should_render() {
            self.hide();
            return;
        }
        let (w, h) = window_size(ctrl.theme(), ctrl.page_items().len());
        let out = self.renderer.render(ctrl);
        unsafe {
            // RGBA（预乘）→ BGRA（GDI 32bpp 内存序）
            let mut bgra = vec![0u8; out.pixels.len()];
            for (i, px) in out.pixels.chunks_exact(4).enumerate() {
                let o = i * 4;
                bgra[o] = px[2];
                bgra[o + 1] = px[1];
                bgra[o + 2] = px[0];
                bgra[o + 3] = px[3];
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32), // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let dc = GetDC(Some(self.hwnd));
            let _ = StretchDIBits(
                dc,
                0,
                0,
                w as i32,
                h as i32,
                0,
                0,
                w as i32,
                h as i32,
                Some(bgra.as_ptr() as *const core::ffi::c_void),
                &bmi,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            let _ = ReleaseDC(Some(self.hwnd), dc);

            if w != self.width || h != self.height {
                self.width = w;
                self.height = h;
            }
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                w as i32,
                h as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    pub fn hide(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}

impl Drop for CandidateWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
