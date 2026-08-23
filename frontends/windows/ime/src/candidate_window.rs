//! Windows 候选窗（置顶 popup）：`CpuCandidateRenderer`（tiny-skia+cosmic-text）
//! 渲染为 RGBA 缓冲，GDI StretchDIBits blit 到窗口。

use std::sync::OnceLock;

use verba_candidate::renderer::{window_size, CpuCandidateRenderer};
use verba_candidate::CandidateWindowController;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateRoundRectRgn, GetDC, GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SetWindowRgn,
    StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SetWindowPos, ShowWindow,
    HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, WINDOW_EX_STYLE,
    WINDOW_STYLE, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
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

    /// 更新候选窗：需要显示则渲染 + 智能定位 + 显示，否则隐藏。
    /// `anchor` = (x, top, bottom)，x 为组合起点横坐标、top/bottom 为组合文本行上下缘。
    pub fn update(&mut self, ctrl: &CandidateWindowController, anchor: (i32, i32, i32)) {
        if !ctrl.should_render() {
            self.hide();
            return;
        }
        let (w, h) = window_size(ctrl);
        let (px, py) = fit_position(
            anchor,
            w as i32,
            h as i32,
            monitor_work_area(anchor.0, anchor.2),
        );
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
                px,
                py,
                w as i32,
                h as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        apply_window_region(self.hwnd, w, h, ctrl.theme().corner_radius);
    }

    /// 仅移动位置（组合布局就绪后定时器重试精确定位时调用）。
    pub fn move_to(&mut self, anchor: (i32, i32, i32)) {
        let (px, py) = fit_position(
            anchor,
            self.width as i32,
            self.height as i32,
            monitor_work_area(anchor.0, anchor.2),
        );
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                px,
                py,
                self.width as i32,
                self.height as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOSIZE,
            );
        }
    }

    pub fn hide(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}

/// 智能定位：默认候选窗出现在光标正下方；下方放不下翻到上方；
/// 上下都放不下时选空间更大的一侧贴边；水平越界平移回工作区内。
/// `anchor` = (x, top, bottom)，`work` 为光标所在显示器工作区（不含任务栏）。
fn fit_position(anchor: (i32, i32, i32), w: i32, h: i32, work: RECT) -> (i32, i32) {
    let (x, top, bottom) = anchor;
    // 水平：默认与光标左对齐，越界平移进工作区（窗口比工作区宽时贴左缘）。
    let px = x.clamp(work.left, (work.right - w).max(work.left));
    // 垂直：默认正下方。
    let py = if bottom + h <= work.bottom {
        bottom
    } else if top - h >= work.top {
        top - h
    } else if top - work.top >= work.bottom - bottom {
        // 上方空间更大：贴工作区顶部。
        work.top
    } else {
        // 下方空间更大（或相等）：贴工作区底部。
        (work.bottom - h).max(work.top)
    };
    (px, py)
}

/// 光标所在显示器的工作区（不含任务栏）。
/// 给窗口设置圆角可见区域（配合渲染器圆角背景，角部透明）。
fn apply_window_region(hwnd: HWND, w: u32, h: u32, radius: u32) {
    if radius == 0 {
        return;
    }
    unsafe {
        let rgn = CreateRoundRectRgn(
            0,
            0,
            w as i32 + 1,
            h as i32 + 1,
            (radius * 2) as i32,
            (radius * 2) as i32,
        );
        if !rgn.is_invalid() {
            // SetWindowRgn 接管区域所有权（此后系统负责释放），勿手动 DeleteObject。
            let _ = SetWindowRgn(hwnd, Some(rgn), true);
        }
    }
}

fn monitor_work_area(x: i32, y: i32) -> RECT {
    unsafe {
        let pt = POINT { x, y };
        let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            mi.rcWork
        } else {
            mi.rcMonitor
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

#[cfg(test)]
mod tests {
    use super::*;

    fn work(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }

    #[test]
    fn below_when_space() {
        // 光标上方/下方都有空间 → 默认正下方。
        assert_eq!(
            fit_position((100, 480, 500), 300, 200, work(0, 0, 1920, 1040)),
            (100, 500)
        );
    }

    #[test]
    fn flip_above_when_below_overflow() {
        // 下方放不下（bottom=1000，h=200 → 1200 > 1040），上方有空间 → 翻上方。
        assert_eq!(
            fit_position((100, 950, 1000), 300, 200, work(0, 0, 1920, 1040)),
            (100, 750)
        );
    }

    #[test]
    fn top_aligned_when_both_overflow_and_more_above() {
        // 上下都放不下（h=400），上方空间 200 > 下方 100 → 贴工作区顶部。
        assert_eq!(
            fit_position((100, 200, 400), 300, 400, work(0, 0, 1920, 500)),
            (100, 0)
        );
    }

    #[test]
    fn bottom_aligned_when_both_overflow_and_more_below() {
        // 上下都放不下（h=400），下方空间 300 > 上方 50 → 贴工作区底部。
        assert_eq!(
            fit_position((100, 50, 200), 300, 400, work(0, 0, 1920, 500)),
            (100, 100)
        );
    }

    #[test]
    fn shift_left_when_right_overflow() {
        // 光标靠右：x=1800 + 300 > 1920 → 左移到 1620。
        assert_eq!(
            fit_position((1800, 480, 500), 300, 200, work(0, 0, 1920, 1040)),
            (1620, 500)
        );
    }

    #[test]
    fn clamp_left_when_negative_x() {
        assert_eq!(
            fit_position((-100, 480, 500), 300, 200, work(0, 0, 1920, 1040)),
            (0, 500)
        );
    }
}
