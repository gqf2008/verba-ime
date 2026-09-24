//! 悬浮 AI 回复按钮（issue verba-float-button）：输入法 session 激活时
//! 在光标/文本区旁弹出气泡小按钮，点击后编排「截前台窗口 → OCR →
//! LLM 生成回复」，结果经 stdout 交前端上屏（与 region-ocr 同一契约）。
//!
//! 架构归属（jev 判断 p=1.000，沿 issue #82 既定模式）：按钮窗是独立
//! helper 进程（本子命令），winit 事件循环不落在 TSF DLL / IMK 进程。
//! 前端只在 session 激活/失活时 spawn/kill 本进程并传锚点坐标。
//!
//! 焦点约束：按钮窗必须「点击不抢编辑器焦点」——macOS 走
//! NonactivatingPanel 样式位 + Accessory 激活策略，Windows 走
//! WS_EX_NOACTIVATE，X11 走 override_redirect（见 chrome.rs / resumed）。
//!
//! v1 已知限制（docs/architecture.md 同步）：
//! - 截的是「前台窗口在屏幕上的可见区域」，遮挡部分会带入遮挡内容；
//! - 窗口为方形（圆角图标需平台 shaping，后续打磨）；
//! - Wayland 焦点豁免以合成器为准。

mod chrome;

use std::num::NonZeroU32;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use verba_ipc::{LlmSession, VerbaClient};
use verba_protos::stream_event;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::capture::{capture_active_window, virtual_screen};
use crate::daemon::connect_daemon;
use crate::TriggerError;

/// 按钮逻辑尺寸（全局单位 = xcap Monitor 单位：macOS 点 / Win·Linux 物理像素）。
pub const BUTTON_SIZE: u32 = 44;
/// 锚点到按钮的间隙（全局单位）。
pub const BUTTON_GAP: i32 = 8;
/// 等待点击的最大时长（前端失活会提前 kill；本 TTL 只是崩溃兜底）。
pub const IDLE_TTL: Duration = Duration::from_secs(30 * 60);
/// 点击后整条「截窗 + OCR + LLM」管线的超时。
pub const PIPELINE_TIMEOUT: Duration = Duration::from_secs(120);

/// 品牌深空蓝（assets/branding 参考图同族；方形底）。
const COLOR_BG: u32 = 0x4A6CF7;
/// 悬停提亮。
const COLOR_BG_HOVER: u32 = 0x6B88F9;
/// 忙碌（管线在跑）压暗 + 白点。
const COLOR_BG_BUSY: u32 = 0x3A56C8;
/// 对话泡与忙碌点：白。
const COLOR_WHITE: u32 = 0xFFFFFF;

/// 悬浮按钮入参（前端 spawn 时经 CLI 传入）。
#[derive(Debug, Clone)]
pub struct FloatArgs {
    /// 锚点（光标点 / 文本区左上，全局单位）。
    pub at: (i32, i32),
    /// LLM 窗口会话标识（与 `//` 同会话，可追问；空 key = legacy 槽）。
    pub session_id: u64,
    pub session_key: String,
}

/// 按钮结束态：回复文本 / 用户取消 / 超时 / 管线失败（携带原因）。
/// Failed 由 bin 映射为 stderr + 退出 1——取消语义只留给真正的用户取消/
/// 超时，管线错误不再被静默吞掉（独立评审 F2）。
#[derive(Debug, PartialEq, Eq)]
pub enum FloatOutcome {
    Reply(String),
    Cancelled,
    Expired,
    Failed(String),
}

/// worker 结果 → 结束态：非空回复原样带；空回复与管线错误归 Failed
/// （携带原因）。纯函数以便单测钉住映射语义。
fn outcome_from_result(result: Result<String, TriggerError>) -> FloatOutcome {
    match result {
        Ok(text) if !text.trim().is_empty() => FloatOutcome::Reply(text),
        Ok(_) => FloatOutcome::Failed("LLM 返回空文本".into()),
        Err(e) => FloatOutcome::Failed(e.to_string()),
    }
}

/// 计算按钮左上角：默认锚点右下（gap 间隙）；下方放不下翻转到上方；
/// 右越工作区向左收。`work` = 工作区 (x, y, w, h)（全局单位）。
pub fn button_origin(
    anchor: (i32, i32),
    btn: (u32, u32),
    gap: i32,
    work: (i32, i32, i32, i32),
) -> (i32, i32) {
    let (ax, ay) = anchor;
    let (bw, bh) = (btn.0 as i32, btn.1 as i32);
    let (wx, wy, ww, wh) = work;
    let mut x = ax + gap;
    let mut y = ay + gap;
    if y + bh > wy + wh {
        y = (ay - gap - bh).max(wy);
    }
    if x + bw > wx + ww {
        x = (wx + ww - bw).max(wx);
    }
    (x.max(wx), y.max(wy))
}

