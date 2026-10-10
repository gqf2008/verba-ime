//! macOS 自绘候选窗：`NSPanel` + 自定义 `NSView`，把 `verba-candidate`
//! 的 CPU 渲染结果（tiny-skia，与 Windows 同款）作为位图 blit 上屏。
//!
//! 取代原生 `IMKCandidates`——后者是老 AppKit 面板，主题/分页/光标避让
//! 接不上，两端 UX 分叉（见 `docs/architecture.md` §12.2 选型）。本模块
//! 与 Windows `candidate_window.rs` 共享同一渲染器，做到两端一致。
//!
//! 坐标系约定同 `float_panel`：对外接口用 **Cocoa 底左**（NSWindow frame
//! 语义），锚点纯函数 `anchor_cg` 产出 **CG 顶左**，由调用侧换算。

use std::cell::RefCell;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBitmapImageRep, NSColor, NSDeviceRGBColorSpace, NSFloatingWindowLevel,
    NSImage, NSPanel, NSView, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use verba_candidate::renderer::{result_text_wrap_width, window_size, CpuCandidateRenderer};
use verba_candidate::CandidateWindowController;

/// 候选窗与光标行的间隙（点，纵向）。
const CAND_GAP: i32 = 4;

/// 锚点纯函数（CG 顶左，可测）：
/// - `caret = Some((x, top, bottom))`：光标矩形右缘 CG x、行顶/行底 CG y；
///   默认落在**行正下方**（`bottom + GAP`），下方放不下翻到行上方
///   （`top - h - GAP`），再不行贴空间更大的一侧；水平 clamp 进工作区。
/// - `caret = None`：退工作区左上内缩（光标矩形无效时不乱飘）。
///
/// `work = (wx, wy, ww, wh)`（CG 顶左工作区）。
pub fn anchor_cg(
    caret: Option<(i32, i32, i32)>,
    work: (i32, i32, i32, i32),
    w: i32,
    h: i32,
) -> (i32, i32) {
    let (wx, wy, ww, wh) = work;
    let xr = wx.saturating_add(ww);
    let yb = wy.saturating_add(wh);
    let px = clamp_i32(
        caret.map(|c| c.0).unwrap_or(wx + 8),
        wx,
        xr.saturating_sub(w),
    );
    let py = match caret {
        Some((_, top, bottom)) => {
            let below = bottom.saturating_add(CAND_GAP);
            let above = top.saturating_sub(CAND_GAP).saturating_sub(h);
            if below.saturating_add(h) <= yb {
                below
            } else if above >= wy {
                above
            } else if top.saturating_sub(wy) >= yb.saturating_sub(bottom) {
                wy
            } else {
                yb.saturating_sub(h).max(wy)
            }
        }
        None => wy.saturating_add(8),
    };
    (px, clamp_i32(py, wy, yb.saturating_sub(h)))
}

fn clamp_i32(v: i32, lo: i32, hi: i32) -> i32 {
    v.max(lo).min(hi.max(lo))
}

#[derive(Default)]
struct CandidateViewIvars {
    image: RefCell<Option<Retained<NSImage>>>,
}

define_class!(
    /// 候选内容视图：`isFlipped` = true（顶左原点，与位图行序一致），
    /// drawRect 把渲染位图铺满 bounds。
    ///
    /// SAFETY: NSView 子类化无额外约束；类仅注册一次；实例仅 IME 主线程
    /// 创建/访问。
    #[name = "VerbaCandidateView"]
    #[unsafe(super = NSView)]
    #[ivars = CandidateViewIvars]
    struct CandidateView;

    impl CandidateView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _rect: NSRect) {
            let ivars = self.ivars();
            let img = ivars.image.borrow();
            if let Some(img) = img.as_ref() {
                let bounds = self.bounds();
                img.drawInRect(bounds);
            }
        }
    }
);

