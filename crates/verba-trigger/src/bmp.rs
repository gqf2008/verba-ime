//! BMP 编码：32bpp top-down BMP 字节（daemon OCR 的原生输入格式）。
//!
//! BMP 为 WIC/CGImage 原生可解码格式；编码零依赖（手写文件头），
//! 避免引入 image/png 大依赖。纯 Rust，跨平台同源。

/// 一次截图的产物：尺寸 + BMP 字节（32bpp，top-down，无行填充）。
#[derive(Debug, Clone)]
pub struct ScreenShot {
    pub width: i32,
    pub height: i32,
    pub bmp: Vec<u8>,
}

/// 32bpp top-down BMP 编码（BITMAPFILEHEADER + BITMAPINFOHEADER + 像素）。
pub fn encode_bmp(width: i32, height: i32, bgra: &[u8]) -> Vec<u8> {
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
