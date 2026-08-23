//! RapidOCR 本地识别：PaddleOCR + ONNXRuntime（经 Python 子进程）。
//!
//! 本机 Rust 工具链为 `x86_64-pc-windows-gnu`（无 MSVC），`ort` 无 windows-gnu 预编译，
//! 故不走 Rust `ort` 绑定，而是调用用户侧 Python `rapidocr_onnxruntime`（同算法、同模型，
//! 中文识别强于 Windows.Media.Ocr）。
//!
//! Python 解释器按 `ocr_rapid_python` 配置 → `VERBA_RAPIDOCR_PYTHON` 环境变量 →
//! `{data_dir}/venv-ocr/Scripts/python.exe`（Windows）/ `bin/python`（unix）→ PATH `python` 依次解析。
//! 模型由 `rapidocr_onnxruntime` 首次运行自动下载（PP-OCRv4，约 10-20MB）。

use std::sync::atomic::{AtomicU64, Ordering};

use verba_ai::OcrProvider;

use crate::OcrError;

/// 嵌入的 Python 辅助脚本（读取图像文件，打识别结果到 stdout）。
const HELPER_SCRIPT: &str = r#"# -*- coding: utf-8 -*-
import sys

def main():
    if len(sys.argv) < 2:
        print("missing image path", file=sys.stderr)
        sys.exit(2)
    img = sys.argv[1]
    try:
        from rapidocr_onnxruntime import RapidOCR
    except Exception as e:
        print("rapidocr_onnxruntime 未安装: %s" % e, file=sys.stderr)
        sys.exit(3)
    ocr = RapidOCR()
    result, _ = ocr(img)
    if result:
        lines = [str(item[1]) for item in result]
        print("\n".join(lines))

if __name__ == "__main__":
    main()
"#;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// RapidOCR 识别器（每次识别调用一次 Python 子进程）。
#[derive(Debug, Clone, Default)]
pub struct RapidOcr {
    python: Option<String>,
}

impl RapidOcr {
    pub fn new() -> Self {
        Self::default()
    }

    /// 指定 Python 解释器路径（覆盖自动探测）。
    pub fn with_python(python: impl Into<String>) -> Self {
        Self {
            python: Some(python.into()),
        }
    }

    /// 解析 Python 解释器路径（见模块头部探测顺序）。
    fn resolve_python(&self) -> Result<String, OcrError> {
        if let Some(p) = &self.python {
            if !p.is_empty() {
                return Ok(p.clone());
            }
        }
        if let Ok(p) = std::env::var("VERBA_RAPIDOCR_PYTHON") {
            if !p.is_empty() {
                return Ok(p);
            }
        }
        if let Ok(dirs) = verba_config::VerbaDirs::locate() {
            let data = dirs.data_dir();
            let win = data.join("venv-ocr").join("Scripts").join("python.exe");
            if win.exists() {
                return Ok(win.to_string_lossy().into_owned());
            }
            let unix = data.join("venv-ocr").join("bin").join("python");
            if unix.exists() {
                return Ok(unix.to_string_lossy().into_owned());
            }
        }
        Ok("python".to_owned())
    }

    /// 确保辅助脚本已写入临时目录并返回其路径。
    fn helper_path() -> Result<std::path::PathBuf, OcrError> {
        let dir = std::env::temp_dir().join("verba-ocr");
        std::fs::create_dir_all(&dir)
            .map_err(|e| OcrError::Rapid(format!("创建临时目录失败: {e}")))?;
        let path = dir.join("rapidocr_helper.py");
        if !path.exists() {
            std::fs::write(&path, HELPER_SCRIPT)
                .map_err(|e| OcrError::Rapid(format!("写 helper 失败: {e}")))?;
        }
        Ok(path)
    }

    /// 写图像到临时文件并调用 Python 识别。
    async fn run(&self, image: &[u8]) -> Result<String, OcrError> {
        let python = self.resolve_python()?;
        let helper = Self::helper_path()?;
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let img_path =
            std::env::temp_dir().join(format!("verba-ocr-{}-{}.bmp", std::process::id(), id));
        std::fs::write(&img_path, image)
            .map_err(|e| OcrError::Rapid(format!("写图像失败: {e}")))?;
        let output = tokio::process::Command::new(&python)
            .arg(&helper)
            .arg(&img_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| OcrError::Rapid(format!("启动 python 失败: {e}")))?;
        let _ = std::fs::remove_file(&img_path);
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(OcrError::Rapid(format!(
                "RapidOCR 失败: {}",
                if err.is_empty() { "未知".into() } else { err }
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

impl OcrProvider for RapidOcr {
    type Error = OcrError;

    async fn recognize(&self, image: &[u8]) -> Result<String, OcrError> {
        if image.is_empty() {
            return Err(OcrError::EmptyImage);
        }
        self.run(image).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_explicit_python() {
        let o = RapidOcr::with_python("C:\\py\\python.exe");
        assert_eq!(o.resolve_python().unwrap(), "C:\\py\\python.exe");
    }

    #[test]
    fn helper_script_contains_rapidocr_import() {
        assert!(HELPER_SCRIPT.contains("rapidocr_onnxruntime"));
    }
}
