//! 屏幕截取：xcap 跨平台实现（Win32 GDI / macOS CoreGraphics / Linux X11）。
//!
//! 坐标约定（issue #82）：全局坐标 = 各显示器边界（xcap Monitor x/y/宽高，
//! 顶左原点；macOS 为点、Windows/Linux 为物理像素）。多显示器选区由
//! 各显示器图像裁剪拼接（capture_region 复合）。
//!
//! 替换原 Windows BitBlt 实现（frontends/windows/ime/src/capture.rs），
//! 对外行为不变：返回 32bpp top-down BMP（daemon OCR 原生输入）。

use xcap::Monitor;

use crate::bmp::{encode_bmp, ScreenShot};
use crate::TriggerError;

/// 屏幕选区（全局坐标，单位同各平台 xcap Monitor）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// 虚拟屏幕边界（多显示器并集，原点可能为负）。
#[derive(Debug, Clone, Copy)]
pub struct VirtualScreen {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl VirtualScreen {
    /// 覆盖区域的全局原点（xcap 点单位）。
    pub fn origin(&self) -> (i32, i32) {
        (self.x, self.y)
    }
}

/// 全部显示器的 RGBA 快照（复合虚拟屏幕）。选区遮罩底图与复合截取共用。
pub(crate) struct RgbaSnapshot {
    pub vs: VirtualScreen,
    /// 32bpp RGBA，top-down，行内无填充，宽 = vs.width。
    pub rgba: Vec<u8>,
}

fn monitors() -> Result<Vec<Monitor>, TriggerError> {
    Monitor::all().map_err(|e| TriggerError::Capture(format!("枚举显示器失败: {e}")))
}

/// 查询虚拟屏幕边界（多显示器并集）。
pub fn virtual_screen() -> Result<VirtualScreen, TriggerError> {
    let list = monitors()?;
    if list.is_empty() {
        return Err(TriggerError::Capture("无可用显示器".into()));
    }
    let (mut x1, mut y1, mut x2, mut y2) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for m in &list {
        let mx = m.x().map_err(|e| TriggerError::Capture(e.to_string()))?;
        let my = m.y().map_err(|e| TriggerError::Capture(e.to_string()))?;
        let mw = m
            .width()
            .map_err(|e| TriggerError::Capture(e.to_string()))?;
        let mh = m
            .height()
            .map_err(|e| TriggerError::Capture(e.to_string()))?;
        x1 = x1.min(mx);
        y1 = y1.min(my);
        x2 = x2.max(mx + mw as i32);
        y2 = y2.max(my + mh as i32);
    }
    Ok(VirtualScreen {
        x: x1,
        y: y1,
        width: x2 - x1,
        height: y2 - y1,
    })
}

/// 复合虚拟屏幕的 RGBA 快照：逐显示器截取，把交集区域拷入统一画布。
///
/// 单位换算（独立审查 Retina 实测）：xcap `Monitor::x/y/width/height` 为
/// **点**（CGDisplayBounds），`capture_image()` 为**物理像素**（Retina 2×，
/// 真机 1470×956 点 → 2940×1912 像素）。画布与选区坐标统一在点网格，
/// 拷贝时按每显示器 `图像/边界` 比例做最近邻采样（一次性成本，正确优先）。
fn snapshot_virtual() -> Result<RgbaSnapshot, TriggerError> {
    let vs = virtual_screen()?;
    let mut rgba = vec![0u8; (vs.width as usize) * (vs.height as usize) * 4];
    for m in monitors()? {
        let mx = m.x().map_err(|e| TriggerError::Capture(e.to_string()))?;
        let my = m.y().map_err(|e| TriggerError::Capture(e.to_string()))?;
        let mw = m
            .width()
            .map_err(|e| TriggerError::Capture(e.to_string()))?;
        let mh = m
            .height()
            .map_err(|e| TriggerError::Capture(e.to_string()))?;
        if mw == 0 || mh == 0 {
            continue;
        }
        let (x1, y1) = (mx.max(vs.x), my.max(vs.y));
        let (x2, y2) = (
            (mx + mw as i32).min(vs.x + vs.width),
            (my + mh as i32).min(vs.y + vs.height),
        );
        if x2 <= x1 || y2 <= y1 {
            continue;
        }
        let img = m
            .capture_image()
            .map_err(|e| TriggerError::Capture(format!("截取显示器失败: {e}")))?;
        let (iw, ih) = img.dimensions();
        let (iw, ih) = (iw as i32, ih as i32);
        if iw <= 0 || ih <= 0 {
            continue;
        }
        // 点 → 像素的采样比例（scale=1 时恒等于 1，走同一采样路径）
        let sx = iw as f64 / mw as f64;
        let sy = ih as f64 / mh as f64;
        let raw = img.as_raw();
        for row in y1..y2 {
            let srow = (((row - my) as f64 * sy) as i32).clamp(0, ih - 1);
            let src_row = (srow as usize) * iw as usize;
            let dst_row = ((row - vs.y) as usize) * vs.width as usize;
            for col in x1..x2 {
                let scol = (((col - mx) as f64 * sx) as i32).clamp(0, iw - 1);
                let src = (src_row + scol as usize) * 4;
                let dst = (dst_row + (col - vs.x) as usize) * 4;
                rgba[dst..dst + 4].copy_from_slice(&raw[src..src + 4]);
            }
        }
    }
    Ok(RgbaSnapshot { vs, rgba })
}

/// RGBA → BGRA（BMP 行内序），原地交换 R/B。
fn rgba_to_bgra(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
}

/// 截取屏幕上的矩形区域（全局坐标，自动裁剪到屏内）。
pub fn capture_region(x: i32, y: i32, width: i32, height: i32) -> Result<ScreenShot, TriggerError> {
    if width <= 0 || height <= 0 {
        return Err(TriggerError::Capture(format!(
            "选区尺寸非法: {width}x{height}"
        )));
    }
    let snap = snapshot_virtual()?;
    let (x1, y1) = (x.max(snap.vs.x), y.max(snap.vs.y));
    let (x2, y2) = (
        (x + width).min(snap.vs.x + snap.vs.width),
        (y + height).min(snap.vs.y + snap.vs.height),
    );
    let (w, h) = (x2 - x1, y2 - y1);
    if w <= 0 || h <= 0 {
        return Err(TriggerError::Capture("选区在屏幕外".into()));
    }
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    for row in 0..h as usize {
        let src = ((row + (y1 - snap.vs.y) as usize) * snap.vs.width as usize
            + (x1 - snap.vs.x) as usize)
            * 4;
        let dst = row * w as usize * 4;
        out[dst..dst + w as usize * 4].copy_from_slice(&snap.rgba[src..src + w as usize * 4]);
    }
    rgba_to_bgra(&mut out);
    Ok(ScreenShot {
        width: w,
        height: h,
        bmp: encode_bmp(w, h, &out),
    })
}

/// 截取主屏全屏（主显示器）。
pub fn capture_primary_screen() -> Result<ScreenShot, TriggerError> {
    let list = monitors()?;
    let m = list
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| list.first())
        .ok_or_else(|| TriggerError::Capture("无可用显示器".into()))?;
    let img = m
        .capture_image()
        .map_err(|e| TriggerError::Capture(format!("截取主屏失败: {e}")))?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(TriggerError::Capture("主屏尺寸为 0".into()));
    }
    let mut bgra = img.into_raw();
    rgba_to_bgra(&mut bgra);
    let (w, h) = (w as i32, h as i32);
    Ok(ScreenShot {
        width: w,
        height: h,
        bmp: encode_bmp(w, h, &bgra),
    })
}

