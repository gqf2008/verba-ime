//! 选区截图：全屏遮罩 + 鼠标拖选矩形（跨平台单实现，issue #82）。
//!
//! 替换原 Win32 UpdateLayeredWindow 实现（frontends/windows/ime/src/
//! selection.rs）：winit 建无边框置顶窗 + softbuffer CPU 光栅。
//! UX 与原实现一致——遮罩变暗、选区高亮 + 2px 橙色边框、松开确认、
//! Esc/右键取消；底图为触发瞬间的冻结快照（截取先于开窗，画面干净）。
//!
//! 坐标（thin 平台适配层，其余全库单实现）：全程使用「全局单位」空间
//! （macOS 点 / Windows·Linux 物理像素，即 xcap Monitor 单位）。
//! - macOS：xcap=点、winit physical=backing 像素 → 定位/光标经 scale 换算到
//!   逻辑单位
//! - Windows/Linux：xcap=物理像素 = winit physical → 直传，scale 恒按 1
//!
//! softbuffer 像素无 alpha，遮罩用亮度衰减实现；缓冲取 vs 尺寸（单位网格）。

use std::num::NonZeroU32;

use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, Position, Size};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::capture::{snapshot_for_overlay, RgbaSnapshot, ScreenRect};
use crate::TriggerError;

/// 遮罩亮度系数（与原 Win32 实现的 110/255 alpha 观感一致：保留 45% 亮度）。
const DIM_NUM: u32 = 115;
const DIM_DEN: u32 = 255;
/// 选区边框颜色（橙色，同原实现 0xD5,0x78,0x00）。
const BORDER_RGB: u32 = 0xD57800;
/// 选区边框厚度（全局单位）。
const BORDER: i32 = 2;
/// 最小选区（同原实现：宽高 ≥2 才算有效）。
const MIN_SIZE: i32 = 2;

/// 全局单位 ← winit physical（macOS 除以 scale 得「点」，其余直传）。
fn unit_from_physical(p: PhysicalPosition<f64>, scale: f64) -> (i32, i32) {
    #[cfg(target_os = "macos")]
    {
        let l = p.to_logical::<i32>(scale);
        (l.x, l.y)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = scale;
        (p.x as i32, p.y as i32)
    }
}

/// winit Position ← 全局单位（unit_from_physical 的逆变换）。
fn position_from_unit(x: i32, y: i32, scale: f64) -> Position {
    #[cfg(target_os = "macos")]
    {
        let _ = scale;
        Position::Logical(winit::dpi::LogicalPosition::new(x as f64, y as f64))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = scale;
        Position::Physical(PhysicalPosition::new(x, y))
    }
}

/// winit Size ← 全局单位。
fn size_from_unit(w: i32, h: i32) -> Size {
    #[cfg(target_os = "macos")]
    {
        Size::Logical(winit::dpi::LogicalSize::new(w as f64, h as f64))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Size::Physical(winit::dpi::PhysicalSize::new(
            w.max(0) as u32,
            h.max(0) as u32,
        ))
    }
}

