//! Verba 拼音引擎：本地拼音 → 汉字候选。
//!
//! 数据（见 `data/` 与 `data/README.md`）：
//! - 单字：来自 Jun Da 现代汉语字频表（hanzi_db.csv，频率排序）
//! - 词语：来自 CC-CEDICT（phrase-pinyin-data）
//!
//! 引擎提供：
//! - [`lookup`]：给定无调拼音串，返回候选（词语优先 + 单字，按频率排序）
//! - 前缀匹配：输入部分拼音时也返回完整拼音以该前缀开头的候选

#![forbid(unsafe_code)]

mod data;
mod engine;
mod fuzzy;

pub use engine::{Candidate, CandidateKind, PinyinEngine};
