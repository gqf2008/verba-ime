# AGENTS.md

## 项目简介
Verba · 拾言输入法：开源跨平台多模态 AI 输入法（OCR / ASR / LLM / TTS），支持 Windows / macOS / Linux。
架构为「共享 Rust 核心 + 各平台薄前端 + 后台 daemon 进程」，详见 `docs/`。

## 目录约定
- `crates/` — Rust workspace：`verba-core`（引擎/状态机）、`verba-ai`（能力 provider）、`verba-protos`（IPC 协议）、`verba-ipc`（IPC 传输）、`verba-config`（配置/密钥）、`verba-daemon`（后台进程）、`verba-cli`（调试 CLI）
- `frontends/` — 各平台输入法前端：`windows/`（TSF）、`macos/`（IMK）、`linux/`（Fcitx5/IBus/Wayland）
- `apps/` — 设置面板等桌面应用（Slint 1.17）
- `docs/` — 架构、路线图、协议、服务商矩阵、构建、品牌文档
- `assets/` — 图标与品牌资源

## 通用规则
遵循 `~/.agents/rules/` 下的通用规则（开发流程、开发规范、提交规范、合并规范、经验沉淀等）；本文件为仓库级规则，冲突时以本文件为准。

## 跨平台默认
- 用户提出的所有功能默认在 **Windows / macOS / Linux 全平台**实现；未明确说明平台限定时，不得只在一个平台收口，也不得把单平台实现当作完成。
- 某平台前端尚未就绪（如 Linux 前端未开始）时，共享 core / daemon / verba-trigger 必须先落下平台中立实现；前端就绪后直接接线，禁止在共享层写死单平台或把缺口留到前端。
- 交付时必须显式列出各平台状态；任何平台缺口都要说明原因与补齐计划，不能静默略过或用「当前仅 X 接入」包装成已完成。

## 构建与验证
- 构建（Windows 须 MSVC target）：`scripts\build-msvc.cmd build --workspace --target x86_64-pc-windows-msvc`
- 测试：`scripts\build-msvc.cmd test --workspace --target x86_64-pc-windows-msvc`
- Lint：`cargo fmt --all -- --check` + `scripts\build-msvc.cmd clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings`
- 文档改动需保持 `docs/` 内交叉引用与 README 一致
- 平台前端改动需在对应平台验证（TSF / IMK / Fcitx5 无法在纯 CI 完整跑通，至少保证 `cargo check` 门禁）
- 关键决策（架构、协议、provider）先更新对应文档再写代码，契约不漂移

## 提交
按通用规则：单职责提交、禁止临时文件/密钥/构建产物、提交前自跑最小验证。