# Verba · 拾言输入法

> **声、像、思、音，皆成文字。**
> 一款开源跨平台「多模态 AI 输入法」：把 **OCR（图片/截图转文字）、ASR（语音转文字）、LLM（远程大模型）、TTS（文字转语音）** 融为一体，支持 **Windows / macOS / Linux**。

> 项目状态：**M1/M5 已实机验收**（Windows TSF + LLM 直输 + 中文引擎：Rime/候选窗/融合）；
> 当前推进 **M3 多模态 + M4 TTS Rust 核心**（TTS mock/edge-tts + OCR mock/Windows.Media.Ocr/rapid（原生 Rust ONNX/RapidOCR，无 Python） + ASR mock/openai 已端到端通；「眼睛」——`//` 指令自动捕捉光标上方屏幕，OCR 或多模态 vision 喂给 LLM；AI 模式支持多轮上下文（`//重置` 清空）。`verba-cli diag` 一键诊断（健康/配置/日志尾/进程）。Piper/whisper.cpp 跟进）。
> 见 [Windows 手动验收清单](docs/manual-acceptance-windows.md) 与 [路线图](docs/roadmap.md)。

---

## 为什么做 Verba

传统输入法只处理「键盘 → 文字」这一条通道，而我们的表达方式早已不止于键盘：

- 看到一段文字、一张截图，想直接变成可编辑文本 → **OCR**
- 想说一段话，不想打字 → **ASR**
- 想润色、翻译、续写、总结，或直接"问一句" → **LLM（远程）**
- 想把文字读出来，边写边听 → **TTS**

Verba 的目标是成为三个平台上的「统一输入入口」：**任何表达方式，最终都变成文字，落到光标处。**

## 核心特性

| 能力 | 说明 |
| --- | --- |
| 🖼️ OCR | 截图选区域 / 剪贴板图片 / 图片文件 → 文字上屏；`//看图` / 眼睛 vision 直接把屏幕区域交给多模态 LLM 理解与提取 |
| 🎙️ ASR | 全局快捷键唤起语音输入 → 实时转写 → 上屏 |
| 🤖 LLM（远程） | 输入法内 AI 模式：翻译、润色、续写、总结、自定义 Prompt，流式输出；多轮上下文（`ai_context_turns`，`//重置` 清空） |
| 🔊 TTS | 朗读选中文本 / 候选词，可配置上屏自动朗读 |
| ⌨️ 输入 | 英文直输 + 标点 + 快捷指令；快捷短语（`//短语 <名称>` 一键插入用户模板，`verba-cli phrase set/list/del` 管理）；中文拼音引擎（可选集成 librime） |
| ⚙️ 配置 | 统一设置面板（Slint 跨平台），AI 服务商可插拔，密钥系统密钥库安全存储 |
| 🔒 隐私 | 本地能力默认离线；远程 LLM 显式开关并提示数据出境 |

## 支持平台

| 平台 | 输入法框架 | 状态 |
| --- | --- | --- |
| Windows 10/11 | Text Services Framework (TSF) | 规划中 |
| macOS 12+ | Input Method Kit (IMK) | 规划中 |
| Linux | Fcitx5 / IBus / Wayland (zwp_input_method_v2) / X11 (XIM) | 规划中 |

## 架构一览

共享 Rust 核心 + 各平台薄前端 + 后台守护进程（daemon）：

```
┌──────────────┐   ┌──────────────┐   ┌──────────────┐
│ Windows TSF  │   │  macOS IMK   │   │  Linux       │
│ 前端 (Rust)  │   │ 前端 (Swift) │   │ Fcitx5/IBus  │
└──────┬───────┘   └──────┬───────┘   └──────┬───────┘
       │     IPC（Protobuf / 本地套接字）      │
       └──────────────────┬──────────────────┘
                  ┌────────▼────────┐
                  │  Verba Daemon   │  ← 核心引擎（Rust/tokio）
                  │ OCR·ASR·LLM·TTS │
                  └────────────────┘
```

详见 [docs/architecture.md](docs/architecture.md)。

## 文档

- [架构设计](docs/architecture.md)
- [路线图](docs/roadmap.md)
- [AI 服务商矩阵（OCR/ASR/LLM/TTS）](docs/providers.md)
- [IPC 协议草案](docs/protocol.md)
- [构建与打包](docs/building.md)
- [命名与品牌](docs/naming.md)

## 快速开始（当前骨架）

```bash
git clone https://github.com/gqf2008/verba-ime.git
cd verba-ime
cargo build --workspace          # 构建核心与 CLI
cargo run -p verba-cli -- --help
```

> 目前仅有核心骨架与 CLI 调试入口；平台输入法前端从 M1（Windows）开始落地，见 [路线图](docs/roadmap.md)。

## 路线图摘要

- **M0 地基**：仓库骨架、CI、核心引擎与 CLI（进行中）
- **M1 Windows 垂直切片**：TSF 前端 + LLM 远程直输
- **M2 三端齐平**：macOS IMK、Linux Fcitx5/IBus
- **M3 多模态**：OCR（截图）与 ASR（语音）接入
- **M4 打磨发布**：TTS、设置面板、候选窗口、打包签名、Alpha/Beta
- **M5 中文引擎**：librime 集成 + 候选窗/分页/主题/融合 ✅

## 参考与同类项目

- [imekit](https://github.com/SergioRibera/imekit) — Rust 跨平台 IME 协议库（TSF/IMK/Fcitx/IBus/Wayland）
- [khiin-rs](https://github.com/aiongg/khiin-rs) — Rust 跨平台输入法工程范式（TSF/IMK/Android + Protobuf IPC）
- [fcitx5-afrim](https://github.com/fodydev/fcitx5-afrim) — Rust 编写原生 Fcitx5 插件的范式
- [Rime / librime](https://github.com/rime/librime) — 中文输入引擎（路线图集成对象）
- 素言 SuYan 输入法 — 同类产品（Windows/macOS、RIME 系、离线语音 + 截图），作为竞品与体验参考

## 许可证

MIT License（暂定，正式发布前可调整）。