//! 模糊拼音（容错）：输入串按模糊规则扩展为多个等价串。
//!
//! 规则覆盖常见的"模糊音"：
//! - 声母：zh↔z、ch↔c、sh↔s、n↔l、f↔h、r↔l
//! - 韵母：an↔ang、en↔eng、in↔ing、ian↔iang、uan↔uang
//!
//! 字符串级替换会产生少量无意义变体（如 "shanghai" 中 "an"→"ang" 得 "shangghai"），
//! 它们不会匹配任何真实词条，只会增加少量候选噪音，无害。

use std::collections::HashSet;

/// 模糊规则：`from` → `to`（成对出现表示双向）。
pub(crate) const RULES: &[(&str, &str)] = &[
    ("zh", "z"),
    ("z", "zh"),
    ("ch", "c"),
    ("c", "ch"),
    ("sh", "s"),
    ("s", "sh"),
    ("n", "l"),
    ("l", "n"),
    ("f", "h"),
    ("h", "f"),
    ("r", "l"),
    ("l", "r"),
    ("an", "ang"),
    ("ang", "an"),
    ("en", "eng"),
    ("eng", "en"),
    ("in", "ing"),
    ("ing", "in"),
    ("ian", "iang"),
    ("iang", "ian"),
    ("uan", "uang"),
    ("uang", "uan"),
];

/// 变体数量上限（防止组合爆炸）。
pub(crate) const MAX_VARIANTS: usize = 64;

/// 生成输入的所有模糊等价串（含原串）。
pub(crate) fn fuzzy_expand(input: &str) -> Vec<String> {
    let mut results: HashSet<String> = HashSet::new();
    results.insert(input.to_owned());
    let mut queue: Vec<String> = vec![input.to_owned()];
    while let Some(s) = queue.pop() {
        if results.len() >= MAX_VARIANTS {
            break;
        }
        for (from, to) in RULES {
            if let Some(pos) = s.find(from) {
                let mut t = s.clone();
                t.replace_range(pos..pos + from.len(), to);
                if results.insert(t.clone()) {
                    queue.push(t);
                }
            }
        }
    }
    results.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_shenme_to_senme() {
        let v = fuzzy_expand("shenme");
        assert!(v.iter().any(|x| x == "senme"), "shenme 应含 senme 变体: {v:?}");
        assert!(v.iter().any(|x| x == "shenme"));
    }

    #[test]
    fn expands_senme_to_shenme() {
        let v = fuzzy_expand("senme");
        assert!(v.iter().any(|x| x == "shenme"), "senme 应含 shenme 变体: {v:?}");
    }

    #[test]
    fn expands_zhongguo_to_zongguo() {
        let v = fuzzy_expand("zhongguo");
        assert!(v.iter().any(|x| x == "zongguo"), "zhongguo 应含 zongguo: {v:?}");
    }

    #[test]
    fn expands_nihao_to_lihao() {
        let v = fuzzy_expand("nihao");
        assert!(v.iter().any(|x| x == "lihao"), "nihao 应含 lihao: {v:?}");
    }

    #[test]
    fn bounded() {
        let v = fuzzy_expand("zhongguorendamin");
        assert!(v.len() <= MAX_VARIANTS, "变体应受限: {}", v.len());
    }
}
