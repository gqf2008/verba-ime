//! 屏幕截图：全屏 BitBlt → 32bpp top-down BMP 字节。
//!
//! BMP 为 WIC 原生可解码格式（BitmapDecoder），daemon 的 Windows.Media.Ocr 可直接识别；
//! 编码零依赖（手写文件头），避免引入 image/png 大依赖。

use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    GetDeviceCaps, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HGDIOBJ, HORZRES, SRCCOPY, VERTRES,
};

use crate::TriggerError;

/// 一次截图的产物：尺寸 + BMP 字节（32bpp，top-down，无行填充）。
#[derive(Debug, Clone)]
pub struct ScreenShot {
    pub width: i32,
    pub height: i32,
    pub bmp: Vec<u8>,
}

/// 截取主屏全屏。
pub fn capture_primary_screen() -> Result<ScreenShot, TriggerError> {
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err(TriggerError::Capture("GetDC 失败".into()));
        }
        let width = GetDeviceCaps(Some(screen), HORZRES);
        let height = GetDeviceCaps(Some(screen), VERTRES);
        if width <= 0 || height <= 0 {
            let _ = ReleaseDC(None, screen);
            return Err(TriggerError::Capture(format!(
                "屏幕尺寸非法: {width}x{height}"
            )));
        }

        let mem = CreateCompatibleDC(Some(screen));
        if mem.is_invalid() {
            let _ = ReleaseDC(None, screen);
            return Err(TriggerError::Capture("CreateCompatibleDC 失败".into()));
        }
        let bmp = CreateCompatibleBitmap(screen, width, height);
        if bmp.is_invalid() {
            let _ = DeleteDC(mem);
            let _ = ReleaseDC(None, screen);
            return Err(TriggerError::Capture("CreateCompatibleBitmap 失败".into()));
        }

        let old: HGDIOBJ = SelectObject(mem, HGDIOBJ(bmp.0));
        BitBlt(mem, 0, 0, width, height, Some(screen), 0, 0, SRCCOPY)
            .map_err(|e| TriggerError::Capture(format!("BitBlt 失败: {e}")))?;

        let row_bytes = (width as usize) * 4;
        let mut pixels = vec![0u8; row_bytes * (height as usize)];
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = GetDIBits(
            mem,
            bmp,
            0,
            height as u32,
            Some(pixels.as_mut_ptr() as *mut core::ffi::c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        // 恢复并释放 GDI 对象。
        let _ = SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        let _ = ReleaseDC(None, screen);

        if lines != height {
            return Err(TriggerError::Capture(format!(
                "GetDIBits 失败: 返回 {lines} 行"
            )));
        }
        Ok(ScreenShot {
            width,
            height,
            bmp: encode_bmp(width, height, &pixels),
        })
    }
}

/// 32bpp top-down BMP 编码（BITMAPFILEHEADER + BITMAPINFOHEADER + 像素）。
fn encode_bmp(width: i32, height: i32, bgra: &[u8]) -> Vec<u8> {
    let data_len = bgra.len() as u32;
    let file_size = 14u32 + 40u32 + data_len;
    let mut out = Vec::with_capacity(file_size as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&(-height).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(bgra);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmp_header_is_valid() {
        let bmp = encode_bmp(2, 2, &[0u8; 16]);
        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(bmp.len(), 14 + 40 + 16);
        // 文件头 offBits = 54
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 54);
        // 信息头 biWidth / biHeight(top-down 负值)
        assert_eq!(i32::from_le_bytes(bmp[18..22].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(bmp[22..26].try_into().unwrap()), -2);
        assert_eq!(u16::from_le_bytes(bmp[28..30].try_into().unwrap()), 32);
    }

    #[test]
    fn bmp_data_roundtrip() {
        let data = vec![0x11u8, 0x22, 0x33, 0x44];
        let bmp = encode_bmp(1, 1, &data);
        assert_eq!(&bmp[54..], &data[..]);
    }
}
