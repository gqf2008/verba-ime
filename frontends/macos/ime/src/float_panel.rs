//! 悬浮 AI 回复气泡（v2，进程内 Panel）。
//!
//! v1 的「IME 激活时 spawn verba-trigger 跨进程小窗」模式在飞书 / 终端
//! 类客户端引发 IME 会话高频自激抖动（activate/spawn/deactivate ≥10Hz
//! 刷屏，无法输入；D2，见 walgit 线程 verba-float-button-d2-session-churn
//! 与 docs/architecture.md 已知限制）。v2 重构要点：
//!
//! - 窗口收进 **IME 进程**（NSPanel + NonactivatingPanel styleMask，
//!   NSPanel 本体才有意义——AppKit 对普通 NSWindow 忽略该位），与候选窗
//!   同址生命周期：session 激活显、失活隐——激活路径零跨进程，
//!   从根上消灭自激环；
//! - 锚点 = **光标所在行正下方（左对齐光标右缘，行底 + 16pt 间隙）**、收
//!   进**前台窗口 bounds**（越界翻转到窗口内），光标矩形无效时退窗口右上
//!   内缩——不再依赖裸屏幕坐标，垃圾光标矩形最多退化为角落定位，不会把气
//!   泡画到屏幕外（D1 几何守卫由收进语义接管）。行正下方而非右下方：整行
//!   让开（不压 I-beam 与当前行字形），且符合输入法辅助 UI 居下惯例；
//!   下方放不下时翻转到行上方（真机验收：用户反馈右下遮挡光标；独立评审
//!   minor：锚行顶 + 间隙仍会压行下部，故锚行底）。
//! - 点击（或 `//`+TAB，同一入口）只 spawn **无头** `verba-trigger
//!   float-run`（截前台窗口 → daemon OCR → stdout）——点击频率级，
//!   不在激活频率级 spawn；LLM 生成在 IME 进程内走 start_llm 流式预览
//!   （与改写同通道），helper 不再持有模板/会话。

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSEvent, NSFloatingWindowLevel, NSPanel, NSView,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

/// 气泡边长（点）。真机验收用户裁定：v1 的 44pt 太大，小一半取 22。
pub const BUBBLE_SIZE: f64 = 22.0;
/// 光标行与气泡的间隙（点，纵向）。真机验收：8pt 偏贴，16pt 离光标更远。
const BUBBLE_GAP: i32 = 16;
/// 退窗口右上时的内缩边距（点）。
const BUBBLE_MARGIN: i32 = 12;
/// 圆角半径（点，随 22pt 气泡等比缩自 v1 的 12pt）。
const CORNER_RADIUS: f64 = 6.5;

/// v1 品牌色（crates/verba-trigger/src/float.rs 同一组常量，前端不另起色板）。
const COLOR_BG: (f64, f64, f64) = (
    0x4A as f64 / 255.0,
    0x6C as f64 / 255.0,
    0xF7 as f64 / 255.0,
);
const COLOR_BG_BUSY: (f64, f64, f64) = (
    0x3A as f64 / 255.0,
    0x56 as f64 / 255.0,
    0xC8 as f64 / 255.0,
);
const COLOR_WHITE: (f64, f64, f64) = (1.0, 1.0, 1.0);