/// 归一化两个角点为 (x1,y1,x2,y2)。
fn normalize(a: (i32, i32), b: (i32, i32)) -> (i32, i32, i32, i32) {
    (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

/// 快照 RGBA（全局单位网格）→ 预变暗 RGB u32（0x00RRGGBB），一次性成本。
fn build_dim_base(snap: &RgbaSnapshot) -> Vec<u32> {
    let n = (snap.vs.width as usize) * (snap.vs.height as usize);
    let mut out = Vec::with_capacity(n);
    let px = &snap.rgba;
    for i in 0..n {
        let (r, g, b) = (px[i * 4] as u32, px[i * 4 + 1] as u32, px[i * 4 + 2] as u32);
        out.push(
            ((r * DIM_NUM / DIM_DEN) << 16)
                | ((g * DIM_NUM / DIM_DEN) << 8)
                | (b * DIM_NUM / DIM_DEN),
        );
    }
    out
}

/// 选区会话状态（窗口生命周期内单线程访问）。
struct SelectionState {
    snap: RgbaSnapshot,
    dim_base: Vec<u32>,
    dragging: bool,
    /// 全局单位、窗口局部坐标（未动鼠标前取屏幕中心，防 (0,0) 兜底误选）。
    cursor: (i32, i32),
    start: (i32, i32),
    result: Option<ScreenRect>,
    window: Option<std::rc::Rc<Window>>,
    surface: Option<softbuffer::Surface<std::rc::Rc<Window>, std::rc::Rc<Window>>>,
}

impl SelectionState {
    fn new(snap: RgbaSnapshot) -> Self {
        let dim_base = build_dim_base(&snap);
        let center = (snap.vs.width / 2, snap.vs.height / 2);
        Self {
            snap,
            dim_base,
            dragging: false,
            cursor: center,
            start: center,
            result: None,
            window: None,
            surface: None,
        }
    }

    /// 重绘：底图 memcpy + 选区内亮像素覆盖 + 边框。
    fn redraw(&mut self) -> Result<(), TriggerError> {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return Ok(());
        };
        let w = self.snap.vs.width;
        let h = self.snap.vs.height;
        if w <= 0 || h <= 0 {
            return Ok(());
        }
        let (bw, bh) = (
            NonZeroU32::new(w as u32).ok_or_else(|| TriggerError::Capture("宽度为 0".into()))?,
            NonZeroU32::new(h as u32).ok_or_else(|| TriggerError::Capture("高度为 0".into()))?,
        );
        surface
            .resize(bw, bh)
            .map_err(|e| TriggerError::Capture(format!("resize 失败: {e}")))?;
        let mut buf = surface
            .buffer_mut()
            .map_err(|e| TriggerError::Capture(format!("取缓冲失败: {e}")))?;
        let px = buf.as_mut();
        let n = bw.get() as usize * bh.get() as usize;
        if px.len() < n {
            return Err(TriggerError::Capture(format!(
                "缓冲不足: {} < {n}",
                px.len()
            )));
        }
        px[..n].copy_from_slice(&self.dim_base);
        if self.dragging {
            let (x1, y1, x2, y2) = normalize(self.start, self.cursor);
            // 选区内恢复原亮度（边框内圈起）
            for y in (y1 + BORDER).max(0)..(y2 - BORDER + 1).min(h) {
                for x in (x1 + BORDER).max(0)..(x2 - BORDER + 1).min(w) {
                    let o = y as usize * w as usize + x as usize;
                    let p = &self.snap.rgba[o * 4..o * 4 + 3];
                    px[o] = ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | p[2] as u32;
                }
            }
            // 2px 边框
            for i in 0..BORDER {
                hline(px, w, h, y1 + i, x1, x2);
                hline(px, w, h, y2 - i, x1, x2);
                vline(px, w, h, x1 + i, y1, y2);
                vline(px, w, h, x2 - i, y1, y2);
            }
        }
        window.pre_present_notify();
        buf.present()
            .map_err(|e| TriggerError::Capture(format!("present 失败: {e}")))?;
        Ok(())
    }

    fn finish_drag(&mut self) {
        let (x1, y1, x2, y2) = normalize(self.start, self.cursor);
        if x2 - x1 >= MIN_SIZE && y2 - y1 >= MIN_SIZE {
            self.result = Some(ScreenRect {
                x: self.snap.vs.x + x1,
                y: self.snap.vs.y + y1,
                width: x2 - x1,
                height: y2 - y1,
            });
        }
    }
}

fn hline(px: &mut [u32], w: i32, h: i32, y: i32, x1: i32, x2: i32) {
    if y < 0 || y >= h {
        return;
    }
    for x in x1.max(0)..=x2.min(w - 1) {
        px[y as usize * w as usize + x as usize] = BORDER_RGB;
    }
}

fn vline(px: &mut [u32], w: i32, h: i32, x: i32, y1: i32, y2: i32) {
    if x < 0 || x >= w {
        return;
    }
    for y in y1.max(0)..=y2.min(h - 1) {
        px[y as usize * w as usize + x as usize] = BORDER_RGB;
    }
}

/// winit 应用壳：创建覆盖窗 → 事件驱动重绘 → 完成后退出循环。
struct SelectionApp {
    state: Option<SelectionState>,
}

impl ApplicationHandler for SelectionApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = self.state.as_mut() else {
            event_loop.exit();
            return;
        };
        let (vx, vy) = (state.snap.vs.x, state.snap.vs.y);
        let (vw, vh) = (state.snap.vs.width, state.snap.vs.height);
        let attrs = Window::default_attributes()
            .with_title("VerbaSelection")
            .with_decorations(false)
            .with_resizable(false)
            .with_active(true)
            .with_position(position_from_unit(vx, vy, 1.0))
            .with_inner_size(size_from_unit(vw, vh));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => std::rc::Rc::new(w),
            Err(e) => {
                log::warn!("创建选区窗口失败: {e}");
                event_loop.exit();
                return;
            }
        };
        window.set_cursor(winit::window::CursorIcon::Crosshair);
        let context = match softbuffer::Context::new(window.clone()) {
            Ok(c) => std::rc::Rc::new(c),
            Err(e) => {
                log::warn!("softbuffer Context 失败: {e}");
                event_loop.exit();
                return;
            }
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("softbuffer Surface 失败: {e}");
                event_loop.exit();
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
        let scale = window.scale_factor();
        match event {
            WindowEvent::RedrawRequested => {
                if let Err(e) = state.redraw() {
                    log::warn!("选区重绘失败: {e}");
                    event_loop.exit();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.cursor = unit_from_physical(position, scale);
                if state.dragging {
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: btn, button, ..
            } => match (btn, button) {
                (ElementState::Pressed, MouseButton::Left) => {
                    state.dragging = true;
                    state.start = state.cursor;
                    window.request_redraw();
                }
                (ElementState::Released, MouseButton::Left) => {
                    state.finish_drag();
                    event_loop.exit();
                }
                (ElementState::Pressed, MouseButton::Right) => {
                    state.result = None;
                    event_loop.exit();
                }
                _ => {}
            },
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape)
                {
                    state.result = None;
                    event_loop.exit();
                }
            }
            WindowEvent::CloseRequested => {
                state.result = None;
                event_loop.exit();
            }
            _ => {}
        }
    }
}

