// Verba macOS IMK 薄壳：承载 IMK 输入会话，经 dlopen 调用 Rust 引擎（libverba_ime_macos.dylib）。
// 注意：本文件仅作为薄壳模板；真正注册为系统输入法（TISInputSource）需打包为 .app + Info.plist，
//      并在 macOS 真机验收。CI（macos-latest）可 `swift build` 验证编译。

import Cocoa
import InputMethodKit
import Darwin

/// dlopen Rust 引擎并调用 C ABI。
enum VerbaEngine {
    static let libName = "libverba_ime_macos.dylib"

    static func connect() -> Bool {
        guard let handle = dlopen(libName, RTLD_NOW) else {
            NSLog("[VerbaIMK] dlopen %@ 失败", libName)
            return false
        }
        guard let sym = dlsym(handle, "verba_mac_connect") else { return false }
        typealias Fn = @convention(c) () -> Int32
        let f = unsafeBitCast(sym, to: Fn.self)
        return f() == 1
    }

    static func ping() -> Bool {
        guard let handle = dlopen(libName, RTLD_NOW) else { return false }
        guard let sym = dlsym(handle, "verba_mac_ping") else { return false }
        typealias Fn = @convention(c) () -> Int32
        let f = unsafeBitCast(sym, to: Fn.self)
        return f() == 1
    }
}

/// IMK 输入控制器（薄壳）。按键/文本交给 Verba 核心处理，这里先占位。
final class VerbaIMKController: IMKInputController {
    private var engineOK = false

    override func activateServer(_ sender: Any!) {
        super.activateServer(sender)
        engineOK = VerbaEngine.connect()
        NSLog("[VerbaIMK] 激活，Rust 引擎连接: %@", engineOK ? "OK" : "FAIL")
    }

    override func inputText(_ string: String!, client sender: Any!) -> Bool {
        guard engineOK else { return false }
        NSLog("[VerbaIMK] inputText: %@", string)
        // TODO(ci/macos): 把按键交给 Rust 引擎（verba_core 状态机）→ 取候选/上屏。
        return true
    }

    override func didCommand(by aSelector: Selector!, client sender: Any!) -> Bool {
        return false
    }
}
