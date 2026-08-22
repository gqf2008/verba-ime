# macOS 前端（IMK）

- 技术：Swift / ObjC 薄壳（方案 A，推荐）或 Rust `objc2-input-method-kit`（方案 B，评估中）。
- 形态：`.appex`，装入 `~/Library/Input Methods`，在系统设置「键盘 → 输入法」中启用。
- 关键点：
  - 实现 `IMKInputController` 子类；Info.plist 声明输入法组件。
  - 基础输入**无需**辅助功能权限。
  - 麦克风：需 `NSMicrophoneUsageDescription`（TCC 弹窗）；截图 OCR：需屏幕录制权限（ScreenCaptureKit）。
- 打包：`.app` 内含 `.appex`，Developer ID 签名 + 公证。
- 状态：**未开始（M2）**。