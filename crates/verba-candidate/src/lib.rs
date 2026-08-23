//! 候选窗口共享逻辑（纯 Rust、跨平台、无 UI 依赖）。
//!
//! 平台适配层（Windows 自绘窗 / macOS IMK / Linux fcitx5-IBus）只负责渲染，
//! 候选列表、分页、选择、主题等全部在此控制器内完成，可离线单测。

#![forbid(unsafe_code)]

pub mod renderer;

use serde::{Deserialize, Serialize};

/// 主题 token：各端渲染器消费同一套颜色/尺寸。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    pub background: String,
    pub text_color: String,
    pub selected_background: String,
    pub selected_text_color: String,
    pub border_color: String,
    pub font_size: u32,
    pub padding: u32,
    pub item_height: u32,
    pub page_size: usize,
    pub max_width: u32,
    /// 圆角半径（像素）。
    #[serde(default = "default_corner_radius")]
    pub corner_radius: u32,
    /// 布局：horizontal（横向候选行，微软拼音/手心风格）| vertical（竖向列表）。
    #[serde(default = "default_layout")]
    pub layout: String,
    /// 是否在顶部显示拼音组合串（preedit 头）。
    #[serde(default = "default_show_preedit")]
    pub show_preedit: bool,
    /// 头部高度（show_preedit 且 preedit 非空时占用）。
    #[serde(default = "default_header_height")]
    pub header_height: u32,
    /// 候选间距（horizontal 布局）。
    #[serde(default = "default_gap")]
    pub gap: u32,
    /// horizontal 布局的窗口最大宽度。
    #[serde(default = "default_max_width_horizontal")]
    pub max_width_horizontal: u32,
    /// 候选块内左右留白（horizontal）。
    #[serde(default = "default_item_padding")]
    pub item_padding: u32,
    /// 页码脚高度（仅多页时占用）。
    #[serde(default = "default_footer_height")]
    pub footer_height: u32,
    /// 拼音组合头文字色（如 `#888888`）。
    #[serde(default = "default_header_text_color")]
    pub header_text_color: String,
    /// 分隔线颜色（拼音头/页码脚下）。
    #[serde(default = "default_separator_color")]
    pub separator_color: String,
    /// 弱化文字色（页码脚）。
    #[serde(default = "default_muted_color")]
    pub muted_color: String,
}

fn default_corner_radius() -> u32 {
    6
}
fn default_layout() -> String {
    "horizontal".to_owned()
}
fn default_show_preedit() -> bool {
    true
}
fn default_header_height() -> u32 {
    24
}
fn default_gap() -> u32 {
    10
}
fn default_max_width_horizontal() -> u32 {
    560
}
fn default_item_padding() -> u32 {
    4
}
fn default_footer_height() -> u32 {
    18
}
fn default_header_text_color() -> String {
    "#888888".to_owned()
}
fn default_separator_color() -> String {
    "#E0E0E0".to_owned()
}
fn default_muted_color() -> String {
    "#888888".to_owned()
}
impl Default for Theme {
    fn default() -> Self {
        Self {
            background: "#FFFFFF".into(),
            text_color: "#333333".into(),
            selected_background: "#D8E6FF".into(),
            selected_text_color: "#1A56DB".into(),
            border_color: "#CCCCCC".into(),
            font_size: 14,
            padding: 6,
            item_height: 22,
            page_size: 9,
            max_width: 360,
            corner_radius: default_corner_radius(),
            layout: default_layout(),
            show_preedit: default_show_preedit(),
            header_height: default_header_height(),
            gap: default_gap(),
            max_width_horizontal: default_max_width_horizontal(),
            item_padding: default_item_padding(),
            footer_height: default_footer_height(),
            header_text_color: default_header_text_color(),
            separator_color: default_separator_color(),
            muted_color: default_muted_color(),
        }
    }
}