/// 锚点纯函数（CG 顶左全局点，可测）：
/// - `caret = Some((cx, cy, ch))`：cx = 光标右缘 CG x（气泡左对齐它），
///   cy = **光标行顶** CG y，ch = 行高。气泡顶边锚**行底**（cy+ch）+
///   BUBBLE_GAP——整行让开，不压 I-beam（独立评审 minor：`client_caret_point`
///   产出的 cy 即行顶，cy+GAP 会压当前行下部）；下方放不下时翻到行上方
///   （贴行顶 − GAP，两分支对称），右缘越界翻到光标左侧，最终收进窗口
///   bounds；
/// - `caret = None`（光标矩形无效/缺省）：窗口右上内缩 BUBBLE_MARGIN
///   （用户裁定：垃圾光标矩形场景退窗口右上，见 v2 线程锚位选择）。
pub fn bubble_anchor(caret: Option<(i32, i32, i32)>, win: (i32, i32, i32, i32)) -> (i32, i32) {
    let (wx, wy, ww, wh) = win;
    let size = BUBBLE_SIZE as i32;
    match caret {
        Some((cx, cy, ch)) => {
            // 偏好光标所在行正下方（行底 + GAP，整行让开；真机验收：右下
            // 压插入列遮挡光标）。下方放不下时翻到行上方（贴行顶 − GAP）。
            // 贴边翻转只是选侧，最终的 clamp 才保证收进窗口——垃圾光标
            // 矩形 any 方向都出不去。全程 saturating：CG 浮点 as-cast 已
            // 饱和到 i32 边界，加减不得再溢出（debug panic / release 回
            // 绕，复审 F6）。
            let mut x = cx;
            let mut y = cy.saturating_add(ch).saturating_add(BUBBLE_GAP);
            if y.saturating_add(size) > wy.saturating_add(wh) {
                y = cy.saturating_sub(BUBBLE_GAP).saturating_sub(size);
            }
            if x.saturating_add(size) > wx.saturating_add(ww) {
                x = cx.saturating_sub(BUBBLE_GAP).saturating_sub(size);
            }
            (
                clamp_into(x, wx, wx + ww - size),
                clamp_into(y, wy, wy + wh - size),
            )
        }
        None => {
            // 光标矩形无效（终端类客户端）→ 窗口右上内缩；两轴都 clamp
            // （< 气泡高的窗口不探出底边，复审 F5）。
            let x = wx
                .saturating_add(ww)
                .saturating_sub(size)
                .saturating_sub(BUBBLE_MARGIN);
            (
                clamp_into(x, wx, wx + ww - size),
                clamp_into(wy.saturating_add(BUBBLE_MARGIN), wy, wy + wh - size),
            )
        }
    }
}

/// 收进窗口轴：v 夹到 [lo, hi]；窗口比气泡还小的退化情形（hi < lo）贴
/// 窗口原点侧 lo——气泡锚在窗口角上，绝不越界。
fn clamp_into(v: i32, lo: i32, hi: i32) -> i32 {
    v.max(lo).min(hi.max(lo))
}

define_class!(
    /// 气泡内容视图：drawRect 画品牌蓝圆角 + 三点，mouseDown 触发管道。
    /// 点击经由本类回到 [`on_bubble_click`]（模块级入口，IMK 主线程）。
    ///
    /// SAFETY: NSView 子类化无额外约束；类仅注册一次（objc2 保证），
    /// 实例在 IME 主线程创建/销毁。
    #[name = "VerbaBubbleView"]
    #[unsafe(super = NSView)]
    struct BubbleView;

    impl BubbleView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, rect: NSRect) {
            draw_bubble(rect, crate::float_panel::bubble_busy());
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            crate::float_panel::on_bubble_click();
        }
    }
);

impl BubbleView {
    /// 主线程实例化：alloc → set_ivars(()) → super initWithFrame:。
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: initWithFrame: 为 NSView 指定初始化器；this 为刚 alloc 的
        // 本类实例（ivarless）；仅 IME 主线程调用。
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// 画气泡：圆角底 + 三个白点（busy 时压深色，与 v1 busy 态同语义）。
fn draw_bubble(rect: NSRect, busy: bool) {
    let bg = if busy { COLOR_BG_BUSY } else { COLOR_BG };
    let path =
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, CORNER_RADIUS, CORNER_RADIUS);
    NSColor::colorWithRed_green_blue_alpha(bg.0, bg.1, bg.2, 1.0).setFill();
    path.fill();

    // 三个白点：水平居中排布，busy 时略收拢（视觉降噪）。22pt 气泡内
    // 三点外缘不得越界：点半径 2、中心间距 7（busy 5），最外缘 ±9 < 11。
    let dot_r = 2.0_f64;
    let gap = if busy { 2.5 } else { 3.5 };
    let cy = rect.origin.y + rect.size.height / 2.0;
    let cx = rect.origin.x + rect.size.width / 2.0;
    for i in -1..=1 {
        let dx = cx + (i as f64) * gap * 2.0 - dot_r;
        let dy = cy - dot_r;
        let dot = NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
            NSPoint::new(dx, dy),
            NSSize::new(dot_r * 2.0, dot_r * 2.0),
        ));
        NSColor::colorWithRed_green_blue_alpha(COLOR_WHITE.0, COLOR_WHITE.1, COLOR_WHITE.2, 1.0)
            .setFill();
        dot.fill();
    }
}

struct PanelState {
    panel: Retained<NSPanel>,
    view: Retained<BubbleView>,
}

