//! Windows.Media.Ocr 本地识别（零下载；Windows 10+ 内置 OCR，需安装对应语言 OCR 包）。

#![cfg(windows)]

use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use verba_ai::OcrProvider;

use crate::OcrError;

/// Windows.Media.Ocr 识别器（无状态；每次调用在线程上初始化 MTA COM）。
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsMediaOcr;

impl WindowsMediaOcr {
    pub fn new() -> Self {
        Self
    }
}

/// 当前线程初始化 COM 多线程公寓（重复调用返回 S_FALSE，可安全忽略）。
fn init_com() {
    // SAFETY: CoInitializeEx 仅设置线程公寓状态，无所有权转移；
    // MTA 重复初始化返回 S_FALSE，无需 CoUninitialize 配对。
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn win_err(e: windows::core::Error) -> OcrError {
    OcrError::Windows(e.to_string())
}

/// 在已初始化 COM 的当前线程上同步执行 WinRT OCR（阻塞线程池调用）。
fn recognize_sync(image: &[u8]) -> Result<String, OcrError> {
    init_com();
    futures_executor::block_on(async {
        // 1) 图像字节 → 内存流
        let stream = InMemoryRandomAccessStream::new().map_err(win_err)?;
        {
            let writer = DataWriter::CreateDataWriter(&stream).map_err(win_err)?;
            writer.WriteBytes(image).map_err(win_err)?;
            writer
                .StoreAsync()
                .map_err(win_err)?
                .await
                .map_err(win_err)?;
            // DetachStream 防止 writer 释放时关闭底层流。
            writer.DetachStream().map_err(win_err)?;
        }
        stream.Seek(0).map_err(win_err)?;

        // 2) 解码为 SoftwareBitmap
        let decoder = BitmapDecoder::CreateAsync(&stream)
            .map_err(win_err)?
            .await
            .map_err(win_err)?;
        let bitmap = decoder
            .GetSoftwareBitmapAsync()
            .map_err(win_err)?
            .await
            .map_err(win_err)?;

        // 3) OCR 引擎：简体中文优先，回退用户配置文件语言
        let zh = HSTRING::from("zh-Hans-CN");
        let engine = Language::CreateLanguage(&zh)
            .ok()
            .and_then(|lang| OcrEngine::TryCreateFromLanguage(&lang).ok())
            .or_else(|| OcrEngine::TryCreateFromUserProfileLanguages().ok())
            .ok_or_else(|| {
                OcrError::Windows("无可用 OCR 引擎（需 Windows 10+ 且已安装 OCR 语言包）".into())
            })?;

        // 4) 识别并拼接各行
        let result = engine
            .RecognizeAsync(&bitmap)
            .map_err(win_err)?
            .await
            .map_err(win_err)?;
        let mut text = String::new();
        let lines = result.Lines().map_err(win_err)?;
        for i in 0..lines.Size().map_err(win_err)? {
            let line = lines.GetAt(i).map_err(win_err)?;
            let t = line.Text().map_err(win_err)?.to_string();
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&t);
        }
        Ok(text)
    })
}

impl OcrProvider for WindowsMediaOcr {
    type Error = OcrError;

    async fn recognize(&self, image: &[u8]) -> Result<String, OcrError> {
        if image.is_empty() {
            return Err(OcrError::EmptyImage);
        }
        let image = image.to_vec();
        // WinRT 阻塞调用放到阻塞线程池（单线程 COM 公寓），避免卡住 daemon 事件循环。
        tokio::task::spawn_blocking(move || recognize_sync(&image))
            .await
            .map_err(|e| OcrError::Windows(format!("OCR 线程失败: {e}")))?
    }
}
