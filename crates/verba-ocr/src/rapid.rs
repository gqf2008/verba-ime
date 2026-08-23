//! RapidOCR 本地识别：PaddleOCR + ONNXRuntime（经常驻 Python 子进程）。
//!
//! 本机 Rust 工具链为 `x86_64-pc-windows-gnu`（无 MSVC），`ort` 无 windows-gnu 预编译，
//! 故不走 Rust `ort` 绑定，而是调用用户侧 Python `rapidocr_onnxruntime`（同算法、同模型，
//! 中文识别强于 Windows.Media.Ocr）。
//!
//! - 常驻单进程：把模型加载一次、复用于多次识别，避免每次调用冷启动（~1.5s→热调用 ~0.4s）。
//! - 帧协议：stdin 写 `u32 LE 长度 + 图像字节`；stdout 读 `u32 LE 长度 + 识别文本`。
//!   Python 侧所有库/日志输出重定向到 stderr，防止污染帧流；异常回 `ERROR:<msg>` 帧。
//! - Python 解释器按 `ocr_rapid_python` 配置 → `VERBA_RAPIDOCR_PYTHON` 环境变量 →
//!   `{data_dir}/venv-ocr/Scripts/python.exe`（Windows）/ `bin/python`（unix）→ PATH `python` 依次解析。

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};

use verba_ai::OcrProvider;

use crate::OcrError;

/// 嵌入的常驻 Python 服务脚本。
const HELPER_SCRIPT: &str = r#"# -*- coding: utf-8 -*-
import os, sys, struct, tempfile, time

# 库/日志的 print() 全部重定向到 stderr，避免污染帧协议（stdout 只写帧）。
_orig_stdout = sys.stdout.fileno()
sys.stdout = sys.stderr
OUT = os.fdopen(os.dup(_orig_stdout), 'wb', buffering=0)

def _read_exact(n):
    data = b''
    while len(data) < n:
        chunk = sys.stdin.buffer.read(n - len(data))
        if not chunk:
            return None
        data += chunk
    return data

try:
    from rapidocr_onnxruntime import RapidOCR
except Exception as e:
    print("rapidocr_onnxruntime \u672a\u5b89\u88c5: %s" % e, file=sys.stderr)
    sys.exit(3)

ocr = RapidOCR()

def _frame(text):
    data = text.encode('utf-8')
    OUT.write(struct.pack('<I', len(data)) + data)
    OUT.flush()

def main():
    if os.name == 'nt':
        import msvcrt
        msvcrt.setmode(sys.stdin.fileno(), os.O_BINARY)
        msvcrt.setmode(_orig_stdout, os.O_BINARY)
    while True:
        hdr = _read_exact(4)
        if hdr is None:
            break
        (n,) = struct.unpack('<I', hdr)
        img = _read_exact(n)
        if img is None:
            break
        tmp = None
        try:
            tmp = tempfile.mktemp(suffix='.bmp')
            with open(tmp, 'wb') as f:
                f.write(img)
            result, _ = ocr(tmp)
            text = '\n'.join(str(item[1]) for item in result) if result else ''
        except Exception as e:
            text = 'ERROR:' + str(e)
        finally:
            if tmp and os.path.exists(tmp):
                try:
                    os.remove(tmp)
                except OSError:
                    pass
        _frame(text)

if __name__ == '__main__':
    main()
"#;

/// 常驻 OCR 进程：stdin/stdout 双管道。
struct RapidProcess {
    python: String,
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl Drop for RapidProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 全局共享的常驻 OCR 进程（按 python 路径复用；进程退出自动重启）。
static RAPID_POOL: OnceLock<Mutex<Option<RapidProcess>>> = OnceLock::new();

#[inline]
fn pool() -> &'static Mutex<Option<RapidProcess>> {
    RAPID_POOL.get_or_init(|| Mutex::new(None))
}

/// RapidOCR 识别器。
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
    fn helper_path() -> Result<PathBuf, OcrError> {
        let dir = std::env::temp_dir().join("verba-ocr");
        std::fs::create_dir_all(&dir)
            .map_err(|e| OcrError::Rapid(format!("创建临时目录失败: {e}")))?;
        let path = dir.join("rapidocr_server.py");
        if !path.exists() {
            std::fs::write(&path, HELPER_SCRIPT)
                .map_err(|e| OcrError::Rapid(format!("写 helper 失败: {e}")))?;
        }
        Ok(path)
    }

    /// 常驻识别：spawn_blocking 内完成帧读写（阻塞 IO，不占 async runtime）。
    async fn run(&self, image: &[u8]) -> Result<String, OcrError> {
        let python = self.resolve_python()?;
        let helper = Self::helper_path()?.to_string_lossy().into_owned();
        let img = image.to_vec();
        tokio::task::spawn_blocking(move || process_image(&python, &helper, &img))
            .await
            .map_err(|e| OcrError::Rapid(format!("OCR 线程失败: {e}")))?
    }
}

/// 在常驻进程中完成一次识别（含必要时的重启）。
fn process_image(python: &str, helper: &str, image: &[u8]) -> Result<String, OcrError> {
    let mut guard = pool()
        .lock()
        .map_err(|_| OcrError::Rapid("OCR 池锁中毒".into()))?;
    let should_spawn = match guard.as_mut() {
        Some(p) => p.python != python || !matches!(p.child.try_wait(), Ok(None)),
        None => true,
    };
    if should_spawn {
        *guard = Some(spawn_process(python, helper)?);
    }
    let proc = guard.as_mut().expect("spawn 后必有进程");
    let text = exchange(proc, image)?;
    if let Some(err) = text.strip_prefix("ERROR:") {
        return Err(OcrError::Rapid(err.to_owned()));
    }
    Ok(text)
}

/// 启动常驻 Python OCR 进程。
fn spawn_process(python: &str, helper: &str) -> Result<RapidProcess, OcrError> {
    let mut child = Command::new(python)
        .arg("-u")
        .arg(helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| OcrError::Rapid(format!("启动 Python OCR 进程失败: {e}")))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| OcrError::Rapid("获取 OCR stdin 失败".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| OcrError::Rapid("获取 OCR stdout 失败".into()))?;
    Ok(RapidProcess {
        python: python.to_owned(),
        child,
        stdin,
        stdout,
    })
}

/// 写图像帧、读文本帧。
fn exchange(proc: &mut RapidProcess, image: &[u8]) -> Result<String, OcrError> {
    proc.stdin
        .write_all(&(image.len() as u32).to_le_bytes())
        .map_err(|e| OcrError::Rapid(format!("写图像头失败: {e}")))?;
    proc.stdin
        .write_all(image)
        .map_err(|e| OcrError::Rapid(format!("写图像失败: {e}")))?;
    proc.stdin
        .flush()
        .map_err(|e| OcrError::Rapid(format!("刷新 stdin 失败: {e}")))?;

    let mut rlen = [0u8; 4];
    proc.stdout
        .read_exact(&mut rlen)
        .map_err(|e| OcrError::Rapid(format!("读响应长度失败: {e}")))?;
    let len = u32::from_le_bytes(rlen) as usize;
    let mut buf = vec![0u8; len];
    proc.stdout
        .read_exact(&mut buf)
        .map_err(|e| OcrError::Rapid(format!("读响应失败: {e}")))?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
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
