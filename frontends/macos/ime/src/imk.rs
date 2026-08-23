//! macOS IMK 输入控制器（全 Rust：objc2 + objc2-input-method-kit）。
//!
//! `IMKInputController` 是 Apple 的 ObjC 类；Rust 通过 `define_class!` 子类化它，
//! 并把真实逻辑交给 `verba_ime_macos::MacIme` / daemon。CI（macos-latest）编译验证；
//! 运行时注册为系统输入法需 macOS 真机。

#![cfg(target_os = "macos")]

use objc2::runtime::AnyObject;
use objc2::{define_class, MainThreadOnly};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};
use objc2_input_method_kit::{IMKInputController, IMKStateSetting};

#[derive(Debug, Default)]
struct Ivars {
    engine_ok: bool,
}

define_class!(
    // SAFETY: IMKInputController 的子类化无需额外约束。
    #[unsafe(super = IMKInputController)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct VerbaIMKController;

    // SAFETY: NSObjectProtocol 无安全要求。
    unsafe impl NSObjectProtocol for VerbaIMKController {}

    // SAFETY: IMKStateSetting 协议无安全要求。
    unsafe impl IMKStateSetting for VerbaIMKController {
        #[unsafe(method(activateServer:))]
        fn activate_server(&self, _sender: Option<&AnyObject>) {
            self.ivars().engine_ok = crate::MacIme::connect().is_ok();
            log::info!("[VerbaIMK] activateServer, engine={}", self.ivars().engine_ok);
        }

        #[unsafe(method(deactivateServer:))]
        fn deactivate_server(&self, _sender: Option<&AnyObject>) {
            log::info!("[VerbaIMK] deactivateServer");
        }
    }

    // SAFETY: 覆盖父类（NSObjectIMKServerInput 类别）的 inputText:client:。
    impl VerbaIMKController {
        #[unsafe(method(inputText:client:))]
        fn input_text(&self, string: Option<&NSString>, _sender: Option<&AnyObject>) -> bool {
            if let Some(s) = string {
                log::info!("[VerbaIMK] inputText: {}", s.to_string());
            }
            // TODO(ci/macos): 把按键交给 verba-core 状态机 → 取候选/上屏。
            true
        }
    }
);

/// 确保 IMKInputController 子类已在链接时注册（触发 define_class! 生成的类）。
pub fn register() {
    let _ = std::any::type_name::<VerbaIMKController>();
}
