# 平台前端（frontends）

每个平台一个输入法前端，原则：
- **薄壳**：只做系统 IME 协议接入（注册、按键、preedit、候选、上屏），不承载 AI 逻辑。
- 所有重量级任务通过 IPC 交给 `verba-daemon`。

| 目录 | 平台 | 框架 | 技术 |
| --- | --- | --- | --- |
| `windows/` | Windows 10/11 | TSF | Rust（`windows` crate） |
| `macos/` | macOS 12+ | IMK | Swift 薄壳 + Rust 核心 |
| `linux/` | Linux | Fcitx5 / IBus / Wayland / XIM | C++ shim + Rust（corrosion）/ imekit |

各端说明见子目录 README。**各端通用验收标准**：
1. 安装后系统设置中可见并启用
2. 英文 + 标点直输上屏
3. preedit（未上屏状态）正确显示与编辑
4. 模式切换（AI / 语音 / OCR）与 daemon 通信正常
5. 权限弹窗（麦克风 / 录屏）按需正确触发