impl CandidateView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(CandidateViewIvars::default());
        // SAFETY: initWithFrame: 为 NSView 指定初始化器；this 为刚 alloc 的
        // 本类实例；仅 IME 主线程调用。
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn set_image(&self, img: Option<Retained<NSImage>>) {
        *self.ivars().image.borrow_mut() = img;
        self.setNeedsDisplay(true);
    }
}

struct PanelState {
    panel: Retained<NSPanel>,
    view: Retained<CandidateView>,
    renderer: CpuCandidateRenderer,
}

/// 进程内单例存储（同 `float_panel`：仅 IME 主线程访问，显式标注 Sync）。
struct PanelStore(RefCell<Option<PanelState>>);
unsafe impl Sync for PanelStore {}
unsafe impl Send for PanelStore {}

fn panel_state() -> &'static PanelStore {
    static S: OnceLock<PanelStore> = OnceLock::new();
    S.get_or_init(|| PanelStore(RefCell::new(None)))
}

/// 把 RGBA（预乘，tiny-skia 输出）打成 `NSImage`，逻辑尺寸按 scale 折算。
fn image_from_rgba(
    rgba: &[u8],
    pw: u32,
    ph: u32,
    logical_w: f64,
    logical_h: f64,
) -> Option<Retained<NSImage>> {
    // SAFETY: planes=null 让 AppKit 自行分配缓冲；随后的 copy 长度
    // = 宽*高*4 与分配的 bytesPerRow*height 一致。
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            pw as isize,
            ph as isize,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            (pw as isize) * 4,
            32,
        )
    }?;
    let dst = rep.bitmapData();
    if dst.is_null() {
        return None;
    }
    // SAFETY: dst 由上面分配、容量 = pw*4*ph 字节；rgba 长度相同。
    unsafe {
        std::ptr::copy_nonoverlapping(rgba.as_ptr(), dst, rgba.len());
    }
    // img 逻辑尺寸 = 像素 / scale，显示时按点缩放（Retina 清晰）。
    let img = NSImage::initWithSize(NSImage::alloc(), NSSize::new(logical_w, logical_h));
    img.addRepresentation(&rep);
    Some(img)
}

/// 创建 panel（幂等；仅 IME 主线程调用）。
fn ensure_panel(mtm: MainThreadMarker) {
    if panel_state().0.borrow().is_some() {
        return;
    }
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0));
    // NSPanel 本体 + NonactivatingPanel：弹候选不激活 IME 应用、不带走宿主
    // 焦点（与 float 气泡同类型，AppKit 对普通 NSWindow 忽略该位）。
    let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        style,
        NSBackingStoreType::Buffered,
        false,
    );
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    // 圆角/透明角由渲染位图的 alpha 提供；窗口阴影按矩形算会露方角，关掉。
    panel.setHasShadow(false);
    // SAFETY: releasedWhenClosed 仅影响 close 时的内存语义，本类不 close
    // （只 orderOut），置 NO 防误 close 释放。
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setLevel(NSFloatingWindowLevel);
    // 候选窗不接收鼠标（v1：选词走数字键/方向键；鼠标点选列后续）。
    panel.setIgnoresMouseEvents(true);

    let view = CandidateView::new(mtm, frame);
    panel.setContentView(Some(&view));

    *panel_state().0.borrow_mut() = Some(PanelState {
        panel,
        view,
        renderer: CpuCandidateRenderer::new(),
    });
    crate::imk::dbg_log("candidate(skia): panel 已创建");
}

