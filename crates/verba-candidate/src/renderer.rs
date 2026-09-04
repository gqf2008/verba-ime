//! CPU 候选窗渲染器：用 tiny-skia（软件光栅）+ cosmic-text（文字排版/字形）
//! 把候选窗状态画成 RGBA（预乘）缓冲。跨平台：平台层只负责把缓冲 blit 到窗口。
//!
//! 布局：
//! - horizontal（微软拼音/手心风格）：顶部拼音组合头 + 横向候选行 + 页码脚。
//! - vertical：顶部拼音组合头 + 竖向候选列表 + 页码脚（经典回退）。
//! - result（AI 结果浮层）：多行结果文本 + 状态行，无候选/页码脚；
//!   优先于上述两种候选布局（两态互斥）。
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

    /// 渲染当前候选窗状态。窗口尺寸由主题、候选数与结果块行数决定。
    pub fn render(&mut self, ctrl: &CandidateWindowController) -> RenderedCandidateWindow {
        // 结果浮层形态优先于候选布局（两态互斥，结果态优先）。
        if ctrl.result_block().is_some() {
            return self.render_result(ctrl);
        }
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
        let pw = pixmap.width() as f32;
        self.draw_text(pixmap, status, theme.padding as f32, ty, muted, size, pw);
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
        let pw = pixmap.width() as f32;
        self.draw_text(
            pixmap,
            ctrl.preedit(),
            theme.padding as f32 + 4.0,
            ty,
            muted,
            size,
            pw,
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
        let pw = pixmap.width() as f32;
        self.draw_text(pixmap, &page_label, x, ty, muted, size, pw);
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
        let pw = pixmap.width() as f32;

        for (idx, text) in items.iter().enumerate() {
            let is_selected = ctrl.selected_index() == Some(idx);
            let tw = self.text_width(text, font_size);
            let block_w = tw + theme.item_padding as f32 * 2.0;
            if x + block_w > limit {
                self.draw_text(&mut pixmap, "…", x + 4.0, line_top, text_fg, font_size, pw);
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
                pw,
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
        let pw = pixmap.width() as f32;

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
                pw,
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

    /// AI 结果浮层：卡片背景 + 多行结果文本 + 状态行（无候选/页码脚/拼音头
    /// ——阶段提示走状态行，与应用内 preedit 短串各司其职）。
    /// 高度与 `window_size` 的结果分支共用 `result_height`/`result_window_width`
    /// 这一份公式——两处独立计算必致「建窗尺寸 ≠ 位图尺寸」的错位/裁切。
    fn render_result(&mut self, ctrl: &CandidateWindowController) -> RenderedCandidateWindow {
        let theme = ctrl.theme();
        let pad = theme.padding;
        let status = Self::status_height(ctrl);
        let width = result_window_width(theme);
        let height = pad * 2 + result_height(theme, ctrl.result_lines()) + status;
        let mut pixmap = Pixmap::new(width, height).expect("候选窗 pixmap");
        self.draw_card(&mut pixmap, theme, width, height);
        let text = ctrl.result_block().unwrap_or("");
        let text_fg =
            parse_color(&theme.text_color).unwrap_or(Color::from_rgba8(0x33, 0x33, 0x33, 0xFF));
        // draw_text 已支持多行：按 wrap_width 自动换行并逐行下移
        // （layout_runs 的 line_y 差值即行高）。换行宽度与 measure_lines
        // 传入的 result_text_wrap_width 是同一个值，测量与渲染天然一致；
        // 内容宽度 = 窗宽 - 左右内边距（draw 起点 x=pad，满行末字不再
        // 侵入右边距被裁，独立复审 P4）。
        self.draw_text(
            &mut pixmap,
            text,
            pad as f32,
            pad as f32,
            text_fg,
            theme.font_size as f32,
            result_text_wrap_width(theme),
        );
        if status > 0 {
            self.draw_status(&mut pixmap, ctrl, width, (height - status) as f32, status);
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

    /// 测量文本按给定宽度换行后的行数（供平台层在 show 前预测量，
    /// 结果回填 `set_result_lines`，浮层高度才与实际换行一致）。
    ///
    /// Buffer 参数与 `draw_text` 完全一致（同 Metrics / 同 set_size 宽度 /
    /// 同 Shaping），单一 attrs 下 `layout_runs` 每条 run 恰是一行，故 run
    /// 计数即行数——测量与渲染必须出自同一套参数，否则行数漂移 → 高度
    /// 与实际换行不符（末行被裁或底部留白）。
    ///
    /// 注意：DPI 缩放场景须传入**缩放后**的 font_size 与窗口宽度再测
    /// （wrap_width 取 `result_text_wrap_width(scaled_theme)`——与
    /// render_result 的 draw_text 用同一公式，测量/渲染换行点一致）。
    pub fn measure_lines(&mut self, text: &str, font_size: f32, wrap_width: f32) -> usize {
        let metrics = Metrics::new(font_size, font_size * 1.3);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(&mut self.font_system, Some(wrap_width), None);
        buffer.set_text(&mut self.font_system, text, Attrs::new(), Shaping::Advanced);
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer.layout_runs().count().max(1)
    }

    /// 在 (x, y) 画一行文字（cosmic-text 排版 + swash 字形栅格化，
    /// 经 tiny-skia source-over 合成到背景，输出全不透明）。
    /// wrap_width 显式传入（换行宽度不再隐式取整幅 pixmap 宽——结果浮层
    /// 用窗宽减左右内边距的内容宽，独立复审 P4-1），多数调用点传 pixmap 宽。
    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &mut self,
        pixmap: &mut Pixmap,
        text: &str,
        x: f32,
        y: f32,
        color: Color,
        size: f32,
        wrap_width: f32,
    ) {
        let (r8, g8, b8) = (
            (color.red() * 255.0) as u8,
            (color.green() * 255.0) as u8,
            (color.blue() * 255.0) as u8,
        );
        let metrics = Metrics::new(size, size * 1.3);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(&mut self.font_system, Some(wrap_width), None);
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

/// AI 结果浮层的窗口宽度：结果形态两种布局共用这一份公式——
/// `window_size`、`render_result` 与平台层的 `measure_lines` 调用都取此值，
/// 三者的换行宽度才能一致。
pub fn result_window_width(theme: &Theme) -> u32 {
    if theme.layout == "horizontal" {
        theme.max_width_horizontal
    } else {
        theme.max_width
    }
}

/// AI 结果浮层的**文本换行宽度**（窗宽减左右内边距）：render_result 的
/// draw_text 与平台层 measure_lines 共用这一份公式（独立复审 P4-1：以整幅
/// pixmap 宽换行而 draw 起点 x=pad，满行末字会侵入右边距直至被裁）。
pub fn result_text_wrap_width(theme: &Theme) -> f32 {
    (result_window_width(theme).saturating_sub(theme.padding * 2)) as f32
}

/// 结果块总高度——`window_size` 与 `render_result` **共用此唯一公式**。
/// 两处独立计算必致「建窗尺寸 ≠ 位图尺寸」的错位/裁切（本仓库已为
/// 双公式漂移交过学费）。行高与 `draw_text` 的 `Metrics::new(size, size*1.3)`
/// 对齐；ceil 取整保证高度预算 ≥ 实际排版（宁可底部多 1px，不可裁末行）。
pub fn result_height(theme: &Theme, lines: usize) -> u32 {
    (lines as f32 * theme.font_size as f32 * 1.3).ceil() as u32
}

/// 主题尺寸辅助：窗口期望尺寸（多页时含页码脚，供平台层创建窗口）。
/// header/footer/status 高度与 render_* 共用 `CpuCandidateRenderer` 的
/// 同名辅助（同模块直呼，杜绝「两处本该一致的公式分开维护」）；
/// 结果浮层分支共用 `result_height`/`result_window_width`。
pub fn window_size(ctrl: &CandidateWindowController) -> (u32, u32) {
    let theme = ctrl.theme();
    if ctrl.result_block().is_some() {
        // 结果浮层：不画拼音头/页码脚（阶段提示走状态行）。
        return (
            result_window_width(theme),
            theme.padding * 2
                + result_height(theme, ctrl.result_lines())
                + CpuCandidateRenderer::status_height(ctrl),
        );
    }
    let header = CpuCandidateRenderer::header_height(ctrl);
    let footer = CpuCandidateRenderer::footer_height(ctrl);
    let status = CpuCandidateRenderer::status_height(ctrl);
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
    use crate::renderer::{
        result_height, result_text_wrap_width, result_window_width, window_size,
        CpuCandidateRenderer,
    };
    use crate::{CandidateWindowController, Theme};

    /// 组装一个已实测行数的结果浮层控制器（测量 → 回填的标准流程）。
    /// 注意测量对象必须是 set_result_block **截断后**的显示文本，
    /// 不是原始输入——浮层高度对应的是实际会画出来的内容。
    fn result_ctrl(renderer: &mut CpuCandidateRenderer, text: &str) -> CandidateWindowController {
        let mut ctrl = CandidateWindowController::new(Theme::default());
        ctrl.set_result_block(text);
        let width = result_text_wrap_width(ctrl.theme());
        let lines = renderer.measure_lines(
            ctrl.result_block().unwrap(),
            ctrl.theme().font_size as f32,
            width,
        );
        ctrl.set_result_lines(lines);
        ctrl
    }

    /// 换行宽度公式（独立复审 P4-1）：render_result 的 draw_text 与
    /// measure_lines 调用方共用 `result_text_wrap_width`——文本区宽 =
    /// 窗宽 - 左右内边距，满行末字不侵入右边距。锁住恒等式防两处漂移。
    #[test]
    fn result_text_wrap_width_leaves_both_paddings() {
        let theme = Theme::default();
        assert_eq!(
            result_text_wrap_width(&theme),
            (result_window_width(&theme) - theme.padding * 2) as f32
        );
        // 挤压防御：窗宽再小也不会换行到负宽（saturating）。
        let mut tiny = theme.clone();
        tiny.max_width = theme.padding; // 窗宽 < 2*padding
        assert!(result_text_wrap_width(&tiny) >= 0.0);
    }

    #[test]
    fn premultiply_boundaries() {
        // 回归：u8 wrapping_mul 下 255×255 mod 256 = 1 → 1/255 = 0（恒黑 bug）
        assert_eq!(premultiply_channel(255, 255), 255);
        assert_eq!(premultiply_channel(255, 0), 0);
        assert_eq!(premultiply_channel(128, 255), 128);
        assert_eq!(premultiply_channel(255, 128), 128);
        assert_eq!(premultiply_channel(200, 100), 78); // 200*100/255 = 78.4 → 78
    }

    /// 回归（真机 NOTEPAD 4K@150% 候选窗全黑）：DPI 缩放（scaled）后的
    /// 渲染输出必须有不透明像素（卡片背景白底）。若此测试红，说明缩放
    /// 路径 render 出了全透明缓冲——平台层 blit 后窗口全黑不可见。
    #[test]
    fn render_scaled_outputs_opaque_card() {
        let mut renderer = CpuCandidateRenderer::new();
        let mut ctrl = CandidateWindowController::new(Theme::default());
        ctrl.set_candidates(vec!["你".into(), "泥".into(), "拟".into()]);
        ctrl.set_preedit("nihao");
        ctrl.show();
        let scaled = ctrl.scaled(1.5);
        let out = renderer.render(&scaled);
        // 默认 vertical 布局：宽 = max_width(360)；逻辑高 = 6*2 + 32 + 3*30 = 134。
        assert_eq!((out.width, out.height), (540, 201), "150% 缩放尺寸");
        // 中心像素与四角内侧均须不透明（卡片背景填充覆盖整窗）。
        // 采样点须在圆角半径内侧（corner_radius=6 逻辑 ×1.5=9 物理），
        // (2,2) 这类角落点落在圆角抗锯齿带上、alpha 天然 < 255。
        let samples = [
            (out.height / 2, out.width / 2),
            (12, 12),
            (out.height - 13, out.width - 13),
        ];
        for (y, x) in samples {
            let i = (y * out.width + x) as usize * 4;
            assert_eq!(
                out.pixels[i + 3],
                255,
                "({x},{y}) alpha 应为 255，实际全透明"
            );
        }
        // 恒等对照：scale=1.0 的输出同样不透明（原路径回归保护）。
        let out1 = renderer.render(&ctrl);
        let i = ((out1.height / 2) * out1.width + out1.width / 2) as usize * 4;
        assert_eq!(out1.pixels[i + 3], 255);
    }

    /// 锁住「两处本该一致」：结果浮层形态下 `window_size`（建窗/定位依据）
    /// 与 `render`（实际位图）必须给出同一尺寸——结果块高度依赖换行行数，
    /// 任何一边的公式漂移都会让 StretchDIBits 按 mismatched 尺寸拉伸/裁切。
    #[test]
    fn window_size_matches_render_output_for_result_block() {
        let mut renderer = CpuCandidateRenderer::new();
        let mut ctrl = result_ctrl(&mut renderer, &"多行结果文本。".repeat(40));
        assert!(
            ctrl.result_lines() > 1,
            "280 个全角字符在 {}px 宽度下必然折行",
            result_window_width(ctrl.theme())
        );
        ctrl.set_status(Some("生成中…".into()));
        ctrl.show();
        let (w, h) = window_size(&ctrl);
        let out = renderer.render(&ctrl);
        assert_eq!((out.width, out.height), (w, h));
    }

    /// 结果浮层确实画上了文本：结果区应有可观的深色字形像素。
    /// 用 ASCII 文本——任何字体配置下必有字形，不依赖 CJK 字体存在。
    #[test]
    fn result_mode_renders_text_pixels() {
        let mut renderer = CpuCandidateRenderer::new();
        let mut ctrl = result_ctrl(&mut renderer, &"AI result line of text. ".repeat(12));
        ctrl.show();
        let out = renderer.render(&ctrl);
        // 白底 (#FFFFFF) 上统计字形像素（text_color #333333 → R < 128）
        let dark = out
            .pixels
            .chunks_exact(4)
            .filter(|p| p[0] < 128 && p[3] == 255)
            .count();
        assert!(dark > 100, "结果区应有可观数量的文字像素，实际 {dark}");
        let (w, h) = window_size(&ctrl);
        assert_eq!((out.width, out.height), (w, h));
    }

    /// 显示截断使高度有界：超长文本（10 倍于 MAX_RESULT_CHARS）行数收敛
    /// 在截断上限内，窗口高度不再随输入无限增长。断言上界而非精确值
    /// （字体度量跨平台有差异）。
    #[test]
    fn truncated_result_height_bounded() {
        let mut renderer = CpuCandidateRenderer::new();
        let ctrl = result_ctrl(&mut renderer, &"字".repeat(10_000));
        let (w, h) = window_size(&ctrl);
        assert_eq!(w, Theme::default().max_width);
        // 601 字（600+省略号）在 360px 宽、18px 字号下约 30 行，
        // 留足字体度量余量断言上界。
        assert!(h < 1500, "截断后高度应有界，实际 {h}");
    }

    /// 结果浮层形态优先于候选布局：即便候选仍在（前端未清理的中间态），
    /// 尺寸公式也必须走结果分支，与 render 的分派次序一致。
    #[test]
    fn result_mode_takes_priority_over_candidates() {
        let mut ctrl = CandidateWindowController::new(Theme::default());
        ctrl.set_candidates(vec!["你".into(), "泥".into(), "拟".into()]);
        let candidate_size = window_size(&ctrl);
        ctrl.set_result_block("结果");
        ctrl.set_result_lines(2);
        let result_size = window_size(&ctrl);
        assert_ne!(candidate_size, result_size, "两形态尺寸公式应不同");
        assert_eq!(
            result_size.1,
            Theme::default().padding * 2 + result_height(ctrl.theme(), 2),
            "结果分支高度只由 pad*2 + result_height 构成（无状态行）"
        );
    }
}
