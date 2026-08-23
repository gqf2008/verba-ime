//! 选区截图：全屏半透明遮罩 + 鼠标拖选矩形，返回所选虚拟屏幕区域。
//!
//! 使用 UpdateLayeredWindow 做逐像素 alpha 遮罩（变暗 + 高亮选区边框），
//! 独立进程 / 独立线程运行消息循环，不阻塞宿主。

use std::ffi::c_void;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, LoadCursorW, PostQuitMessage, RegisterClassW, SetWindowLongPtrW, ShowWindow,
    UpdateLayeredWindow, CREATESTRUCTW, GWLP_USERDATA, IDC_CROSS, MSG, SW_SHOW, ULW_ALPHA,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT,
    WM_RBUTTONDOWN, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOPMOST, WS_POPUP,
};

use crate::capture::VirtualScreen;
use crate::TriggerError;

/// 屏幕选区（虚拟屏幕坐标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// 选区窗口状态（GWLP_USERDATA 指向，窗口生命周期内唯一线程访问）。
struct SelectionState {
    vs: VirtualScreen,
    mem: HDC,
    dib: HBITMAP,
    bits: *mut u8,
    dragging: bool,
    start: (i32, i32),
    current: (i32, i32),
    result: Option<ScreenRect>,
}

impl SelectionState {
    fn new(vs: VirtualScreen) -> Self {
        Self {
            vs,
            mem: HDC::default(),
            dib: HBITMAP::default(),
            bits: std::ptr::null_mut(),
            dragging: false,
            start: (0, 0),
            current: (0, 0),
            result: None,
        }
    }

    /// 创建 32bpp 表面并关联到内存 DC（窗口创建后调用一次）。
    fn init_surface(&mut self) -> Result<(), TriggerError> {
        unsafe {
            self.mem = CreateCompatibleDC(None);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: self.vs.width,
                    biHeight: -self.vs.height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            self.dib = CreateDIBSection(Some(self.mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
                .map_err(|e| TriggerError::Capture(format!("CreateDIBSection 失败: {e}")))?;
            self.bits = bits as *mut u8;
            let _ = SelectObject(self.mem, HGDIOBJ(self.dib.0));
            Ok(())
        }
    }

    fn deinit_surface(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.dib.0));
            let _ = DeleteDC(self.mem);
            self.dib = HBITMAP::default();
            self.mem = HDC::default();
            self.bits = std::ptr::null_mut();
        }
    }
}

/// 弹窗选区；Esc / 右键取消返回 Ok(None)，完成返回 Ok(Some(rect))。
pub fn select_region() -> Result<Option<ScreenRect>, TriggerError> {
    let vs = crate::capture::virtual_screen();
    if vs.width <= 0 || vs.height <= 0 {
        return Err(TriggerError::Capture("虚拟屏幕尺寸非法".into()));
    }
    let mut state = Box::new(SelectionState::new(vs));
    state.init_surface()?;
    let state_ptr = Box::into_raw(state);
    unsafe {
        let hinstance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map_err(|e| TriggerError::Capture(format!("GetModuleHandleW 失败: {e}")))?;
        let hinstance = std::mem::transmute::<
            windows::Win32::Foundation::HMODULE,
            windows::Win32::Foundation::HINSTANCE,
        >(hinstance);
        let class: Vec<u16> = "VerbaSelectionWindow"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let cursor = LoadCursorW(None, IDC_CROSS)
            .map_err(|e| TriggerError::Capture(format!("LoadCursorW 失败: {e}")))?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(sel_wndproc),
            hInstance: hinstance,
            hCursor: cursor,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_LAYERED,
            PCWSTR(class.as_ptr()),
            w!("VerbaSelection"),
            WS_POPUP,
            vs.x,
            vs.y,
            vs.width,
            vs.height,
            None,
            None,
            Some(hinstance),
            Some(state_ptr as *mut c_void),
        )
        .map_err(|e| TriggerError::Capture(format!("创建选区窗口失败: {e}")))?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        render(hwnd, &*state_ptr);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = DispatchMessageW(&msg);
        }
        let result = (*state_ptr).result.take();
        let _ = DestroyWindow(hwnd);
        (*state_ptr).deinit_surface();
        drop(Box::from_raw(state_ptr));
        Ok(result)
    }
}