/// 进程内单例存储。裸 `static` 要求 Sync，而 NSPanel 是 MainThreadOnly
/// （!Sync）：本模块所有访问点都在 IME 主线程（各调用处 SAFETY 注），
/// 无跨线程路径，故显式标注 Sync（objc2 0.6 已移除 MainThreadBound 助手，
/// dispatch2 才有；此处自管）。
struct PanelStore(RefCell<Option<PanelState>>);

// SAFETY: PanelState（NSPanel/BubbleView）仅经 panel_state() 访问。
// AppKit 触点（ensure_panel/show_bubble/hide_bubble 的 orderOut/setFrame/
// setNeedsDisplay）都在 IME 主线程（见各函数 SAFETY 注）；唯一可被后台
// 线程触达的 set_busy_draw 已用 MainThreadMarker::new() 门住 AppKit 部分
// （复审 F2），跨线程只剩原子量写。OnceLock 的 Sync 另要求 T: Send，
// 此处一并标注（存储一旦初始化即不移动）。
unsafe impl Sync for PanelStore {}
unsafe impl Send for PanelStore {}

fn panel_state() -> &'static PanelStore {
    static S: OnceLock<PanelStore> = OnceLock::new();
    S.get_or_init(|| PanelStore(RefCell::new(None)))
}

/// busy 标记也供绘制回调读取（drawRect 不可重入 RefCell，单独原子量）。
static BUSY: AtomicBool = AtomicBool::new(false);

pub(crate) fn bubble_busy() -> bool {
    BUSY.load(Ordering::SeqCst)
}

/// 创建 panel（幂等；仅 IME 主线程调用）。
fn ensure_panel(mtm: MainThreadMarker) {
    if panel_state().0.borrow().is_some() {
        return;
    }
    let frame = NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(BUBBLE_SIZE, BUBBLE_SIZE),
    );
    // NSPanel 本体 + NonactivatingPanel：点气泡不激活 IME 应用、不带走
    // 宿主焦点（v1 helper 用 NSWindow 加 styleMask 位，AppKit 对普通
    // NSWindow 忽略该位——v1 焦点扰动嫌疑面之一，v2 用对类型）。
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
    panel.setHasShadow(true);
    // SAFETY: releasedWhenClosed 仅影响 close 时的内存语义，本类不 close
    // （只 orderOut），置 NO 防误 close 释放（与候选窗同策略）。
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setLevel(NSFloatingWindowLevel);

    let view = BubbleView::new(mtm, frame);
    panel.setContentView(Some(&view));

    *panel_state().0.borrow_mut() = Some(PanelState { panel, view });
    crate::imk::dbg_log("float(v2): panel 已创建");
}

/// 摆位并显示气泡（IME 主线程；session 激活路径调用）。
///
/// 坐标系 = **Cocoa 底左全局点**（NSWindow frame 语义，主屏左下原点）——
/// 锚点纯函数 `bubble_anchor` 与 `client_window_bounds` 产出 CG 顶左，
/// 调用侧（activate_server）负责按主屏高度换算（复审 F1：CG 顶左直接喂
/// setFrame 会让 Y 轴关于主屏高度镜像，clamp 全部失效）。f64 保留换算
/// 精度（屏高非整点的缩放模式不引入取整误差）。
pub fn show_bubble(at: (f64, f64)) {
    // SAFETY: 本函数仅从 IMK 主线程（activate_server）调用。
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    ensure_panel(mtm);
    let st = panel_state().0.borrow();
    let Some(st) = st.as_ref() else { return };
    let frame = NSRect::new(
        NSPoint::new(at.0, at.1),
        NSSize::new(BUBBLE_SIZE, BUBBLE_SIZE),
    );
    st.panel.setFrame_display(frame, true);
    st.panel.orderFrontRegardless();
}

/// 隐藏气泡并重置 busy（session 失活/密码字段/开关关闭时调用）。
pub fn hide_bubble() {
    BUSY.store(false, Ordering::SeqCst);
    // SAFETY: 仅从 IMK 主线程（deactivate_server / 开关变更）调用。
    if let Ok(mut st) = panel_state().0.try_borrow_mut() {
        if let Some(st) = st.as_mut() {
            st.panel.orderOut(None);
        }
    }
}

