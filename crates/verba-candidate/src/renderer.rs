//! CPU 候选窗渲染器：用 tiny-skia（软件光栅）+ cosmic-text（文字排版/字形）
//! 把候选窗状态画成 RGBA（预乘）缓冲。跨平台：平台层只负责把缓冲 blit 到窗口。
//!
//! 布局：
//! - horizontal（默认，微软拼音/手心风格）：顶部拼音组合头 + 横向候选行 + 页码脚。
//! - vertical：顶部拼音组合头 + 竖向候选列表 + 页码脚（经典回退）。
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
        if ctrl.theme().layout == "horizontal" {
            self.render_horizontal(ctrl)
        } else {
            self.render_vertical(ctrl)
        }
    }

    fn header_height(ctrl: &CandidateWindowController) -> u32 {
        let theme = ctrl.theme();
        if theme.show_preedit && !ctrl.preedit().is_empty() {
            theme.header_height
        } else {
            0
        }
    }

    fn footer_height(ctrl: &CandidateWindowController) -> u32 {
        if ctrl.total_pages() > 1 {
            ctrl.theme().footer_height
        } else {
            0
        }
    }

    /// 状态行高度：非空状态时占一行。
    fn status_height(ctrl: &CandidateWindowController) -> u32 {
        if ctrl.status().is_some() {
            (ctrl.theme().font_size + 4).max(16)
        } else {
            0
        }
    }

    /// 在候选窗底部画状态行（弱化色）。
    fn draw_status(
        &mut self,
        pixmap: &mut Pixmap,
        ctrl: &CandidateWindowController,
        _width: u32,
        y: f32,
        status_h: u32,
    ) {
        let Some(status) = ctrl.status() else {
            return;
        };
        let theme = ctrl.theme();
        let muted =
            parse_color(&theme.muted_color).unwrap_or(Color::from_rgba8(0x88, 0x88, 0x88, 0xFF));
        let size = (theme.font_size as f32 * 0.8).max(10.0);
        let ty = y + (status_h as f32 - size * 1.3) / 2.0;
        self.draw_text(pixmap, status, theme.padding as f32, ty, muted, size);
    }

    /// 卡片背景 + 圆角边框（两种布局共用）。
    fn draw_card(&mut self, pixmap: &mut Pixmap, theme: &Theme, width: u32, height: u32) {
        let bg = parse_color(&theme.background).unwrap_or(Color::WHITE);
        if let Some(bg_path) = rounded_rect_path(
            0.5,
            0.5,
            width as f32 - 1.0,
            height as f32 - 1.0,
            theme.corner_radius as f32,
        ) {
            let mut bg_paint = Paint::default();
            bg_paint.set_color(bg);
            pixmap.fill_path(
                &bg_path,
                &bg_paint,
                tiny_skia::FillRule::Winding,
                tiny_skia::Transform::identity(),
                None,
            );
        } else {
            pixmap.fill(bg);
        }
        let border =
            parse_color(&theme.border_color).unwrap_or(Color::from_rgba8(0xCC, 0xCC, 0xCC, 0xFF));
        if let Some(stroke_path) = rounded_rect_path(
            0.5,
            0.5,
            width as f32 - 1.0,
            height as f32 - 1.0,
            theme.corner_radius as f32,
        ) {
            let mut paint = Paint::default();
            paint.set_color(border);
            pixmap.stroke_path(
                &stroke_path,
                &paint,
                &Stroke::default(),
                tiny_skia::Transform::identity(),
                None,
            );
        }
    }

    /// 顶部拼音组合头（灰色 preedit + 底部细分隔线）。
    fn draw_header(
        &mut self,
        pixmap: &mut Pixmap,
        ctrl: &CandidateWindowController,
        width: u32,
        header_h: u32,
    ) {
        if header_h == 0 {
            return;
        }
        let theme = ctrl.theme();
        let muted =
            parse_color(&theme.muted_color).unwrap_or(Color::from_rgba8(0x88, 0x88, 0x88, 0xFF));
        let size = theme.font_size as f32;
        let ty = (header_h as f32 - size * 1.3) / 2.0;
        self.draw_text(
            pixmap,
            ctrl.preedit(),
            theme.padding as f32 + 4.0,
            ty,
            muted,
            size,
        );
        let mut line_paint = Paint::default();
        line_paint.set_color(
            parse_color(&theme.separator_color)
                .unwrap_or(Color::from_rgba8(0xE0, 0xE0, 0xE0, 0xFF)),
        );
        if let Some(r) = tiny_skia::Rect::from_xywh(1.0, header_h as f32, width as f32 - 2.0, 1.0) {
            pixmap.fill_rect(r, &line_paint, tiny_skia::Transform::identity(), None);
        }
    }

    /// 页码脚（多页）：分隔线 + 右对齐页码。
    fn draw_footer(
        &mut self,
        pixmap: &mut Pixmap,
        ctrl: &CandidateWindowController,
        y: f32,
        width: u32,
        footer_h: u32,
    ) {
        let theme = ctrl.theme();
        let muted =
            parse_color(&theme.muted_color).unwrap_or(Color::from_rgba8(0x88, 0x88, 0x88, 0xFF));
        let mut line_paint = Paint::default();
        line_paint.set_color(muted);
        if let Some(r) = tiny_skia::Rect::from_xywh(1.0, y, width as f32 - 2.0, 1.0) {
            pixmap.fill_rect(r, &line_paint, tiny_skia::Transform::identity(), None);
        }
        let page_label = format!("{}/{}", ctrl.current_page() + 1, ctrl.total_pages());
        let size = (theme.font_size as f32 * 0.8).max(10.0);
        let tw = self.text_width(&page_label, size);
        let x = width as f32 - theme.padding as f32 - tw;
        let ty = y + (footer_h as f32 - size * 1.3) / 2.0;
        self.draw_text(pixmap, &page_label, x, ty, muted, size);
    }

    /// horizontal：拼音头 + 横向候选行 + 页码脚（微软拼音/手心风格）。
    fn render_horizontal(&mut self, ctrl: &CandidateWindowController) -> RenderedCandidateWindow {
        let theme = ctrl.theme();
        let items = ctrl.page_items();
        let pad = theme.padding;
        let header = Self::header_height(ctrl);
        let footer = Self::footer_height(ctrl);
        let status = Self::status_height(ctrl);
        let width = theme.max_width_horizontal;
        let height = pad * 2 + header + theme.item_height + footer + status;
        let mut pixmap = Pixmap::new(width, height).expect("候选窗 pixmap");
        self.draw_card(&mut pixmap, theme, width, height);
        self.draw_header(&mut pixmap, ctrl, width, header);

        let sel_bg = parse_color(&theme.selected_background)
            .unwrap_or(Color::from_rgba8(0xD8, 0xE6, 0xFF, 0xFF));
        let sel_fg = parse_color(&theme.selected_text_color)
            .unwrap_or(Color::from_rgba8(0x1A, 0x56, 0xDB, 0xFF));
        let text_fg =
            parse_color(&theme.text_color).unwrap_or(Color::from_rgba8(0x33, 0x33, 0x33, 0xFF));

        let item_y = pad + header;
        let font_size = theme.font_size as f32;
        let line_top = item_y as f32 + (theme.item_height as f32 - font_size * 1.3) / 2.0;
        let mut x = pad as f32;
        let limit = width as f32 - pad as f32;

        for (idx, text) in items.iter().enumerate() {
            let is_selected = ctrl.selected_index() == Some(idx);
            let tw = self.text_width(text, font_size);
            let block_w = tw + theme.item_padding as f32 * 2.0;
            if x + block_w > limit {
                self.draw_text(&mut pixmap, "…", x + 4.0, line_top, text_fg, font_size);
                break;
            }
            if is_selected {
                let sel_r = (theme.corner_radius as f32).min(8.0);
                if let Some(rect) = rounded_rect_path(
                    x,
                    item_y as f32,
                    block_w,
                    theme.item_height as f32 - 1.0,
                    sel_r,
                ) {
                    let mut p = Paint::default();
                    p.set_color(sel_bg);
                    pixmap.fill_path(
                        &rect,
                        &p,
                        tiny_skia::FillRule::Winding,
                        tiny_skia::Transform::identity(),
                        None,
                    );
                }
            }
            let c = if is_selected { sel_fg } else { text_fg };
            self.draw_text(
                &mut pixmap,
                text,
                x + theme.item_padding as f32,
                line_top,
                c,
                font_size,
            );
            x += block_w + theme.gap as f32;
        }

        if status > 0 {
            self.draw_status(
                &mut pixmap,
                ctrl,
                width,
                (height - footer - status) as f32,
                status,
            );
        }
        if footer > 0 {
            self.draw_footer(&mut pixmap, ctrl, (height - footer) as f32, width, footer);
        }
        RenderedCandidateWindow {
            width,
            height,
            pixels: pixmap.take(),
        }
    }

    /// vertical：拼音头 + 竖向候选列表 + 页码脚（经典回退）。
    fn render_vertical(&mut self, ctrl: &CandidateWindowController) -> RenderedCandidateWindow {
        let theme = ctrl.theme();
        let items = ctrl.page_items();
        let pad = theme.padding;
        let header = Self::header_height(ctrl);
        let footer = Self::footer_height(ctrl);
        let status = Self::status_height(ctrl);
        let width = theme.max_width;
        let height = pad * 2 + header + items.len() as u32 * theme.item_height + footer + status;
        let mut pixmap = Pixmap::new(width, height).expect("候选窗 pixmap");
        self.draw_card(&mut pixmap, theme, width, height);
        self.draw_header(&mut pixmap, ctrl, width, header);

        let sel_bg = parse_color(&theme.selected_background)
            .unwrap_or(Color::from_rgba8(0xD8, 0xE6, 0xFF, 0xFF));
        let sel_fg = parse_color(&theme.selected_text_color)
            .unwrap_or(Color::from_rgba8(0x1A, 0x56, 0xDB, 0xFF));
        let text_fg =
            parse_color(&theme.text_color).unwrap_or(Color::from_rgba8(0x33, 0x33, 0x33, 0xFF));

        for (idx, text) in items.iter().enumerate() {
            let y = pad + header + idx as u32 * theme.item_height;
            let is_selected = ctrl.selected_index() == Some(idx);
            if is_selected {
                let sel_r = (theme.corner_radius as f32).min(8.0);
                if let Some(rect) = rounded_rect_path(
                    1.0,
                    y as f32,
                    width as f32 - 2.0,
                    theme.item_height as f32 - 1.0,
                    sel_r,
                ) {
                    let mut p = Paint::default();
                    p.set_color(sel_bg);
                    pixmap.fill_path(
                        &rect,
                        &p,
                        tiny_skia::FillRule::Winding,
                        tiny_skia::Transform::identity(),
                        None,
                    );
                }
            }
            let fg = if is_selected { sel_fg } else { text_fg };
            let label = format!("{}.{text}", idx + 1);
            let line_top =
                y as f32 + (theme.item_height as f32 - theme.font_size as f32 * 1.3) / 2.0;
            self.draw_text(
                &mut pixmap,
                &label,
                pad as f32,
                line_top,
                fg,
                theme.font_size as f32,
            );
        }

        if status > 0 {
            self.draw_status(
                &mut pixmap,
                ctrl,
                width,
                (height - footer - status) as f32,
                status,
            );
        }
        if footer > 0 {
            self.draw_footer(&mut pixmap, ctrl, (height - footer) as f32, width, footer);
        }
        RenderedCandidateWindow {
            width,
            height,
            pixels: pixmap.take(),
        }
    }

    /// 文本排版宽度（用于右对齐 / 横向布局）。
    fn text_width(&mut self, text: &str, size: f32) -> f32 {
        let metrics = Metrics::new(size, size * 1.3);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(&mut self.font_system, Some(10000.0), None);
        buffer.set_text(&mut self.font_system, text, Attrs::new(), Shaping::Advanced);
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer
            .layout_runs()
            .last()
            .map(|run| run.line_w)
            .unwrap_or(0.0)
    }

    /// 在 (x, y) 画一行文字（cosmic-text 排版 + swash 字形栅格化，
    /// 经 tiny-skia source-over 合成到背景，输出全不透明）。
    fn draw_text(
        &mut self,
        pixmap: &mut Pixmap,
        text: &str,
        x: f32,
        y: f32,
        color: Color,
        size: f32,
    ) {
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
        let runs: Vec<_> = buffer.layout_runs().collect();
        let first_line_y = runs.first().map(|run| run.line_y).unwrap_or(0.0);
        for run in &runs {
            for glyph in run.glyphs {
                let pg = glyph.physical((0.0, 0.0), 1.0);
                let Some(img) = self
                    .swash
                    .get_image_uncached(&mut self.font_system, pg.cache_key)
                else {
                    continue;
                };
                let gw = img.placement.width;
                let gh = img.placement.height;
                if gw == 0 || gh == 0 {
                    continue;
                }
                let mask = &img.data[..(gw as usize * gh as usize)];
                let mut gp = match Pixmap::new(gw, gh) {
                    Some(gp) => gp,
                    None => continue,
                };
                for (i, &a) in mask.iter().enumerate() {
                    // 预乘必须用宽类型中间量：u8 `wrapping_mul` 在 255×255 溢出
                    // （mod 256 → 1 → 1/255=0），所有字形像素会塌成不透明黑，
                    // 暗色主题文字完全不可见（架构审查 P1-3）。
                    gp.pixels_mut()[i] = tiny_skia::PremultipliedColorU8::from_rgba(
                        premultiply_channel(r8, a),
                        premultiply_channel(g8, a),
                        premultiply_channel(b8, a),
                        a,
                    )
                    .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
                }
                let gx = (x + pg.x as f32 + img.placement.left as f32).round() as i32;
                let gy = (y + (run.line_y - first_line_y) + pg.y as f32 + img.placement.top as f32
                    - size * 0.5)
                    .round() as i32;
                let paint = PixmapPaint {
                    opacity: color.alpha(),
                    blend_mode: tiny_skia::BlendMode::SourceOver,
                    ..PixmapPaint::default()
                };
                pixmap.draw_pixmap(
                    gx,
                    gy,
                    gp.as_ref(),
                    &paint,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
        }
    }
}

/// 圆角矩形路径（半径按宽高收敛）。
fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<tiny_skia::Path> {
    let r = radius.max(0.0).min(w / 2.0).min(h / 2.0);
    if r <= 0.0 {
        let mut p = PathBuilder::new();
        p.push_rect(Rect::from_xywh(x, y, w, h)?);
        return p.finish();
    }
    let mut p = PathBuilder::new();
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.quad_to(x + w, y, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.quad_to(x + w, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.quad_to(x, y + h, x, y + h - r);
    p.line_to(x, y + r);
    p.quad_to(x, y, x + r, y);
    p.close();
    p.finish()
}

fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim_start_matches(char::from(35));
    if s.len() == 6 {
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some(Color::from_rgba8(r, g, b, 255))
    } else {
        None
    }
}

/// 主题尺寸辅助：窗口期望尺寸（多页时含页码脚，供平台层创建窗口）。
pub fn window_size(ctrl: &CandidateWindowController) -> (u32, u32) {
    let theme = ctrl.theme();
    let header = if theme.show_preedit && !ctrl.preedit().is_empty() {
        theme.header_height
    } else {
        0
    };
    let footer = if ctrl.total_pages() > 1 {
        ctrl.theme().footer_height
    } else {
        0
    };
    let status = if ctrl.status().is_some() {
        (ctrl.theme().font_size + 4).max(16)
    } else {
        0
    };
    if theme.layout == "horizontal" {
        (
            theme.max_width_horizontal,
            theme.padding * 2 + header + theme.item_height + footer + status,
        )
    } else {
        let item_count = ctrl.page_items().len();
        (
            theme.max_width,
            theme.padding * 2 + header + item_count as u32 * theme.item_height + footer + status,
        )
    }
}

/// 单通道预乘（宽类型中间量，避免 u8 `wrapping_mul` 溢出塌成 0）。
fn premultiply_channel(v: u8, a: u8) -> u8 {
    (v as u16 * a as u16 / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::premultiply_channel;

    #[test]
    fn premultiply_boundaries() {
        // 回归：u8 wrapping_mul 下 255×255 mod 256 = 1 → 1/255 = 0（恒黑 bug）
        assert_eq!(premultiply_channel(255, 255), 255);
        assert_eq!(premultiply_channel(255, 0), 0);
        assert_eq!(premultiply_channel(128, 255), 128);
        assert_eq!(premultiply_channel(255, 128), 128);
        assert_eq!(premultiply_channel(200, 100), 78); // 200*100/255 = 78.4 → 78
    }
}
