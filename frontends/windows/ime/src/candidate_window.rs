//! Windows 候选窗（置顶 popup）：`CpuCandidateRenderer`（tiny-skia+cosmic-text）
//! 渲染为 RGBA 缓冲，GDI StretchDIBits blit 到窗口。

use std::sync::OnceLock;

use verba_candidate::renderer::{result_text_wrap_width, window_size, CpuCandidateRenderer};
use verba_candidate::CandidateWindowController;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateRoundRectRgn, GetDC, GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SetWindowRgn,
    StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, SRCCOPY,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, IsWindowVisible, RegisterClassW, SetWindowPos,
    ShowWindow, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNA,
    WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
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
    /// 已设置的窗口区域参数 (w, h, radius)；(0, 0, 0) = 尚未设置。
    /// SetWindowRgn 即使同区域也会触发表面重绘，参数未变时必须跳过。
    region_state: (u32, u32, u32),
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
                region_state: (0, 0, 0),
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
        // DPI 缩放：候选窗在物理像素坐标系渲染（DPI-aware 进程的 HWND），
        // 高分屏（Windows 缩放 > 100%）下须把逻辑主题 × scale 输出，否则
        // 窗口和文字只有期望的 1/scale 大小（实测 150% 屏上候选框小 1/3）。
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        // GetDpiForWindow 失败返回 0（窗口 DPI 上下文异常/未初始化时），
        // 按 96（1:1）兜底——绝不让 scale=0 把主题尺寸全部缩成 1px
        // （候选窗将不可见）。日志记录实际 dpi 便于真机排查。
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let mut ctrl = if (scale - 1.0).abs() > f32::EPSILON {
            ctrl.scaled(scale)
        } else {
            ctrl.clone()
        };
        // AI 结果浮层：行数在**缩放后**坐标系实测回填——scaled 的逐字段
        // round 会微调宽度/字号比，换行点可能漂一行（末行被裁或底部留白）。
        // 测量/建窗/渲染三者落在同一控制器实例上（measure_lines 与 draw_text
        // 同一套 Buffer 参数），结构性一致，不靠「两处小心同步」。
        // 非结果形态跳过（候选高度不依赖字体度量）。
        if ctrl.result_block().is_some() {
            let lines = self.renderer.measure_lines(
                ctrl.result_block().unwrap_or(""),
                ctrl.theme().font_size as f32,
                result_text_wrap_width(ctrl.theme()),
            );
            ctrl.set_result_lines(lines);
        }
        let (w, h) = window_size(&ctrl);
        let (px, py) = fit_position(
            anchor,
            w as i32,
            h as i32,
            monitor_work_area(anchor.0, anchor.2),
        );
        log::info!(
            "候选窗渲染 dpi={dpi} scale={scale} size={w}x{h} pos=({px},{py}) anchor=({},{},{})",
            anchor.0,
            anchor.1,
            anchor.2
        );
        let out = self.renderer.render(&ctrl);
        // RGBA（预乘）→ BGRA（GDI 32bpp 内存序）
        let mut bgra = vec![0u8; out.pixels.len()];
        for (i, px) in out.pixels.as_chunks::<4>().0.iter().enumerate() {
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
        unsafe {
            // 顺序（真机踩坑）：①定位+显示 ②圆角裁剪 ③最后 blit 内容。
            // SetWindowRgn(bRedraw=true) 会触发窗口表面重绘，若在 blit 之后
            // 调用会清掉刚画的内容（GDI 窗口不走 WM_PAINT，无人补画）——
            // 真机表现为候选窗时隐时现：首次显示空表面，下次击键重画才恢复。
            let pos_result = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                px,
                py,
                w as i32,
                h as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let shown = ShowWindow(self.hwnd, SW_SHOWNA);
            if self.region_state != (w, h, ctrl.theme().corner_radius) {
                apply_window_region(self.hwnd, w, h, ctrl.theme().corner_radius);
                self.region_state = (w, h, ctrl.theme().corner_radius);
            }
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
            log::info!(
                "候选窗 SetWindowPos={:?} ShowWindow(prev_visible={}) 现可见={}",
                pos_result,
                shown.as_bool(),
                IsWindowVisible(self.hwnd).as_bool()
            );
            if w != self.width || h != self.height {
                self.width = w;
                self.height = h;
            }
        }
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

    /// 窗口当前可见性（集成回归断言用：结果浮层的显示/收起契约）。
    pub fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }
}

