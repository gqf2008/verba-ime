//! RapidOCR 本地识别：PaddleOCR + ONNXRuntime（原生 Rust，无需 Python）。
//!
//! 本机 Rust 工具链现为 `x86_64-pc-windows-msvc`（MSVC 已安装），`ort` 提供 windows-msvc
//! 预编译 onnxruntime，故直接内嵌 `rapidocr-core`（PP-OCRv5 中文 mobile 模型），
//! 不再拉起任何 Python 子进程，也更适合作为系统级输入法的本地 OCR。
//!
//! - 模型自动下载到 `{data_dir}/models-rapidocr`（首次运行，约 10-20MB，校验 SHA-256）。
//! - 常驻一个 `RapidOcr` 运行器（ONNX session 有状态），串行执行识别。
//! - `spawn_blocking` 包裹阻塞推理，不占 daemon async 运行时。

use std::sync::{Mutex, OnceLock};

use verba_ai::OcrProvider;

use crate::OcrError;

/// 常驻的快速 OCR 运行器（ONNX sessions 有状态，需串行访问）。
struct RapidOcrRunner {
    runner: rapidocr_core::RapidOcr,
}

static RAPID_POOL: OnceLock<Mutex<Option<RapidOcrRunner>>> = OnceLock::new();

#[inline]
fn pool() -> &'static Mutex<Option<RapidOcrRunner>> {
    RAPID_POOL.get_or_init(|| Mutex::new(None))
}

/// RapidOCR 识别器（无内部状态；占用全局常驻运行器）。
#[derive(Debug, Clone, Default)]
pub struct RapidOcr {
    /// 保留原 API（Python 时代），现忽略——纯原生，不再需要解释器。
    _python: Option<String>,
}

impl RapidOcr {
    pub fn new() -> Self {
        Self::default()
    }

    /// 兼容旧签名：忽略 Python 解释器（已无 Python 依赖）。
    pub fn with_python(_python: impl Into<String>) -> Self {
        Self::default()
    }

    /// 预热：确保模型已下载并构造运行器（后台加载），不执行识别。
    pub fn warmup(&self) -> Result<(), OcrError> {
        ensure_runner().map(|_| ())
    }

    /// 识别：spawn_blocking 内完成推理（阻塞 IO，不占 async runtime）。
    async fn run(&self, image: &[u8]) -> Result<String, OcrError> {
        let img = image.to_vec();
        tokio::task::spawn_blocking(move || run_native(&img))
            .await
            .map_err(|e| OcrError::Rapid(format!("OCR 线程失败: {e}")))?
    }
}

/// 确保全局运行器已就绪（必要时下载模型并构造）。
fn ensure_runner() -> Result<(), OcrError> {
    let mut guard = pool()
        .lock()
        .map_err(|_| OcrError::Rapid("OCR 池锁中毒".into()))?;
    if guard.is_none() {
        *guard = Some(build_runner()?);
    }
    Ok(())
}

/// 构造原生运行器（下载模型 + 初始化 ONNX sessions）。
fn build_runner() -> Result<RapidOcrRunner, OcrError> {
    let dirs = verba_config::VerbaDirs::locate()
        .map_err(|e| OcrError::Rapid(format!("定位数据目录失败: {e}")))?;
    let user_model_dir = dirs.data_dir().join("models-rapidocr");
    // 模型目录查找：用户数据目录优先（可自更新）；否则用安装包自带
    // （daemon 同目录 models-rapidocr，安装器打包，免首次下载离线可用）。
    let model_dir = if user_model_dir.join("ch_PP-OCRv5_det_mobile.onnx").exists() {
        user_model_dir
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("models-rapidocr")))
            .filter(|d| d.join("ch_PP-OCRv5_det_mobile.onnx").exists())
            .unwrap_or(user_model_dir)
    };
    let cache = rapidocr_core::model::ModelCache::new(&model_dir);
    cache
        .ensure_model_set(
            &rapidocr_core::model::PPOCRV5_CH_MOBILE,
            rapidocr_core::model::ModelDownloadMode::Missing,
        )
        .map_err(|e| OcrError::Rapid(format!("RapidOCR 模型下载/校验失败: {e}")))?;
    let cfg = cache.config_for(&rapidocr_core::model::PPOCRV5_CH_MOBILE);
    let runner = rapidocr_core::RapidOcr::new(cfg)
        .map_err(|e| OcrError::Rapid(format!("RapidOCR 初始化失败: {e}")))?;
    log::info!(
        "RapidOCR 原生运行器就绪（模型目录: {}）",
        model_dir.display()
    );
    Ok(RapidOcrRunner { runner })
}