/// 弹窗选区；Esc / 右键取消返回 Ok(None)，完成返回 Ok(Some(rect))。
pub fn select_region() -> Result<Option<ScreenRect>, TriggerError> {
    let snap = snapshot_for_overlay()?;
    if snap.vs.width <= 0 || snap.vs.height <= 0 {
        return Err(TriggerError::Capture("虚拟屏幕尺寸非法".into()));
    }
    let event_loop = EventLoop::builder()
        .build()
        .map_err(|e| TriggerError::Capture(format!("事件循环创建失败: {e}")))?;
    let mut app = SelectionApp {
        state: Some(SelectionState::new(snap)),
    };
    event_loop
        .run_app(&mut app)
        .map_err(|e| TriggerError::Capture(format!("选区事件循环失败: {e}")))?;
    Ok(app.state.take().and_then(|s| s.result))
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
    fn dim_base_blends_channels() {
        let snap = RgbaSnapshot {
            vs: crate::capture::VirtualScreen {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            rgba: vec![255, 255, 255, 255],
        };
        let v = build_dim_base(&snap)[0];
        assert_eq!((v >> 16) & 0xff, 255 * DIM_NUM / DIM_DEN);
        assert_eq!((v >> 8) & 0xff, 255 * DIM_NUM / DIM_DEN);
        assert_eq!(v & 0xff, 255 * DIM_NUM / DIM_DEN);
    }

    #[test]
    fn hline_clamps_to_buffer() {
        let mut px = vec![0u32; 9]; // 3x3
        hline(&mut px, 3, 3, -1, 0, 2); // 越界行忽略
        hline(&mut px, 3, 3, 1, -5, 99); // 列裁剪
        assert_eq!(px[3], BORDER_RGB);
        assert_eq!(px[4], BORDER_RGB);
        assert_eq!(px[5], BORDER_RGB);
        assert_eq!(px[0], 0);
    }
}