/// 智能定位：默认候选窗出现在光标正下方；下方放不下翻到上方；
/// 上下都放不下时选空间更大的一侧贴边；水平越界平移回工作区内。
/// `anchor` = (x, top, bottom)，`work` 为光标所在显示器工作区（不含任务栏）。
///
/// 防御：部分应用的 TSF 布局实现（Windows Terminal 的 GetTextExt 实测
/// 会把滚动偏移算进坐标，光标在底部时返回 y 超出屏幕数百像素）会给出
/// 越界锚点。此时先把锚点整行平移钳进工作区（保持行高，避免逐键漂移），
/// 再按常规逻辑定位——保证候选窗永远落在工作区内（宁可贴边可见，
/// 不可丢出屏幕）。
fn fit_position(anchor: (i32, i32, i32), w: i32, h: i32, work: RECT) -> (i32, i32) {
    let (x, mut top, mut bottom) = anchor;
    // 锚点钳制（垂直）：整行越界时平移进工作区，保持行高。
    let line_h = (bottom - top).max(1);
    if bottom > work.bottom {
        bottom = work.bottom;
        top = (bottom - line_h).max(work.top);
    } else if top < work.top {
        top = work.top;
        bottom = (top + line_h).min(work.bottom);
    }
    // 水平：默认与光标左对齐，越界平移进工作区（窗口比工作区宽时贴左缘）。
    let px = x.clamp(work.left, (work.right - w).max(work.left));
    // 垂直：默认正下方；下方放不下翻到上方；上下都放不下选空间更大的一侧贴边。
    // 翻上方前须保证窗口完整可见（top 本身不得超出工作区下缘）。
    let py = if bottom + h <= work.bottom {
        bottom
    } else if top - h >= work.top && top <= work.bottom {
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

pub(crate) fn monitor_work_area(x: i32, y: i32) -> RECT {
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

/// 眼睛区域定位：默认捕捉「光标上方」屏幕（用户视线所在），
/// 上方放不下翻到下方，上下都放不下选空间更大的一侧贴边；水平越界平移回工作区。
/// `anchor` = (x, top, bottom)，`offset_y` 表示眼睛区域底部距组合上缘的向上偏移。
pub(crate) fn fit_eye_rect(
    anchor: (i32, i32, i32),
    w: i32,
    h: i32,
    offset_y: i32,
    work: RECT,
) -> (i32, i32) {
    let (x, top, bottom) = anchor;
    let px = x.clamp(work.left, (work.right - w).max(work.left));
    let off = offset_y.max(0);
    // 上方空间：眼睛区域放在组合上方（底部贴 top，再留 off 间隙）。
    let above_space = top - off - work.top;
    let below_space = work.bottom - bottom;
    let py = if h <= above_space {
        // 上方放得下：眼睛区域底部 = top - off。
        top - off - h
    } else if h <= below_space {
        // 上方放不下，翻到光标下方。
        bottom
    } else if above_space >= below_space {
        // 上方空间更大：贴工作区顶部。
        work.top
    } else {
        // 下方空间更大（或相等）：贴工作区底部。
        (work.bottom - h).max(work.top)
    };
    (px, py)
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

    /// 回归（Windows Terminal TSF 坐标 bug）：GetTextExt 返回的锚点整行落在
    /// 工作区外（光标在窗口底部时 y 超出屏幕，实测 (29,1981,2009) 超出
    /// 2560x1440 屏幕）。此前 fit_position 把候选窗定位到 y=1923/2009——
    /// 屏幕外完全不可见。现在锚点先钳进工作区（保持行高），候选窗必须
    /// 完整落在工作区内。
    #[test]
    fn anchor_below_workarea_clamped_into_work() {
        // Terminal 真实场景：work 高 1400（任务栏 40px），锚点行在 1981..2009。
        let work_rect = work(0, 0, 2560, 1400);
        let (px, py) = fit_position((29, 1981, 2009), 560, 58, work_rect);
        assert_eq!(
            (px, py),
            (29, 1314),
            "锚点钳到工作区底行上方，窗口位于该行上方"
        );
        assert!(
            py >= work_rect.top && py + 58 <= work_rect.bottom,
            "候选窗必须完整落在工作区内，实际 py={py}"
        );
        assert!(px >= 0 && px + 560 <= 2560, "水平方向也须在工作区内");
    }

    /// 锚点整行在工作区上方（极端布局异常）→ 钳进工作区后正常定位。
    #[test]
    fn anchor_above_workarea_clamped_into_work() {
        let work_rect = work(0, 0, 1920, 1040);
        let (px, py) = fit_position((100, -120, -92), 300, 200, work_rect);
        // 钳制后 top=0, bottom=28；下方放得下（28+200=228 <= 1040）→ 正下方。
        assert_eq!((px, py), (100, 28));
    }

    /// 行高大于工作区高度的极端情况：钳制后仍须落回工作区内。
    #[test]
    fn anchor_line_taller_than_workarea_still_visible() {
        let work_rect = work(0, 0, 1920, 200);
        let (_, py) = fit_position((100, 500, 2000), 300, 200, work_rect);
        assert!(
            py >= 0 && py + 200 <= 200,
            "超高行也须钳回工作区内，实际 py={py}"
        );
    }

    #[test]
    fn eye_above_when_space() {
        // 光标上方有空间 → 眼睛区域放上方（默认）。
        assert_eq!(
            fit_eye_rect((100, 700, 720), 640, 480, 0, work(0, 0, 1920, 1040)),
            (100, 220)
        );
    }

    #[test]
    fn eye_flip_below_when_above_overflow() {
        // 上方放不下（top=100，h=480 → 越界），下方有空间 → 翻下方。
        assert_eq!(
            fit_eye_rect((100, 100, 120), 640, 480, 0, work(0, 0, 1920, 1040)),
            (100, 120)
        );
    }

    #[test]
    fn eye_top_aligned_when_both_overflow_and_more_above() {
        // 上下都放不下（h=1000），上方空间 200 > 下方 100 → 贴工作区顶部。
        assert_eq!(
            fit_eye_rect((100, 200, 300), 640, 1000, 0, work(0, 0, 1920, 500)),
            (100, 0)
        );
    }

    #[test]
    fn eye_bottom_aligned_when_both_overflow_and_more_below() {
        // 上下都放不下（h=1000），下方空间更大；但窗口超高，底部贴齐被钳制回工作区顶部。
        assert_eq!(
            fit_eye_rect((100, 50, 80), 640, 1000, 0, work(0, 0, 1920, 500)),
            (100, 0)
        );
    }

    #[test]
    fn eye_shift_left_when_right_overflow() {
        // 光标靠右：x=1800 + 640 > 1920 → 左移到 1280。
        assert_eq!(
            fit_eye_rect((1800, 700, 720), 640, 480, 0, work(0, 0, 1920, 1040)),
            (1280, 220)
        );
    }

    #[test]
    fn eye_clamp_left_when_negative_x() {
        assert_eq!(
            fit_eye_rect((-100, 700, 720), 640, 480, 0, work(0, 0, 1920, 1040)),
            (0, 220)
        );
    }

    #[test]
    fn eye_respects_offset() {
        // offset=40：眼睛区域底部再往上 40px。
        assert_eq!(
            fit_eye_rect((100, 760, 780), 640, 480, 40, work(0, 0, 1920, 1040)),
            (100, 240)
        );
    }

    /// 真机自证（`cargo test -- --ignored`，CI 默认跳过）：结果浮层形态
    /// （AI 结果 / OCR 预览同一条渲染路径）的候选窗**确实合成上屏**。
    ///
    /// 起因：真机 2026-09-05 `///` 全链路正常——日志有「候选窗渲染 …
    /// 现可见=true」且持续 9s+——用户却感知「啥也没看到」。渲染层是否
    /// 真的画上了屏幕，只有截屏比对能证明，窗口日志证明不了。
    ///
    /// 方法：品红卡片背景（任何桌面背景下可判定像素来源）→ 显示 →
    /// 截主屏全屏 → 全屏扫描品红像素（对进程 DPI 感知状态免疫：不依赖
    /// GetWindowRect 与物理坐标的对齐，DPI 虚拟化只影响窗口落点，不影响
    /// 「有没有画上」）→ 断言 ①品红覆盖量可观 ②包围盒尺寸合理 ③盒内
    /// 有深色字形像素（标题 + 正文确实栅格化）。
    #[test]
    #[ignore = "真机屏幕截取（需交互桌面会话）"]
    fn overlay_window_paints_on_real_screen() {
        let theme = verba_candidate::Theme {
            background: "#FF00FF".into(),
            ..verba_candidate::Theme::default()
        };
        let mut ctrl = CandidateWindowController::new(theme);
        ctrl.set_result_block("📷 OCR 识别结果\n真机绘制自证：候选窗结果浮层文本。");
        ctrl.set_status(Some("Enter/空格/1 上屏 · Esc 取消".to_owned()));
        ctrl.show();

        // 主屏中部锚点（安静区域；fit_position 保证窗口完整落在工作区内）。
        let work_rect = monitor_work_area(200, 200);
        let mid_x = (work_rect.left + work_rect.right) / 3;
        let mid_y = (work_rect.top + work_rect.bottom) / 3;
        let anchor = (mid_x, mid_y, mid_y + 28);

        let mut cw = CandidateWindow::new().expect("创建候选窗");
        cw.update(&ctrl, anchor);
        assert!(cw.is_visible(), "update 后窗口应可见");

        // DWM 合成一帧的时间余量。
        std::thread::sleep(std::time::Duration::from_millis(400));

        let shot = verba_trigger::capture::capture_primary_screen().expect("截取主屏");
        let img = image::load_from_memory(&shot.bmp)
            .expect("解码截屏 BMP")
            .to_rgba8();
        let (iw, ih) = img.dimensions();
        let mut magenta = 0usize;
        let mut min_x = u32::MAX;
        let mut max_x = 0u32;
        let mut min_y = u32::MAX;
        let mut max_y = 0u32;
        for (x, y, p) in img.enumerate_pixels() {
            let [r, g, b, _] = p.0;
            if r > 180 && b > 180 && g < 120 {
                magenta += 1;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
        if magenta < 2_000 {
            let path = std::env::temp_dir().join("verba-paint-fail.png");
            let _ = img.save_with_format(&path, image::ImageFormat::Png);
            panic!(
                "浮层未画上屏幕：品红像素仅 {magenta}（期望 ≥2000），截屏已存 {}",
                path.display()
            );
        }
        let (bw, bh) = (max_x - min_x + 1, max_y - min_y + 1);
        assert!(
            bw >= 180 && bh >= 40,
            "品红包围盒尺寸异常 {bw}x{bh}（主屏 {}x{}）",
            iw,
            ih
        );

        // 盒内深色字形（标题 + 正文 #333333）：证明文字确实栅格化上屏。
        let mut dark = 0usize;
        for (x, y, p) in img.enumerate_pixels() {
            if x < min_x || x > max_x || y < min_y || y > max_y {
                continue;
            }
            let [r, g, b, _] = p.0;
            if r < 110 && g < 110 && b < 110 {
                dark += 1;
            }
        }
        assert!(
            dark >= 150,
            "盒内字形像素过少（{dark}）：标题/正文未栅格化或不可读"
        );

        cw.hide();
        assert!(!cw.is_visible());
    }
}