/// 重绘遮罩 + 选区边框。
fn render(hwnd: HWND, state: &SelectionState) {
    unsafe {
        let w = state.vs.width as usize;
        let h = state.vs.height as usize;
        if state.bits.is_null() {
            return;
        }
        let px = std::slice::from_raw_parts_mut(state.bits, w * h * 4);
        // 半透明变暗（premultiplied BGRA）
        for p in px.chunks_exact_mut(4) {
            p[0] = 0;
            p[1] = 0;
            p[2] = 0;
            p[3] = 110;
        }
        if state.dragging {
            let (x1, y1, x2, y2) = normalize(state.start, state.current);
            let (lx, ly, rx, ry) = (
                x1 - state.vs.x,
                y1 - state.vs.y,
                x2 - state.vs.x,
                y2 - state.vs.y,
            );
            // 2px 蓝色边框
            for i in 0..=1 {
                draw_vline(&mut px[..], w, h, lx + i, ly, ry, [215, 120, 0, 255]);
                draw_vline(&mut px[..], w, h, rx - i, ly, ry, [215, 120, 0, 255]);
                draw_hline(&mut px[..], w, h, ly + i, lx, rx, [215, 120, 0, 255]);
                draw_hline(&mut px[..], w, h, ry - i, lx, rx, [215, 120, 0, 255]);
            }
        }
        let pt_dst = POINT {
            x: state.vs.x,
            y: state.vs.y,
        };
        let size = SIZE {
            cx: state.vs.width,
            cy: state.vs.height,
        };
        let pt_src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            None,
            Some(&pt_dst),
            Some(&size),
            Some(state.mem),
            Some(&pt_src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
    }
}

fn draw_hline(px: &mut [u8], w: usize, h: usize, y: i32, x1: i32, x2: i32, color: [u8; 4]) {
    if y < 0 || y >= h as i32 {
        return;
    }
    for x in x1.max(0)..=x2.min(w as i32 - 1) {
        set_px(px, w, x, y, color);
    }
}

fn draw_vline(px: &mut [u8], w: usize, h: usize, x: i32, y1: i32, y2: i32, color: [u8; 4]) {
    if x < 0 || x >= w as i32 {
        return;
    }
    for y in y1.max(0)..=y2.min(h as i32 - 1) {
        set_px(px, w, x, y, color);
    }
}

fn set_px(px: &mut [u8], w: usize, x: i32, y: i32, color: [u8; 4]) {
    let o = (y as usize * w + x as usize) * 4;
    if o + 3 < px.len() {
        px[o..o + 4].copy_from_slice(&color);
    }
}

fn normalize(a: (i32, i32), b: (i32, i32)) -> (i32, i32, i32, i32) {
    (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

fn client_pos(lparam: LPARAM) -> (i32, i32) {
    let v = lparam.0 as u32;
    (
        (v & 0xffff) as u16 as i32,
        ((v >> 16) & 0xffff) as u16 as i32,
    )
}

unsafe fn state_ptr(hwnd: HWND) -> *mut SelectionState {
    GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SelectionState
}

unsafe extern "system" fn sel_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = lparam.0 as *const CREATESTRUCTW;
            let p = (*cs).lpCreateParams;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, p as isize);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN => {
            let st = &mut *state_ptr(hwnd);
            st.dragging = true;
            st.start = client_pos(lparam);
            st.current = st.start;
            let _ = SetCapture(hwnd);
            render(hwnd, st);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let st = &mut *state_ptr(hwnd);
            if st.dragging {
                st.current = client_pos(lparam);
                render(hwnd, st);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let st = &mut *state_ptr(hwnd);
            if st.dragging {
                st.current = client_pos(lparam);
                let (x1, y1, x2, y2) = normalize(st.start, st.current);
                if x2 - x1 >= 2 && y2 - y1 >= 2 {
                    st.result = Some(ScreenRect {
                        x: st.vs.x + x1,
                        y: st.vs.y + y1,
                        width: x2 - x1,
                        height: y2 - y1,
                    });
                }
                let _ = ReleaseCapture();
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            let st = &mut *state_ptr(hwnd);
            st.result = None;
            let _ = ReleaseCapture();
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 as u32 == VK_ESCAPE.0 as u32 => {
            let st = &mut *state_ptr(hwnd);
            st.result = None;
            let _ = ReleaseCapture();
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_PAINT => LRESULT(0),
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_orders_corners() {
        assert_eq!(normalize((10, 20), (30, 5)), (10, 5, 30, 20));
        assert_eq!(normalize((30, 5), (10, 20)), (10, 5, 30, 20));
    }

    #[test]
    fn client_pos_decodes_lparam() {
        let lp = LPARAM((100 | (200 << 16)) as isize);
        assert_eq!(client_pos(lp), (100, 200));
    }
}
