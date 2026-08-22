# Windows 前端（TSF）

- 技术：Rust + `windows` crate（TSF 绑定），参考 [khiin-rs `windows/ime`](https://github.com/aiongg/khiin-rs/tree/main/windows) 与 imekit 的 TSF 实现。
- 形态：COM 服务 DLL，注册为系统输入法。
- 关键点：
  - 实现 `ITfTextInputProcessorEx` 等接口；STA 线程模型；回调快速返回，重活交给 daemon。
  - `ITfInputProcessorProfiles::Register` 注册 GUID + 语言栏按钮；IMM32 仅兼容回退。
  - 候选窗口：TSF `ITfCandidateListUIElement` 或自绘置顶窗口（见架构开放问题）。
- 安装：Inno Setup 打包，注册 COM + 输入法。
- 状态：**未开始（M1）**。