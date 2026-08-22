//! 拼音 → 候选 引擎。

use std::collections::HashSet;

use crate::{data, fuzzy};

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

        // 基础档（精确 + 音节边界前缀 + 部分前缀）
        let mut cands = self.collect(&input, 0);

        // 模糊音档：输入的各模糊等价串，统一加 FUZZY_PENALTY（排在基础档之后）
        const FUZZY_PENALTY: u32 = 2000;
        let mut seen: HashSet<String> = HashSet::new();
        for variant in fuzzy::fuzzy_expand(&input) {
            if variant == input || !seen.insert(variant.clone()) {
                continue;
            }
            cands.extend(self.collect(&variant, FUZZY_PENALTY));
        }

        // 简拼档：短输入（≤3 字母）按音节首字母匹配，加 INITIALS_PENALTY
        const INITIALS_PENALTY: u32 = 2600;
        if input.len() <= 3 {
            let lo = idx.initials.partition_point(|b| b.initials < input);
            let mut i = lo;
            while i < idx.initials.len() && idx.initials[i].initials.starts_with(&input) {
                for &(r, w) in &idx.initials[i].entries {
                    cands.push((r + INITIALS_PENALTY, w.to_owned(), CandidateKind::Word));
                }
                i += 1;
            }
        }

        // 整句档：连续拼音 DP 切分组合（如 nishishui → 你是谁）。
        // 分数=频率和 + SENTENCE_PENALTY：真实词典词（含常用短语）排在整句之前，
        // 整句仅作为非词条句子的兜底。
        const SENTENCE_PENALTY: u32 = 500;
        if let Some((score, text)) = self.sentence_candidate(&input) {
            cands.push((score + SENTENCE_PENALTY, text, CandidateKind::Word));
        }

        cands.sort_by_key(|(r, _, _)| *r);
        let mut seen_text = HashSet::new();
        let mut out = Vec::with_capacity(MAX_CANDIDATES);
        for (r, t, k) in cands {
            if seen_text.insert(t.clone()) {
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

    /// 整句候选：对输入的连续拼音做音节切分 + 动态规划，选出
    /// 「词（词典优先）+ 单字」频率和最小的组合（如 "nishishui" → 你是谁）。
    fn sentence_candidate(&self, input: &str) -> Option<(u32, String)> {
        let idx = data::index();
        let syllables = segment_syllables(input, &idx.syllables)?;
        let n = syllables.len();
        if !(2..=12).contains(&n) {
            return None;
        }
        // best[i] = 第 i 个音节到末尾的最优 (分数, 文本)
        let mut best: Vec<Option<(u32, String)>> = vec![None; n + 1];
        best[n] = Some((0, String::new()));
        for i in (0..n).rev() {
            let mut best_here: Option<(u32, String)> = None;
            // 单字（当前音节）
            if let Some((cr, c)) = top_char(syllables[i], idx) {
                if let Some((r, t)) = &best[i + 1] {
                    let cand = (cr + r, format!("{c}{t}"));
                    if best_here.as_ref().is_none_or(|b| cand.0 < b.0) {
                        best_here = Some(cand);
                    }
                }
            }
            // 词典词（跨 2..=6 个音节）
            let mut joined = String::from(syllables[i]);
            for j in (i + 1)..n.min(i + 6) {
                joined.push_str(syllables[j]);
                if let Some((wr, w)) = best_word(&joined, idx) {
                    if let Some((r, t)) = &best[j + 1] {
                        let cand = (wr + r, format!("{w}{t}"));
                        if best_here.as_ref().is_none_or(|b| cand.0 < b.0) {
                            best_here = Some(cand);
                        }
                    }
                }
            }
            best[i] = best_here;
        }
        best[0].clone()
    }

    /// 收集单个拼音串的候选（精确 / 边界前缀 / 部分前缀），分数统一加 `penalty`。
    fn collect(&self, input: &str, penalty: u32) -> Vec<(u32, String, CandidateKind)> {
        let idx = data::index();
        const EXACT_BONUS: u32 = 200;
        const PARTIAL_PENALTY: u32 = 1000;
        let mut cands: Vec<(u32, String, CandidateKind)> = Vec::new();

        // 词语：二分查找定位，精确 + 前缀
        let wlo = idx.words.partition_point(|b| b.pinyin < input);
        if wlo < idx.words.len() && idx.words[wlo].pinyin == input {
            for &(r, w) in &idx.words[wlo].entries {
                cands.push((
                    r.saturating_sub(EXACT_BONUS) + penalty,
                    w.to_owned(),
                    CandidateKind::Word,
                ));
            }
        }
        let mut wi = wlo;
        while wi < idx.words.len() && idx.words[wi].pinyin.starts_with(input) {
            if idx.words[wi].pinyin != input {
                let boundary = idx.words[wi].boundaries.contains(&input.len());
                for &(r, w) in &idx.words[wi].entries {
                    let score = if boundary { r } else { r + PARTIAL_PENALTY };
                    cands.push((score + penalty, w.to_owned(), CandidateKind::Word));
                }
            }
            wi += 1;
        }

        // 单字：二分查找定位
        let clo = idx.chars.partition_point(|b| b.pinyin < input);
        if clo < idx.chars.len() && idx.chars[clo].pinyin == input {
            for &(r, c) in &idx.chars[clo].entries {
                cands.push((
                    r.saturating_sub(EXACT_BONUS) + penalty,
                    c.to_string(),
                    CandidateKind::Char,
                ));
            }
        }
        let mut ci = clo;
        while ci < idx.chars.len() && idx.chars[ci].pinyin.starts_with(input) {
            if idx.chars[ci].pinyin != input {
                for &(r, c) in &idx.chars[ci].entries {
                    cands.push((
                        r + PARTIAL_PENALTY + penalty,
                        c.to_string(),
                        CandidateKind::Char,
                    ));
                }
            }
            ci += 1;
        }

        cands
    }
}
/// 贪心最长匹配把拼音串切成音节；无法切分返回 None。
fn segment_syllables<'a>(
    input: &'a str,
    syllables: &std::collections::HashSet<&str>,
) -> Option<Vec<&'a str>> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let mut matched = false;
        for len in (1..=6.min(bytes.len() - pos)).rev() {
            if let Ok(c) = std::str::from_utf8(&bytes[pos..pos + len]) {
                if syllables.contains(c) {
                    out.push(c);
                    pos += len;
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            return None;
        }
    }
    Some(out)
}