fn set_busy_draw(v: bool) {
    BUSY.store(v, Ordering::SeqCst);
    // 主线程门：clear_busy 会被 float-run 的后台线程调用（失败分支），
    // 离主线程时只复位原子量、不碰 AppKit（复审 F2）。主路径（点击/
    // drain）都在主线程，正常触发重绘。
    let Some(_mtm) = MainThreadMarker::new() else {
        return;
    };
    if let Ok(st) = panel_state().0.try_borrow() {
        if let Some(st) = st.as_ref() {
            st.view.setNeedsDisplay(true);
        }
    }
}

/// 点击入口（气泡 mouseDown 与 `//`+TAB 共用）：busy 忽略 → busy →
/// 后台 spawn `verba-trigger float-run`（前台窗口捕获 → daemon OCR →
/// stdout）→ 识别文本落 float_ocr_slot，由 drain_stream 在主线程拼
/// float_button_prompt 模板并 start_llm（与改写同流式预览通道）。
pub fn on_bubble_click() {
    if !crate::imk::float_button_enabled() {
        return;
    }
    if BUSY.swap(true, Ordering::SeqCst) {
        crate::imk::dbg_log("float(v2): 管道在途，忽略本次触发");
        return;
    }
    set_busy_draw(true);
    crate::imk::trigger_float_run_async();
}

/// 供外部（drain 完成/失败）复位 busy；LLM 起流后由 drain 侧复位。
pub fn clear_busy() {
    set_busy_draw(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_places_below_caret_left_aligned() {
        // 窗口 100,100 到 900,700；光标 (200,300) 行高 15 → 行底 315 + 间隙
        // 16 = 331，x 左对齐光标右缘（真机验收改锚：原右下遮挡光标列；评
        // 审 minor：行顶+间隙会压行下部，故锚行底；用户裁定间隙 8→16）。
        assert_eq!(
            bubble_anchor(Some((200, 300, 15)), (100, 100, 800, 600)),
            (200, 331)
        );
    }

    #[test]
    fn anchor_flips_above_at_window_bottom() {
        // 光标贴窗口底：行下放不下 → 翻到行上方贴行顶 − 间隙，仍收进窗口。
        let (x, y) = bubble_anchor(Some((400, 780, 15)), (100, 100, 800, 700));
        assert_eq!(x, 400);
        assert_eq!(y + BUBBLE_SIZE as i32, 780 - BUBBLE_GAP);
    }

    #[test]
    fn anchor_flips_left_at_window_right_edge() {
        // 光标右缘贴窗口右缘（880+22 > 900 触发翻转）→ 翻到光标左侧（贴
        // 右缘 − 间隙），y 行下不受影响（评审 nit：右缘翻转精确值断言）。
        let (x, y) = bubble_anchor(Some((880, 300, 15)), (100, 100, 800, 600));
        assert_eq!(x, 880 - BUBBLE_GAP - BUBBLE_SIZE as i32);
        assert_eq!(y, 300 + 15 + BUBBLE_GAP);
    }

    #[test]
    fn anchor_clamps_left_when_past_right_edge() {
        // 光标在窗口右缘外（多显示器/垃圾值的温和情形）→ 收进窗口右侧内。
        let (x, _y) = bubble_anchor(Some((1200, 300, 15)), (100, 100, 800, 600));
        assert!(x + BUBBLE_SIZE as i32 <= 900);
        assert!(x >= 100);
    }

    #[test]
    fn anchor_falls_back_to_top_right_without_caret() {
        // 光标矩形无效（终端类客户端）→ 窗口右上内缩。
        let (x, y) = bubble_anchor(None, (100, 100, 800, 600));
        assert_eq!(x, 100 + 800 - BUBBLE_SIZE as i32 - BUBBLE_MARGIN);
        assert_eq!(y, 100 + BUBBLE_MARGIN);
    }

    #[test]
    fn anchor_none_branch_clamps_within_short_window() {
        // 复审 F5：窗口高 30 < margin 12 + 气泡 22 = 34 → y 收进窗口底
        // （30-22=8），不探出底边。
        let (x, y) = bubble_anchor(None, (0, 0, 400, 30));
        assert_eq!(y, 8);
        assert!(y + BUBBLE_SIZE as i32 <= 30);
        assert!(x >= 0 && x + BUBBLE_SIZE as i32 <= 400);
    }

    #[test]
    fn anchor_never_leaves_tiny_window() {
        // 退化小窗（w < 气泡+边距）：至少贴窗口原点一侧，不越界。
        let (x, y) = bubble_anchor(Some((50, 50, 14)), (0, 0, 20, 20));
        assert!(x >= 0 && y >= 0);
        assert!(x <= 20 && y <= 20);
    }
}
