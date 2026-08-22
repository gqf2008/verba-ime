# 设置面板（apps/settings）

- 技术：Tauri 2（Rust 后端 + Web 前端），跨平台统一 UI（参考 khiin-rs 的做法）。
- 职责：
  - 服务商配置（LLM base_url / key / model，OCR / ASR / TTS 偏好）
  - 快捷键绑定、模式行为、隐私开关
  - 模型下载管理（whisper.cpp / PaddleOCR 首次下载与进度）
  - 日志与诊断导出
- 通过 `verba-ipc` 与 daemon 通信，配置热生效。
- 状态：**未开始（M4）**。