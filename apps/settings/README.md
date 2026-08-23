# 设置面板（apps/settings）

- 技术：**Slint 1.17**（Rust 原生跨平台桌面 UI，替代原 Tauri 方案；crates.io 无 0.17 版本线，`0.17` 即 `1.17.x`，固定 `=1.17.1`）。
- 职责：
  - LLM 服务商（base_url / model / temperature / max_tokens / 系统提示词）与 **API Key**（经 IPC `ApiKeySet` 写系统密钥库并热更新 daemon，无需重启）
  - 多模态：OCR / ASR / TTS provider 选择 + 在线端点与模型（ASR/TTS 走「联网」：OpenAI 兼容 `audio/transcriptions` / `audio/speech`；edge-tts 在线音色）
  - 中文引擎（builtin / rime + 方案）与候选窗主题
  - 快捷键速览（当前内置：Ctrl+Alt+O 选区 OCR / Ctrl+Alt+M 录音 ASR / `//朗读` `//截图` `//听写`）
  - 隐私说明（远程数据出境提示、密钥存储位置）
- 通过 `verba-ipc` 与 daemon 通信（GetConfig / SetConfig / ApiKeySet），保存即热生效。

## 构建与运行

```bash
cargo run -p verba-settings      # 需先启动 daemon：verba-cli daemon
```

## 密钥管理

- 面板内「保存密钥」/「清除密钥」经 IPC `ApiKeySet` 写系统密钥库（keyring 平台后端：
  Windows Credential Manager / macOS Keychain / Linux Secret Service）并热更新 daemon 内存。
- CLI 等价命令：`verba-cli key`（查看）、`verba-cli key <值>`（设置）、`verba-cli key clear`（清除）。

## 说明

- `verba-settings` 不继承工作区 `unsafe_code = forbid`：Slint 生成代码带 `#[allow(unsafe_code)]`，
  本地降为 `deny`（手写 unsafe 仍会被拒）。
- 阻塞 IPC 全部在后台线程执行，UI 更新经 `slint::invoke_from_event_loop` 回事件循环线程。
