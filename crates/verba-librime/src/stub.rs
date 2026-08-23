//! 非 Windows stub：保持 API 形状，调用返回 `Unsupported`。

use std::path::Path;

use crate::{RimeCandidate, RimeConfig, RimeError, RimeSchema};

pub struct RimeEngine;

impl RimeEngine {
    pub fn new(_cfg: &RimeConfig) -> Result<Self, RimeError> {
        Err(RimeError::Unsupported)
    }

    pub fn schemas(&self) -> Result<Vec<RimeSchema>, RimeError> {
        Err(RimeError::Unsupported)
    }

    pub fn candidates(
        &self,
        _input: &str,
        _schema: &str,
        _max: usize,
    ) -> Result<Vec<RimeCandidate>, RimeError> {
        Err(RimeError::Unsupported)
    }

    #[allow(dead_code)]
    pub fn dll_path(&self) -> &Path {
        Path::new("")
    }
}