/// 渲染并显示候选窗（IME 主线程）。`caret_cg` = 光标行 (x, top, bottom)
/// （CG 顶左，display 坐标），`work_cg` = 工作区 (x,y,w,h)，`screen_h` =
/// 主屏高（CG→Cocoa 换算），`scale` = backingScaleFactor。
pub fn show(
    ctrl: &CandidateWindowController,
    caret_cg: Option<(i32, i32, i32)>,
    work_cg: (i32, i32, i32, i32),
    screen_h: f64,
    scale: f64,
) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    // SAFETY: 本函数仅从 IMK 主线程调用。
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    ensure_panel(mtm);
    let mut store = panel_state().0.borrow_mut();
    let Some(st) = store.as_mut() else { return };

    let mut c = if (scale - 1.0).abs() > 1e-6 {
        ctrl.scaled(scale as f32)
    } else {
        ctrl.clone()
    };
    // 结果浮层行数在缩放后坐标实测回填（测量/建窗/渲染同源，同 Windows）。
    if let Some(block) = c.result_block() {
        let lines = st.renderer.measure_lines(
            block,
            c.theme().font_size as f32,
            result_text_wrap_width(c.theme()),
        );
        c.set_result_lines(lines);
    }
    let (pw, ph) = window_size(&c);
    if pw == 0 || ph == 0 {
        return;
    }
    let logical_w = pw as f64 / scale;
    let logical_h = ph as f64 / scale;
    // 锚点：CG 顶左 → Cocoa 底左 frame 原点（frame_y = 屏高 - cg_y - 高）。
    let (px, py) = anchor_cg(
        caret_cg,
        work_cg,
        logical_w.round() as i32,
        logical_h.round() as i32,
    );
    let origin = (px as f64, screen_h - py as f64 - logical_h);
    let out = st.renderer.render(&c);
    let img = image_from_rgba(&out.pixels, out.width, out.height, logical_w, logical_h);
    let frame = NSRect::new(
        NSPoint::new(origin.0, origin.1),
        NSSize::new(logical_w, logical_h),
    );
    st.panel.setFrame_display(frame, true);
    st.view.setFrame(frame);
    st.view.set_image(img);
    st.panel.orderFrontRegardless();
}

/// 隐藏候选窗（无窗则空操作）。
pub fn hide() {
    // SAFETY: 仅从 IMK 主线程调用。
    if let Ok(mut store) = panel_state().0.try_borrow_mut() {
        if let Some(st) = store.as_mut() {
            st.panel.orderOut(None);
        }
    }
}

/// 面板当前是否可见（集成回归断言用）。
pub fn is_visible() -> bool {
    panel_state()
        .0
        .try_borrow()
        .ok()
        .and_then(|s| s.as_ref().map(|st| st.panel.isVisible()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_below_caret_within_work() {
        // 工作区 100,100 起 800x600；光标 x=200 top=300 bottom=315（行高 15）；
        // 窗口 360x240 → 落行下方 315+4=319。
        assert_eq!(
            anchor_cg(Some((200, 300, 315)), (100, 100, 800, 600), 360, 240),
            (200, 319)
        );
    }

    #[test]
    fn anchor_flips_above_at_bottom() {
        // 光标贴工作区底：下方放不下 → 翻到行上方 top-4-h。
        let (x, y) = anchor_cg(Some((400, 780, 795)), (100, 100, 800, 700), 360, 240);
        assert_eq!(x, 400);
        assert_eq!(y, 780 - CAND_GAP - 240);
    }

    #[test]
    fn anchor_clamps_right_edge() {
        // 光标 x 越过右缘 → 平移到工作区右缘内。
        let (x, _y) = anchor_cg(Some((1200, 300, 315)), (100, 100, 800, 600), 360, 240);
        assert!(x + 360 <= 900);
        assert!(x >= 100);
    }

    #[test]
    fn anchor_without_caret_falls_back_top_left() {
        let (x, y) = anchor_cg(None, (100, 100, 800, 600), 360, 240);
        assert_eq!((x, y), (108, 108));
    }

    #[test]
    fn anchor_never_leaves_tiny_work() {
        let (x, y) = anchor_cg(Some((50, 50, 60)), (0, 0, 20, 20), 360, 240);
        assert!(x >= 0 && y >= 0);
    }
}