/// 选区遮罩底图：**主显示器** RGBA 快照（供 selection.rs 合成底图）。
///
/// 覆盖窗由 winit 全屏 Borderless 到主屏（跨库坐标拼接在 Retina 上不可靠，
/// 真机实测覆盖层缩在左上角——issue #83 调试），因此快照取主屏边界；
/// 拷贝同 snapshot_virtual：边界(点) × 图像(物理像素) 最近邻采样。
pub(crate) fn snapshot_for_overlay() -> Result<RgbaSnapshot, TriggerError> {
    let list = monitors()?;
    let m = list
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| list.first())
        .ok_or_else(|| TriggerError::Capture("无可用显示器".into()))?;
    let mx = m.x().map_err(|e| TriggerError::Capture(e.to_string()))?;
    let my = m.y().map_err(|e| TriggerError::Capture(e.to_string()))?;
    let mw = m
        .width()
        .map_err(|e| TriggerError::Capture(e.to_string()))?;
    let mh = m
        .height()
        .map_err(|e| TriggerError::Capture(e.to_string()))?;
    if mw == 0 || mh == 0 {
        return Err(TriggerError::Capture("主屏尺寸为 0".into()));
    }
    let img = m
        .capture_image()
        .map_err(|e| TriggerError::Capture(format!("截取主屏失败: {e}")))?;
    let (iw, ih) = img.dimensions();
    let (iw, ih) = (iw as i32, ih as i32);
    if iw <= 0 || ih <= 0 {
        return Err(TriggerError::Capture("主屏图像为空".into()));
    }
    let sx = iw as f64 / mw as f64;
    let sy = ih as f64 / mh as f64;
    let raw = img.as_raw();
    let mut rgba = vec![0u8; (mw as usize) * (mh as usize) * 4];
    for row in 0..mh {
        let srow = ((row as f64 * sy) as i32).clamp(0, ih - 1);
        let src_row = (srow as usize) * iw as usize;
        let dst_row = (row as usize) * mw as usize;
        for col in 0..mw {
            let scol = ((col as f64 * sx) as i32).clamp(0, iw - 1);
            let src = (src_row + scol as usize) * 4;
            let dst = (dst_row + col as usize) * 4;
            rgba[dst..dst + 4].copy_from_slice(&raw[src..src + 4]);
        }
    }
    Ok(RgbaSnapshot {
        vs: VirtualScreen {
            x: mx,
            y: my,
            width: mw as i32,
            height: mh as i32,
        },
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_to_bgra_swaps_channels() {
        let mut px = vec![1u8, 2, 3, 255, 4, 5, 6, 255];
        rgba_to_bgra(&mut px);
        assert_eq!(px, vec![3u8, 2, 1, 255, 6, 5, 4, 255]);
    }
}