/// 回复 prompt 模板渲染：`{ocr}` 占位替换为识别文本。
/// 模板缺占位符即配置错误——拒绝静默丢 OCR 上下文（helper 启动即报错，
/// 不等到 LLM 返回才发现）。
pub fn build_prompt(template: &str, ocr: &str) -> Result<String, TriggerError> {
    const PLACEHOLDER: &str = "{ocr}";
    if !template.contains(PLACEHOLDER) {
        return Err(TriggerError::Config(format!(
            "float_button_prompt 模板缺少 {PLACEHOLDER} 占位符"
        )));
    }
    Ok(template.replace(PLACEHOLDER, ocr))
}

/// 点击后的整条管线：截前台窗口 → OCR → 模板拼 prompt → LLM 生成。
/// 阻塞调用（运行在窗事件循环外的 worker 线程）；结果经 channel 回事件循环。
pub fn run_float_pipeline(session_id: u64, session_key: &str) -> Result<String, TriggerError> {
    let shot = capture_active_window()?;
    let mut client = connect_daemon()?;
    let ocr = client
        .ocr_recognize(&shot.bmp)
        .map_err(|e| TriggerError::Daemon(format!("OCR 失败: {e}")))?;
    if ocr.trim().is_empty() {
        return Err(TriggerError::Capture(
            "前台窗口未识别到文字（窗口可能无文本内容）".into(),
        ));
    }
    let template = float_button_prompt(&mut client)?;
    let prompt = build_prompt(&template, &ocr)?;
    let session = if session_key.is_empty() {
        LlmSession::legacy(session_id)
    } else {
        LlmSession::window(session_id, session_key)
    };
    let id = client
        .llm_start(&prompt, None, None, None, None, session)
        .map_err(|e| TriggerError::Daemon(format!("LLM 请求失败: {e}")))?;
    loop {
        let evt = client
            .next_event(id)
            .map_err(|e| TriggerError::Daemon(format!("LLM 流读取失败: {e}")))?;
        match evt.kind {
            Some(stream_event::Kind::Final(f)) => return Ok(f.text),
            Some(stream_event::Kind::Error(e)) => {
                return Err(TriggerError::Daemon(format!(
                    "LLM 生成失败: code={} {}",
                    e.code, e.message
                )));
            }
            // Chunk 不拼（v1 一次性出最终结果）；Candidates 等其它事件跳过。
            _ => {}
        }
    }
}

/// 从 daemon 配置取回复模板；旧 daemon 无此键时回退到 verba-config 的
/// 内置默认（单一权威，helper 不在此重复字面量）。
fn float_button_prompt(client: &mut VerbaClient) -> Result<String, TriggerError> {
    let cfg = client
        .get_config()
        .map_err(|e| TriggerError::Daemon(format!("读取配置失败: {e}")))?;
    Ok(cfg
        .get("float_button_prompt")
        .cloned()
        .unwrap_or_else(|| verba_config::Config::default().float_button_prompt))
}

/// 按钮视觉态（渲染与事件共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visual {
    Idle,
    Hover,
    Busy,
}

/// 把按钮画进 0x00RRGGBB 缓冲（物理像素网格；`scale` = 物理/逻辑）。
/// 方形底（v1 不做平台 shaping）；对话泡 + 三点；忙碌态白点提示在跑。
fn render_button(buf: &mut [u32], w: u32, h: u32, scale: f64, visual: Visual) {
    let bg = match visual {
        Visual::Idle => COLOR_BG,
        Visual::Hover => COLOR_BG_HOVER,
        Visual::Busy => COLOR_BG_BUSY,
    };
    let s = scale.max(0.1);
    // 设计坐标（逻辑像素）：方形底全铺；对话泡圆角矩形 + 左下小尾；
    // 三点横排在泡内。busy 时泡改深底白点。
    let bubble = (10.0, 12.0, 24.0, 16.0, 7.0_f64); // x, y, w, h, r
    for py in 0..h {
        for px in 0..w {
            let lx = px as f64 / s;
            let ly = py as f64 / s;
            let mut c = bg;
            let in_bubble = in_rounded_rect(lx, ly, bubble.0, bubble.1, bubble.2, bubble.3, bubble.4)
                // 左下小尾（三角形近似：泡底之下、左起 4..10 的竖条 + 斜收）
                || (ly >= bubble.1 + bubble.3 - 1.0
                    && ly < bubble.1 + bubble.3 + 5.0
                    && lx >= bubble.0 + 3.0
                    && lx < bubble.0 + 9.0 - (ly - (bubble.1 + bubble.3 - 1.0)));
            if in_bubble {
                c = if visual == Visual::Busy {
                    COLOR_BG_BUSY
                } else {
                    COLOR_WHITE
                };
            }
            // 三点：idle/hover 蓝点在白泡上；busy 白点在深泡上。
            let dot = if visual == Visual::Busy {
                COLOR_WHITE
            } else {
                COLOR_BG
            };
            for cx in [17.0_f64, 22.0, 27.0] {
                let dx = lx - cx;
                let dy = ly - 20.0;
                if dx * dx + dy * dy <= 2.1_f64.powi(2) {
                    c = dot;
                }
            }
            buf[(py * w + px) as usize] = c;
        }
    }
}