impl Theme {
    /// 暗色预设（深灰底 + 亮色文字 + 蓝色选中）。
    pub fn dark() -> Self {
        Self {
            background: "#1E1E1E".into(),
            text_color: "#D4D4D4".into(),
            selected_background: "#264F78".into(),
            selected_text_color: "#FFFFFF".into(),
            border_color: "#3C3C3C".into(),
            font_size: 14,
            padding: 6,
            item_height: 22,
            page_size: 9,
            max_width: 360,
            corner_radius: default_corner_radius(),
            layout: default_layout(),
            show_preedit: default_show_preedit(),
            header_height: default_header_height(),
            gap: default_gap(),
            max_width_horizontal: default_max_width_horizontal(),
            item_padding: default_item_padding(),
            footer_height: default_footer_height(),
            header_text_color: default_header_text_color(),
            separator_color: default_separator_color(),
            muted_color: default_muted_color(),
        }
    }
}

/// 候选窗口控制器（纯逻辑）。
#[derive(Debug, Clone)]
pub struct CandidateWindowController {
    theme: Theme,
    candidates: Vec<String>,
    page: usize,
    selected: Option<usize>,
    visible: bool,
    /// 逻辑坐标（像素），由平台适配层填充。
    position: Option<(i32, i32)>,
    /// 当前组合串（拼音/preedit），用于候选窗头部展示。
    preedit: String,
}

impl Default for CandidateWindowController {
    fn default() -> Self {
        Self::new(Theme::default())
    }
}

