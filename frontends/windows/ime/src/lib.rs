//! Verba Windows TSF 文本服务（输入法 DLL）。
//!
//! 结构（薄壳 + 共享核心 + daemon）：
//! - [`dll`]：COM 导出（DllGetClassObject / DllRegisterServer / ...）
//! - [`class_factory`]：IClassFactory，创建 TextService
//! - [`text_service`]：TSF 文本服务（按键/组合/候选 + 与 daemon 的 LLM 流式）
//! - [`reg`]：注册表与 TSF 类别注册
//! - [`ipc`]：daemon 连接管理
//!
//! 本 crate 独立于根 workspace（仅 Windows 目标）。

mod candidate_window;
mod class_factory;
mod dll;
mod edit_session;
mod filelog;
mod guids;
mod ipc;
pub mod reg;
pub mod text_service;

pub use dll::{DllGetClassObject, DllRegisterServer, DllUnregisterServer};
pub use guids::{CLSID_VERBA_TEXT_SERVICE, PROFILE_VERBA};