/// 点是否在圆角矩形内（矩形体 + 四角圆）。
fn in_rounded_rect(px: f64, py: f64, x: f64, y: f64, w: f64, h: f64, r: f64) -> bool {
    if px < x || px >= x + w || py < y || py >= y + h {
        return false;
    }
    let cx = px.clamp(x + r, x + w - r);
    let cy = py.clamp(y + r, y + h - r);
    if (px, py) == (cx, cy) {
        return true; // 在矩形体内部
    }
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= r * r
}

/// 窗状态：事件循环驱动。
struct FloatState {
    args: FloatArgs,
    window: Option<std::rc::Rc<Window>>,
    surface: Option<softbuffer::Surface<std::rc::Rc<Window>, std::rc::Rc<Window>>>,
    visual: Visual,
    /// 点击后管线 worker 的结果通道。
    worker: Option<Receiver<Result<String, TriggerError>>>,
    /// 当前阶段的截止时间：等待点击 = 启动 + IDLE_TTL；点击后 = 点击 + PIPELINE_TIMEOUT。
    deadline: Instant,
    outcome: Option<FloatOutcome>,
}

/// 运行按钮窗事件循环，直到点击出结果 / 取消 / 超时。
pub fn run_float_button(args: FloatArgs) -> Result<FloatOutcome, TriggerError> {
    let state = FloatState {
        args,
        window: None,
        surface: None,
        visual: Visual::Idle,
        worker: None,
        deadline: Instant::now() + IDLE_TTL,
        outcome: None,
    };
    let mut app = FloatApp { state: Some(state) };
    let mut builder = EventLoop::builder();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        // Accessory：helper 不出现在 Dock/切换器，且不激活其它应用失焦。
        builder.with_activation_policy(ActivationPolicy::Accessory);
    }
    let event_loop = builder
        .build()
        .map_err(|e| TriggerError::Capture(format!("创建事件循环失败: {e}")))?;
    event_loop
        .run_app(&mut app)
        .map_err(|e| TriggerError::Capture(format!("事件循环失败: {e}")))?;
    app.state
        .and_then(|s| s.outcome)
        .ok_or_else(|| TriggerError::Capture("事件循环结束但无结果".into()))
}

struct FloatApp {
    state: Option<FloatState>,
}

impl FloatApp {
    fn finish(&mut self, event_loop: &ActiveEventLoop, outcome: FloatOutcome) {
        if let Some(state) = self.state.as_mut() {
            state.outcome = Some(outcome);
        }
        event_loop.exit();
    }
}

