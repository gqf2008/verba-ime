//! 拼音 → 候选 引擎。

use std::collections::HashSet;

use crate::data;

/// 候选类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    /// 词语（含单字词）
    Word,
    /// 单字
    Char,
}

/// 一个候选。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    pub kind: CandidateKind,
    /// 频率排名（越小越常用），用于排序/去重。
    pub rank: u32,
}

/// 每次查询返回的最大候选数。
pub const MAX_CANDIDATES: usize = 9;

/// 拼音引擎（无状态；数据在首次使用时解析并缓存）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PinyinEngine;

impl PinyinEngine {
    pub fn new() -> Self {
        Self
    }

    /// 查询给定无调拼音串的候选（不区分大小写）。
    ///
    /// 候选来源（按此顺序合并后按频率排名升序）：
    /// 1. 拼音**完全匹配**的词语
    /// 2. 拼音**完全匹配**的单字
    /// 3. 拼音以输入为**前缀**的词语（输入未完整时补全）
    /// 4. 拼音以输入为**前缀**的单字
    pub fn lookup(&self, input: &str) -> Vec<Candidate> {
        let input = normalize(input);
        if input.is_empty() {
            return Vec::new();
        }
        let idx = data::index();

        // 分档：完全匹配 < 音节边界前缀 < 部分前缀；档内按频率升序
        //   exact      : score = rank - 200   （完全匹配优先）
        //   boundary   : score = rank         （输入正好是词拼音的音节边界，如 "nihao" 对 "nihaoma"）
        //   partial    : score = rank + 1000  （输入在音节中间，如 "nih" 对 "nihao"）
        const EXACT_BONUS: u32 = 200;
        const PARTIAL_PENALTY: u32 = 1000;
        let mut cands: Vec<(u32, String, CandidateKind)> = Vec::new();

        for bucket in &idx.words {
            if bucket.pinyin == input {
                for &(r, w) in &bucket.entries {
                    cands.push((
                        r.saturating_sub(EXACT_BONUS),
                        w.to_owned(),
                        CandidateKind::Word,
                    ));
                }
            } else if bucket.pinyin.len() > input.len() && bucket.pinyin.starts_with(&input) {
                let boundary = bucket.boundaries.contains(&input.len());
                for &(r, w) in &bucket.entries {
                    let score = if boundary { r } else { r + PARTIAL_PENALTY };
                    cands.push((score, w.to_owned(), CandidateKind::Word));
                }
            }
        }

        for bucket in &idx.chars {
            if bucket.pinyin == input {
                for &(r, c) in &bucket.entries {
                    cands.push((
                        r.saturating_sub(EXACT_BONUS),
                        c.to_string(),
                        CandidateKind::Char,
                    ));
                }
            } else if bucket.pinyin.len() > input.len() && bucket.pinyin.starts_with(&input) {
                for &(r, c) in &bucket.entries {
                    cands.push((r + PARTIAL_PENALTY, c.to_string(), CandidateKind::Char));
                }
            }
        }

        cands.sort_by_key(|(r, _, _)| *r);
        let mut seen = HashSet::new();
        let mut out = Vec::with_capacity(MAX_CANDIDATES);
        for (r, t, k) in cands {
            if seen.insert(t.clone()) {
                out.push(Candidate {
                    text: t,
                    kind: k,
                    rank: r,
                });
                if out.len() >= MAX_CANDIDATES {
                    break;
                }
            }
        }
        out
    }
}

/// 归一化输入：去空白、转小写。
pub fn normalize(input: &str) -> String {
    input
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn ni_returns_ni_first() {
        let e = PinyinEngine::new();
        let c = e.lookup("ni");
        assert!(!c.is_empty(), "ni 应有候选");
        assert_eq!(c[0].text, "你", "ni 首选应为 你，实际 {c:?}");
        assert!(c.iter().any(|x| x.text == "你好"), "ni 应含前缀词 你好");
    }

    #[test]
    fn wo_returns_wo_first() {
        let e = PinyinEngine::new();
        assert_eq!(e.lookup("wo")[0].text, "我");
    }

    #[test]
    fn nihao_word() {
        let e = PinyinEngine::new();
        let c = e.lookup("nihao");
        assert_eq!(c[0].text, "你好", "nihao 首选应为 你好，实际 {c:?}");
    }

    #[test]
    fn zhongguo_word() {
        let e = PinyinEngine::new();
        assert_eq!(e.lookup("zhongguo")[0].text, "中国");
    }

    #[test]
    fn case_insensitive() {
        let e = PinyinEngine::new();
        assert_eq!(e.lookup("Nihao")[0].text, "你好");
    }

    #[test]
    fn prefix_single_letter() {
        let e = PinyinEngine::new();
        let c = e.lookup("n");
        assert!(!c.is_empty());
        assert!(
            c.iter().any(|x| x.text == "你" || x.text == "那"),
            "n 前缀应含常见字，实际 {c:?}"
        );
    }

    #[test]
    fn max_candidates_capped() {
        let e = PinyinEngine::new();
        let c = e.lookup("n");
        assert!(c.len() <= MAX_CANDIDATES);
        let c = e.lookup("ni");
        assert!(c.len() <= MAX_CANDIDATES);
    }

    #[test]
    fn empty_input_no_candidates() {
        let e = PinyinEngine::new();
        assert!(e.lookup("").is_empty());
        assert!(e.lookup("  ").is_empty());
    }
}