/// 统一解码为 RGB：此前把原始字节直接写成 .bmp 临时文件（截图是 PNG 内容），
/// rapidocr 按扩展名用 BMP 解码器 → failed to decode image（真机复现）。
/// 现按内容魔数识别（BMP/PNG/JPEG 等，取决于启用 feature）后转 RGB 走 run_image。
fn decode_rgb(image: &[u8]) -> Result<image::RgbImage, OcrError> {
    let img = image::load_from_memory(image)
        .map_err(|e| OcrError::Rapid(format!("图像解码失败: {e}")))?;
    Ok(img.to_rgb8())
}

/// 在常驻运行器上执行一次识别。
fn run_native(image: &[u8]) -> Result<String, OcrError> {
    let mut guard = pool()
        .lock()
        .map_err(|_| OcrError::Rapid("OCR 池锁中毒".into()))?;
    if guard.is_none() {
        *guard = Some(build_runner()?);
    }
    let runner = guard.as_mut().expect("set 后必有运行器");
    let img = decode_rgb(image)?;
    let result = runner.runner.run_image(&img);
    let output = result.map_err(|e| OcrError::Rapid(format!("RapidOCR 识别失败: {e}")))?;
    let mut lines = Vec::new();
    for line in output.lines {
        let text = line.text.trim();
        if !text.is_empty() {
            lines.push(text.to_owned());
        }
    }
    Ok(lines.join("\n"))
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
    fn new_is_default() {
        let o = RapidOcr::new();
        assert!(o._python.is_none());
    }

    #[test]
    fn with_python_ignored() {
        // 兼容旧 API：Python 已被移除，解释器参数不再起作用。
        let o = RapidOcr::with_python("C:\\py\\python.exe");
        assert!(o._python.is_none());
    }

    /// 回归：run_native 的解码契约——必须吃下 IME 截图的实际格式
    /// （capture.rs encode_bmp 产出的 32bpp top-down BMP 文件）与 PNG，
    /// 且对任意字节必须报「图像解码失败」而不是 panic。
    #[test]
    fn decode_rgb_accepts_ime_bmp_and_png() {
        // 与 frontends/windows/ime/src/capture.rs::encode_bmp 同构：BITMAPFILEHEADER(14)
        // + BITMAPINFOHEADER(40, biHeight 负值 top-down, 32bpp) + 2x2 BGRA 像素。
        let mut bmp: Vec<u8> = Vec::new();
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(14u32 + 40u32 + 16).to_le_bytes());
        bmp.extend_from_slice(&0u16.to_le_bytes());
        bmp.extend_from_slice(&0u16.to_le_bytes());
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&2i32.to_le_bytes()); // biWidth
        bmp.extend_from_slice(&(-2i32).to_le_bytes()); // biHeight（top-down）
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
        bmp.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
        bmp.extend_from_slice(&16u32.to_le_bytes()); // biSizeImage
        bmp.extend_from_slice(&0i32.to_le_bytes());
        bmp.extend_from_slice(&0i32.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&[0u8; 16]); // BGRA 像素

        let img = decode_rgb(&bmp).expect("IME 截图 BMP 应可解码");
        assert_eq!((img.width(), img.height()), (2, 2));
        assert_eq!(img.as_raw().len(), 2 * 2 * 3);

        // PNG（用 image 编码器生成，验证魔数识别路径）
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("PNG 编码");
        let img = decode_rgb(png.get_ref()).expect("PNG 应可解码");
        assert_eq!((img.width(), img.height()), (1, 1));

        // 任意字节必须优雅报错（此前写 .bmp 临时文件的路径对任意字节都能走通）
        let err = decode_rgb(b"not an image at all").unwrap_err();
        assert!(err.to_string().contains("图像解码失败"));
    }
}