impl ApplicationHandler for FloatApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = self.state.as_mut() else {
            event_loop.exit();
            return;
        };
        let attrs = Window::default_attributes()
            .with_title("VerbaFloat")
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(winit::window::WindowLevel::AlwaysOnTop)
            .with_active(false)
            .with_inner_size(winit::dpi::LogicalSize::new(
                BUTTON_SIZE as f64,
                BUTTON_SIZE as f64,
            ));
        #[cfg(all(
            unix,
            not(target_os = "macos"),
            not(any(target_os = "android", target_os = "ios"))
        ))]
        let attrs = {
            use winit::platform::x11::WindowAttributesExtX11 as _;
            // override_redirect：WM 不管理，点击不焦点抢占。
            attrs.with_override_redirect(true)
        };
        let window = match event_loop.create_window(attrs) {
            Ok(w) => std::rc::Rc::new(w),
            Err(e) => {
                self.finish(
                    event_loop,
                    FloatOutcome::Failed(format!("创建悬浮按钮窗失败: {e}")),
                );
                return;
            }
        };
        // 非激活修饰：必须在窗口创建后（macOS styleMask / Win exstyle）。
        chrome::make_nonactivating(&window);
        // 摆位：全局单位 → macOS 逻辑（点），其它物理（scale=1 路径，
        // 与 selection.rs 的「薄单位适配层」同一口径）。
        let origin = match virtual_screen() {
            Ok(vs) => button_origin(
                state.args.at,
                (BUTTON_SIZE, BUTTON_SIZE),
                BUTTON_GAP,
                (vs.x, vs.y, vs.width, vs.height),
            ),
            Err(e) => {
                log::warn!("取虚拟屏幕失败，用锚点直接摆位: {e}");
                (state.args.at.0 + BUTTON_GAP, state.args.at.1 + BUTTON_GAP)
            }
        };
        place_window(&window, origin.0, origin.1);
        let context = match softbuffer::Context::new(window.clone()) {
            Ok(c) => std::rc::Rc::new(c),
            Err(e) => {
                self.finish(
                    event_loop,
                    FloatOutcome::Failed(format!("softbuffer Context 失败: {e}")),
                );
                return;
            }
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                self.finish(
                    event_loop,
                    FloatOutcome::Failed(format!("softbuffer Surface 失败: {e}")),
                );
                return;
            }
        };
        state.window = Some(window.clone());
        state.surface = Some(surface);
        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let Some(window) = state.window.as_ref() else {
            return;
        };
        if window.id() != id {
            return;
        }
        match event {
            WindowEvent::CloseRequested => self.finish(event_loop, FloatOutcome::Cancelled),
            WindowEvent::CursorEntered { .. } => {
                if state.visual != Visual::Busy {
                    state.visual = Visual::Hover;
                    window.request_redraw();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if state.visual != Visual::Busy {
                    state.visual = Visual::Idle;
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => match button {
                MouseButton::Left => {
                    if state.worker.is_none() {
                        state.visual = Visual::Busy;
                        window.request_redraw();
                        let (tx, rx) = channel();
                        let session_id = state.args.session_id;
                        let session_key = state.args.session_key.clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(run_float_pipeline(session_id, &session_key));
                        });
                        state.worker = Some(rx);
                        state.deadline = Instant::now() + PIPELINE_TIMEOUT;
                    }
                }
                // 右键取消（等同失活 kill 的保守语义：不触发管线）。
                MouseButton::Right => self.finish(event_loop, FloatOutcome::Cancelled),
                _ => {}
            },
            WindowEvent::RedrawRequested => {
                if let (Some(window), Some(surface)) = (&state.window, &mut state.surface) {
                    redraw(window, surface, state.visual);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if let Some(rx) = state.worker.as_ref() {
            match rx.try_recv() {
                Ok(result) => {
                    self.finish(event_loop, outcome_from_result(result));
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // worker 在 send 前异常终止：不是用户取消，按失败带出。
                    self.finish(
                        event_loop,
                        FloatOutcome::Failed("管线 worker 异常终止".into()),
                    );
                    return;
                }
            }
        }
        let now = Instant::now();
        if now >= state.deadline {
            self.finish(event_loop, FloatOutcome::Expired);
            return;
        }
        // 忙碌期 100ms 轮询 worker 结果；等待点击期睡到 deadline（TTL）。
        let wake = if state.worker.is_some() {
            now + Duration::from_millis(100)
        } else {
            state.deadline
        };
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake));
    }
}

/// 摆位单位适配（issue #82「薄单位适配层」）：
/// macOS 全局单位是点 = winit 逻辑单位；Windows/Linux xcap 为物理像素
/// = winit 物理单位（scale 恒 1 路径，与 selection.rs 同一约定）。
#[cfg(target_os = "macos")]
fn place_window(window: &Window, x: i32, y: i32) {
    window.set_outer_position(winit::dpi::LogicalPosition::new(x as f64, y as f64));
}

#[cfg(not(target_os = "macos"))]
fn place_window(window: &Window, x: i32, y: i32) {
    window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
}

