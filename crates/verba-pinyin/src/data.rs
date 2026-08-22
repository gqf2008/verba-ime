//! 词库加载与索引（include_str! 打包，首次访问解析一次）。

use std::collections::HashSet;
use std::sync::OnceLock;

/// 单字条目：无调拼音 → 频率排序的 (rank, char) 列表。
pub(crate) struct CharBucket {
    pub pinyin: &'static str,
    pub entries: Vec<(u32, char)>,
}

/// 词语条目：无调拼音 → 频率启发排序的 (rank, word) 列表。
pub(crate) struct WordBucket {
    pub pinyin: &'static str,
    /// 拼音按音节切分后的边界位置（如 "nihao" → [2, 5]）。
    pub boundaries: Vec<usize>,
    pub entries: Vec<(u32, &'static str)>,
}

pub(crate) struct Index {
    pub chars: Vec<CharBucket>,
    pub words: Vec<WordBucket>,
}

fn parse_entries(s: &str) -> Vec<(u32, &str)> {
    s.split_whitespace()
        .filter_map(|pair| {
            let (r, t) = pair.split_once(',')?;
            let rank: u32 = r.parse().ok()?;
            Some((rank, t))
        })
        .collect()
}

/// 贪心最长匹配切分拼音为音节，返回各音节结束位置。
pub(crate) fn segment_boundaries(py: &str, syllables: &HashSet<&str>) -> Vec<usize> {
    let mut boundaries = Vec::new();
    let mut pos = 0;
    let b = py.as_bytes();
    while pos < b.len() {
        let mut matched = false;
        // 从长到短尝试音节（最长 6 字符：如 zhuang/chuang）
        for len in (1..=6.min(b.len() - pos)).rev() {
            if let Ok(c) = std::str::from_utf8(&b[pos..pos + len]) {
                if syllables.contains(c) {
                    pos += len;
                    boundaries.push(pos);
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            // 无法切分：整体视为未切分，返回空（调用方按部分前缀处理）
            return Vec::new();
        }
    }
    boundaries
}

fn build_chars(src: &'static str) -> (Vec<CharBucket>, HashSet<&'static str>) {
    let mut syllables = HashSet::new();
    let mut buckets = Vec::new();
    for line in src.lines() {
        let Some((py, rest)) = line.split_once('\t') else {
            continue;
        };
        syllables.insert(py);
        let entries = parse_entries(rest)
            .into_iter()
            .filter_map(|(r, t)| {
                let mut it = t.chars();
                let ch = it.next()?;
                if it.next().is_some() {
                    return None; // 跳过非单字
                }
                Some((r, ch))
            })
            .collect::<Vec<_>>();
        buckets.push(CharBucket {
            pinyin: py,
            entries,
        });
    }
    (buckets, syllables)
}

fn build_words(src: &'static str, syllables: &HashSet<&str>) -> Vec<WordBucket> {
    src.lines()
        .filter_map(|line| {
            let (py, rest) = line.split_once('\t')?;
            let boundaries = segment_boundaries(py, syllables);
            Some(WordBucket {
                pinyin: py,
                boundaries,
                entries: parse_entries(rest),
            })
        })
        .collect()
}

static INDEX: OnceLock<Index> = OnceLock::new();

pub(crate) fn index() -> &'static Index {
    INDEX.get_or_init(|| {
        let (chars, syllables) = build_chars(include_str!("../data/chars.txt"));
        let words = build_words(include_str!("../data/words.txt"), &syllables);
        Index { chars, words }
    })
}