impl CandidateWindowController {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            candidates: Vec::new(),
            page: 0,
            selected: None,
            visible: false,
            position: None,
            preedit: String::new(),
        }
    }

    // ---- 数据 ----

    /// 设置候选列表（页码/选中重置）。
    pub fn set_candidates(&mut self, candidates: Vec<String>) {
        self.candidates = candidates;
        self.page = 0;
        self.selected = Some(0);
    }

    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }

    /// 当前页的候选切片。
    pub fn page_items(&self) -> &[String] {
        let start = self.page * self.theme.page_size;
        let end = (start + self.theme.page_size).min(self.candidates.len());
        if start >= end {
            &[]
        } else {
            &self.candidates[start..end]
        }
    }

    /// 总页数（0 候选时为 0）。
    pub fn total_pages(&self) -> usize {
        self.candidates.len().div_ceil(self.theme.page_size)
    }

    pub fn current_page(&self) -> usize {
        self.page
    }

    pub fn page_size(&self) -> usize {
        self.theme.page_size
    }

    // ---- 分页 ----

    pub fn next_page(&mut self) {
        if self.total_pages() > 1 {
            self.page = (self.page + 1) % self.total_pages();
            self.selected = Some(0);
        }
    }

    pub fn prev_page(&mut self) {
        if self.total_pages() > 1 {
            self.page = (self.page + self.total_pages() - 1) % self.total_pages();
            self.selected = Some(0);
        }
    }

    /// 设置当前页码（0 起，越界钳制；与状态机页码保持同步）。
    pub fn set_page(&mut self, page: usize) {
        if self.candidates.is_empty() {
            self.page = 0;
        } else {
            self.page = page.min(self.total_pages() - 1);
        }
        self.selected = Some(0);
    }

    // ---- 选择 ----

    /// 相对当前页的选中下标（None = 未选中）。
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// 全局下标（含页偏移）。
    pub fn selected_global(&self) -> Option<usize> {
        self.selected.map(|i| self.page * self.theme.page_size + i)
    }

    pub fn select_relative(&mut self, idx: usize) -> bool {
        if idx < self.page_items().len() {
            self.selected = Some(idx);
            true
        } else {
            false
        }
    }

    /// 选中并返回候选文本（用于上屏）；无候选返回 None。
    pub fn commit_selected(&mut self) -> Option<String> {
        let global = self.selected_global()?;
        let text = self.candidates.get(global)?.clone();
        self.hide();
        Some(text)
    }

    /// 按下数字键 1-9 选择对应候选；越界返回 None。
    pub fn select_number(&mut self, n: u32) -> Option<String> {
        if n < 1 || n > self.theme.page_size as u32 {
            return None;
        }
        let idx = (n - 1) as usize;
        if self.select_relative(idx) {
            self.commit_selected()
        } else {
            None
        }
    }

    // ---- 显隐 / 位置 ----

    pub fn show(&mut self) {
        if !self.candidates.is_empty() {
            self.visible = true;
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    /// 设置当前组合串（拼音/preedit），候选窗头部展示。
    pub fn set_preedit(&mut self, preedit: &str) {
        self.preedit = preedit.to_owned();
    }

    /// 当前组合串（拼音/preedit）。
    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    pub fn set_position(&mut self, x: i32, y: i32) {
        self.position = Some((x, y));
    }

    pub fn position(&self) -> Option<(i32, i32)> {
        self.position
    }

    /// 是否需要显示（有候选且可见）。
    pub fn should_render(&self) -> bool {
        self.visible && !self.candidates.is_empty()
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl() -> CandidateWindowController {
        CandidateWindowController::new(Theme {
            page_size: 3,
            ..Theme::default()
        })
    }

    #[test]
    fn pagination_and_selection() {
        let mut c = ctrl();
        c.set_candidates(vec![
            "你".into(),
            "你们".into(),
            "你好".into(),
            "您".into(),
            "尼".into(),
        ]);
        assert_eq!(c.total_pages(), 2);
        assert_eq!(c.current_page(), 0);
        assert_eq!(c.page_items(), &["你", "你们", "你好"]);
        c.next_page();
        assert_eq!(c.current_page(), 1);
        assert_eq!(c.page_items(), &["您", "尼"]);
        assert_eq!(c.select_number(2), Some("尼".into()));
        assert!(!c.visible());
    }

    #[test]
    fn number_select_within_page() {
        let mut c = ctrl();
        c.set_candidates(vec!["你".into(), "您".into(), "尼".into()]);
        assert_eq!(c.select_number(1), Some("你".into()));
        assert_eq!(c.select_number(3), Some("尼".into()));
    }

    #[test]
    fn out_of_range_number_ignored() {
        let mut c = ctrl();
        c.set_candidates(vec!["你".into()]);
        assert_eq!(c.select_number(2), None);
        assert!(c.visible() || !c.should_render());
    }

    #[test]
    fn empty_candidates_no_render() {
        let mut c = ctrl();
        c.show();
        assert!(!c.should_render());
    }

    #[test]
    fn set_page_clamps_and_syncs() {
        let mut c = ctrl();
        c.set_candidates(vec![
            "你".into(),
            "你们".into(),
            "你好".into(),
            "您".into(),
            "尼".into(),
        ]);
        c.set_page(5); // 越界 → 钳制到最后一页
        assert_eq!(c.current_page(), 1);
        assert_eq!(c.page_items(), &["您", "尼"]);
        c.set_page(0);
        assert_eq!(c.page_items(), &["你", "你们", "你好"]);
    }

    #[test]
    fn next_page_wraps() {
        let mut c = ctrl();
        c.set_candidates((0..5).map(|i| format!("c{i}")).collect());
        c.next_page();
        c.next_page();
        assert_eq!(c.current_page(), 0, "应回绕");
    }

    #[test]
    fn theme_roundtrip_serde() {
        let t = Theme::default();
        let s = serde_json::to_string(&t).unwrap();
        let t2: Theme = serde_json::from_str(&s).unwrap();
        assert_eq!(t, t2);
    }

    #[test]
    fn theme_dark_preset_differs() {
        let d = Theme::dark();
        assert_ne!(d.background, Theme::default().background);
        assert_eq!(d.background, "#1E1E1E");
        assert_eq!(d.corner_radius, Theme::default().corner_radius);
    }

    #[test]
    fn theme_serde_backcompat_missing_corner_radius() {
        // 旧版序列化无 corner_radius 字段，应能反序列化并取默认值。
        let s = r##"{"background":"#FFFFFF","text_color":"#333333","selected_background":"#D8E6FF","selected_text_color":"#1A56DB","border_color":"#CCCCCC","font_size":14,"padding":6,"item_height":22,"page_size":9,"max_width":360}"##;
        let t: Theme = serde_json::from_str(s).unwrap();
        assert_eq!(t.corner_radius, Theme::default().corner_radius);
    }
}