/// 某音节的最高频单字。
fn top_char(syllable: &str, idx: &data::Index) -> Option<(u32, char)> {
    let lo = idx.chars.partition_point(|b| b.pinyin < syllable);
    idx.chars.get(lo).and_then(|b| {
        if b.pinyin == syllable {
            b.entries.first().map(|&(r, c)| (r, c))
        } else {
            None
        }
    })
}

/// 某连续拼音串对应的最高频词典词。
fn best_word(pinyin: &str, idx: &data::Index) -> Option<(u32, &'static str)> {
    let lo = idx.words.partition_point(|b| b.pinyin < pinyin);
    idx.words.get(lo).and_then(|b| {
        if b.pinyin == pinyin {
            b.entries.first().map(|&(r, w)| (r, w))
        } else {
            None
        }
    })
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

    #[test]
    fn fuzzy_senme_finds_shenme() {
        let e = PinyinEngine::new();
        let c = e.lookup("senme");
        assert!(
            c.iter().any(|x| x.text == "什么"),
            "模糊音 senme 应含 什么（shenme），实际 {c:?}"
        );
    }

    #[test]
    fn fuzzy_zongguo_finds_zhongguo() {
        let e = PinyinEngine::new();
        let c = e.lookup("zongguo");
        assert!(
            c.iter().any(|x| x.text == "中国"),
            "模糊音 zongguo 应含 中国（zhongguo），实际 {c:?}"
        );
    }

    #[test]
    fn fuzzy_lihao_finds_nihao() {
        let e = PinyinEngine::new();
        let c = e.lookup("lihao");
        assert!(
            c.iter().any(|x| x.text == "你好"),
            "模糊音 lihao 应含 你好（nihao），实际 {c:?}"
        );
    }

    #[test]
    fn fuzzy_does_not_push_out_exact() {
        let e = PinyinEngine::new();
        // 精确匹配仍居首
        assert_eq!(e.lookup("nihao")[0].text, "你好");
        assert_eq!(e.lookup("zhongguo")[0].text, "中国");
    }

    #[test]
    fn initials_nh_finds_nihao() {
        let e = PinyinEngine::new();
        let c = e.lookup("nh");
        assert!(
            c.iter().any(|x| x.text == "你好"),
            "简拼 nh 应含 你好，实际 {c:?}"
        );
    }

    #[test]
    fn initials_zg_finds_zhongguo() {
        let e = PinyinEngine::new();
        let c = e.lookup("zg");
        assert!(
            c.iter().any(|x| x.text == "中国"),
            "简拼 zg 应含 中国（zhongguo），实际 {c:?}"
        );
    }
}
