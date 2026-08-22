//! Verba TSF 相关 GUID。

use windows::core::GUID;

/// TextService CLSID：{7C2D4E6A-1F3B-4A9E-8C5D-2F6B9A0E3D51}
pub const CLSID_VERBA_TEXT_SERVICE: GUID = GUID::from_u128(0x7c2d4e6a_1f3b_4a9e_8c5d_2f6b9a0e3d51);

/// 语言档案 GUID：{A4B8C1D2-3E4F-5A6B-7C8D-9E0F1A2B3C4D}
pub const PROFILE_VERBA: GUID = GUID::from_u128(0xa4b8c1d2_3e4f_5a6b_7c8d_9e0f1a2b3c4d);

/// 显示名称。
pub const TEXT_SERVICE_NAME: &str = "Verba · 拾言输入法";

