//! CPU 候选窗渲染器：用 tiny-skia（软件光栅）+ cosmic-text（文字排版/字形）
//! 把候选窗状态画成 RGBA（预乘）缓冲。跨平台：平台层只负责把缓冲 blit 到窗口。
//!
//! 字体：cosmic-text 的 FontSystem 自动发现系统字体（Windows 微软雅黑 /
//! macOS PingFang / Linux Noto CJK），无需打包字体。

use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, SwashCache};
use tiny_skia::{Color, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke};

use crate::{CandidateWindowController, Theme};

/// 渲染结果：RGBA（预乘）像素缓冲。
pub struct RenderedCandidateWindow {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// CPU 候选窗渲染器（无窗口、无 GPU）。
pub struct CpuCandidateRenderer {
    font_system: FontSystem,
    swash: SwashCache,
}

impl Default for CpuCandidateRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuCandidateRenderer {
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash: SwashCache::new(),
        }
    }

    /// 渲染当前候选窗状态。窗口尺寸由主题与候选数决定。
    pub fn render(&mut self, ctrl: &CandidateWindowController) -> RenderedCandidateWindow {
        let theme = ctrl.theme();
        let items = ctrl.page_items();
        let pad = theme.padding;
        let width = theme.max_width;
        let height = pad * 2 + items.len() as u32 * theme.item_height;
        let mut pixmap = Pixmap::new(width, height).expect("候选窗 pixmap");

        // 背景
        pixmap.fill(parse_color(&theme.background).unwrap_or(Color::WHITE));

        // 边框
        let border = parse_color(&theme.border_color).unwrap_or(Color::from_rgba8(0xCC, 0xCC, 0xCC, 0xFF));
        let mut path = PathBuilder::new();
        path.push_rect(Rect::from_xywh(0.5, 0.5, width as f32 - 1.0, height as f32 - 1.0).unwrap());
        let mut paint = Paint::default();
        paint.set_color(border);
        if let Some(stroke_path) = path.finish() {
            pixmap.stroke_path(
                &stroke_path,
                &paint,
                &Stroke::default(),
                tiny_skia::Transform::identity(),
                None,
            );
        }

        // 候选行
        let sel_bg = parse_color(&theme.selected_background).unwrap_or(Color::from_rgba8(0xD8, 0xE6, 0xFF, 0xFF));
        let sel_fg = parse_color(&theme.selected_text_color).unwrap_or(Color::from_rgba8(0x1A, 0x56, 0xDB, 0xFF));
        let text_fg = parse_color(&theme.text_color).unwrap_or(Color::from_rgba8(0x33, 0x33, 0x33, 0xFF));

        for (idx, text) in items.iter().enumerate() {
            let y = pad + idx as u32 * theme.item_height;
            let is_selected = ctrl.selected_index() == Some(idx);
            if is_selected {
                let mut path = PathBuilder::new();
                path.push_rect(Rect::from_xywh(
                    1.0,
                    y as f32,
                    width as f32 - 2.0,
                    theme.item_height as f32 - 1.0,
                ).unwrap());
                if let Some(rect) = path.finish() {
                    let mut p = Paint::default();
                    p.set_color(sel_bg);
                    pixmap.fill_path(&rect, &p, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
                }
            }
            let fg = if is_selected { sel_fg } else { text_fg };
            let label = format!("{}.{text}", idx + 1);
            // 文字行在候选行内垂直居中
            let line_top = y as f32 + (theme.item_height as f32 - theme.font_size as f32 * 1.3) / 2.0;
            self.draw_text(&mut pixmap, &label, pad as f32, line_top, fg, theme.font_size as f32);
        }

        RenderedCandidateWindow {
            width,
            height,
            pixels: pixmap.take(),
        }
    }

    /// 在 (x, y) 画一行文字（cosmic-text 排版 + swash 字形栅格化，
    /// 经 tiny-skia source-over 合成到背景，输出全不透明）。
    fn draw_text(&mut self, pixmap: &mut Pixmap, text: &str, x: f32, y: f32, color: Color, size: f32) {
        let (r8, g8, b8) = (
            (color.red() * 255.0) as u8,
            (color.green() * 255.0) as u8,
            (color.blue() * 255.0) as u8,
        );
        let metrics = Metrics::new(size, size * 1.3);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(&mut self.font_system, Some(pixmap.width() as f32), None);
        buffer.set_text(&mut self.font_system, text, Attrs::new(), Shaping::Advanced);
        buffer.shape_until_scroll(&mut self.font_system, false);
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                let pg = glyph.physical((0.0, 0.0), 1.0);
                let Some(img) = self.swash.get_image_uncached(&mut self.font_system, pg.cache_key) else {
                    continue;
                };
                let gw = img.placement.width;
                let gh = img.placement.height;
                if gw == 0 || gh == 0 {
                    continue;
                }
                // Mask 内容：data 为 alpha 蒙版（1 字节/像素），按文字色着色
                let mask = &img.data[..(gw as usize * gh as usize)];
                let mut gp = match Pixmap::new(gw, gh) {
                    Some(gp) => gp,
                    None => continue,
                };
                for (i, &a) in mask.iter().enumerate() {
                    gp.pixels_mut()[i] = tiny_skia::PremultipliedColorU8::from_rgba(
                        r8.wrapping_mul(a) / 255,
                        g8.wrapping_mul(a) / 255,
                        b8.wrapping_mul(a) / 255,
                        a,
                    )
                    .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
                }
                let gx = (x + pg.x as f32 + img.placement.left as f32).round() as i32;
                let gy = (y + run.line_y + pg.y as f32 + img.placement.top as f32).round() as i32;
                let paint = PixmapPaint {
                    opacity: color.alpha(),
                    blend_mode: tiny_skia::BlendMode::SourceOver,
                    ..PixmapPaint::default()
                };
                pixmap.draw_pixmap(gx, gy, gp.as_ref(), &paint, tiny_skia::Transform::identity(), None);
            }
        }
    }
}

fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim_start_matches('#');
    if s.len() == 6 {
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some(Color::from_rgba8(r, g, b, 255))
    } else {
        None
    }
}

/// 主题尺寸辅助：窗口期望尺寸（供平台层创建窗口）。
pub fn window_size(theme: &Theme, item_count: usize) -> (u32, u32) {
    (
        theme.max_width,
        theme.padding * 2 + item_count as u32 * theme.item_height,
    )
}
