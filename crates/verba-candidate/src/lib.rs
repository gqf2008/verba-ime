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
}