/// 重绘按钮：缓冲取窗口**物理**尺寸（Retina 下不裁角，同 selection.rs）。
fn redraw(
    window: &Window,
    surface: &mut softbuffer::Surface<std::rc::Rc<Window>, std::rc::Rc<Window>>,
    visual: Visual,
) {
    let physical = window.inner_size();
    let (w, h) = match (
        NonZeroU32::new(physical.width.max(1)),
        NonZeroU32::new(physical.height.max(1)),
    ) {
        (Some(w), Some(h)) => (w, h),
        _ => return,
    };
    if let Err(e) = surface.resize(w, h) {
        log::warn!("悬浮按钮 resize 失败: {e}");
        return;
    }
    let mut buf = match surface.buffer_mut() {
        Ok(b) => b,
        Err(e) => {
            log::warn!("悬浮按钮取缓冲失败: {e}");
            return;
        }
    };
    let px = buf.as_mut();
    if px.len() < (w.get() * h.get()) as usize {
        log::warn!("悬浮按钮缓冲不足: {}", px.len());
        return;
    }
    render_button(px, w.get(), h.get(), window.scale_factor(), visual);
    window.pre_present_notify();
    if let Err(e) = buf.present() {
        log::warn!("悬浮按钮 present 失败: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_places_below_right_of_anchor() {
        // 工作区 0,0,1920,1080；锚点 (100,100) → 右下偏移。
        assert_eq!(
            button_origin((100, 100), (44, 44), 8, (0, 0, 1920, 1080)),
            (108, 108)
        );
    }

    #[test]
    fn origin_flips_above_when_no_space_below() {
        // 锚点贴工作区底：下方放不下 → 翻到上方。
        let (x, y) = button_origin((100, 1076), (44, 44), 8, (0, 0, 1920, 1080));
        assert_eq!(y, 1076 - 8 - 44);
        assert_eq!(x, 108);
    }

    #[test]
    fn origin_clamps_left_when_overflow_right() {
        // 锚点贴工作区右缘：x 收进工作区。
        let (x, _) = button_origin((1919, 100), (44, 44), 8, (0, 0, 1920, 1080));
        assert_eq!(x, 1920 - 44);
    }

    #[test]
    fn prompt_template_replaces_ocr() {
        let p = build_prompt("内容：{ocr}\n写回复", "hello").unwrap();
        assert_eq!(p, "内容：hello\n写回复");
        // 多行 OCR 原样代入。
        let p = build_prompt("{ocr}", "a\nb").unwrap();
        assert_eq!(p, "a\nb");
    }

    #[test]
    fn prompt_template_without_placeholder_is_rejected() {
        assert!(build_prompt("没有占位符", "x").is_err());
        assert!(build_prompt("", "x").is_err());
    }

    #[test]
    fn button_renders_all_states() {
        for v in [Visual::Idle, Visual::Hover, Visual::Busy] {
            let (w, h) = (44u32, 44u32);
            let mut buf = vec![0u32; (w * h) as usize];
            render_button(&mut buf, w, h, 1.0, v);
            // 背景主色占多数。
            let bg = match v {
                Visual::Idle => COLOR_BG,
                Visual::Hover => COLOR_BG_HOVER,
                Visual::Busy => COLOR_BG_BUSY,
            };
            assert!(buf.iter().filter(|&&c| c == bg).count() > (w * h / 2) as usize);
            // 三态互不相同（悬停/忙碌可见反馈）。
        }
        let mut a = vec![0u32; 44 * 44];
        let mut b = vec![0u32; 44 * 44];
        let mut c = vec![0u32; 44 * 44];
        render_button(&mut a, 44, 44, 1.0, Visual::Idle);
        render_button(&mut b, 44, 44, 1.0, Visual::Hover);
        render_button(&mut c, 44, 44, 1.0, Visual::Busy);
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn button_renders_at_retina_scale() {
        let (w, h) = (88u32, 88u32);
        let mut buf = vec![0u32; (w * h) as usize];
        render_button(&mut buf, w, h, 2.0, Visual::Idle);
        assert!(buf.contains(&COLOR_WHITE));
    }

    #[test]
    fn rounded_rect_hit_tests() {
        assert!(in_rounded_rect(12.0, 20.0, 10.0, 12.0, 24.0, 16.0, 7.0));
        assert!(!in_rounded_rect(9.0, 20.0, 10.0, 12.0, 24.0, 16.0, 7.0));
        // 角落在半径外。
        assert!(!in_rounded_rect(10.5, 12.5, 10.0, 12.0, 24.0, 16.0, 2.0));
    }

    #[test]
    fn outcome_mapping_distinguishes_reply_empty_error() {
        assert_eq!(
            outcome_from_result(Ok("你好".into())),
            FloatOutcome::Reply("你好".into())
        );
        // 空白回复按失败带原因（点了按钮却无声消失 = 评审 F2 报的问题）。
        match outcome_from_result(Ok("  \n ".into())) {
            FloatOutcome::Failed(m) => assert!(m.contains("空文本"), "实际: {m}"),
            other => panic!("期望 Failed，得 {other:?}"),
        }
        match outcome_from_result(Err(TriggerError::Config("模板缺 {ocr} 占位".into()))) {
            FloatOutcome::Failed(m) => assert!(m.contains("占位"), "实际: {m}"),
            other => panic!("期望 Failed，得 {other:?}"),
        }
    }
}
