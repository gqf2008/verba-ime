# 架构设计

> 版本：v0.1（规划稿） · 更新：2026-08-22 · 关联：[路线图](roadmap.md)、[IPC 协议](protocol.md)、[服务商矩阵](providers.md)

## 1. 目标与非目标

### 目标
- 一套核心代码驱动 Windows / macOS / Linux 三个平台的输入法。
- 四大能力：OCR、ASR、远程 LLM、TTS，全部可插拔（本地 / 系统 / 云端 provider）。
- 低延迟：语音流式首字 < 1.5s（本地模型）、LLM 首 token < 2s（取决于服务商）、直输 < 20ms。
- 不阻塞 UI：AI 计算全部在 daemon 中异步执行，输入法前端只做薄壳。
- 隐私可控：默认本地优先，远程调用显式授权。

### 非目标（v1）
- 中文拼音引擎：**Rime（librime）单引擎**（daemon 内；本地词库 + 五笔/注音生态）。此前内置 `verba-pinyin` 已移除。
- 不做移动端（Android / iOS）与 Web 前端。
- 不内置模型训练 / 微调。

## 2. 总体架构

三层：

1. **平台前端（frontends）**：各平台输入法壳，负责注册、按键捕获、preedit / 候选展示、上屏。
2. **核心引擎（daemon）**：输入状态机 + AI 能力编排 + 配置 + 任务队列，独立进程运行。
3. **设置与辅助（apps / cli）**：Slint 设置面板（apps/settings）、verba-cli 调试工具。

```
┌─────────────┐ ┌─────────────┐ ┌──────────────────────┐
│ Windows TSF │ │  macOS IMK  │ │ Linux (Fcitx5 / IBus  │
│  (Rust)     │ │   (Rust)    │ │  / Wayland / XIM)     │
└──────┬──────┘ └──────┬──────┘ └──────────┬───────────┘
       │               │                   │
       └───────────────┼───────────────────┘
                       │ IPC: NamedPipe / UnixSocket（可选 D-Bus）
               ┌───────▼────────┐   ┌───────────────┐
               │  verba-daemon   │──▶│  verba-ai     │
               │ (Rust / tokio)  │   │ OCR·ASR·LLM·TTS│
               │ 状态机·任务队列  │   │ provider 插件 │
               └───────┬────────┘   └───────┬───────┘
                       │                    │
               ┌───────▼────────┐   ┌───────▼───────┐
               │ verba-config   │   │ 远程服务 / 本地模型│
               │ 设置·密钥库    │   │ (OpenAI 兼容 ·  │
               └────────────────┘   │  whisper.cpp …)│
                                    └───────────────┘
```

### 为什么用独立 daemon 进程
- OCR / ASR / LLM 是重量级、长耗时任务；TSF / IMK 的回调线程要求快速返回，放同进程会卡输入法甚至被系统判定无响应。
- daemon 崩溃不影响系统输入法注册，可自动拉起。
- 多个输入法实例（多显示器 / 多会话）共享一个引擎与模型加载，省内存。
- 代价：多一层 IPC，由 `verba-ipc` 屏蔽；daemon 需做生命周期管理（跟随登录启动、单实例、健康检查）。

### 进程模型
- 每个平台一个前端（Windows TSF 为 COM 服务 DLL；macOS IMK 为 `.appex`；Linux 为 fcitx5 / ibus 插件进程内）。
- daemon 单实例（启动时探测锁文件 / 命名互斥），无前端连接时休眠以省资源。
- 设置面板按需启动，通过 IPC 读写 daemon 配置。

## 3. 模块划分（Rust workspace）

| crate | 职责 |
| --- | --- |
| `verba-core` | 输入状态机（Normal / Voice / Ocr / Ai）、拼音组合 + 候选选择、AI 提示词、命令路由 |
| `verba-librime` | Rime（librime）中文引擎 FFI 封装（单引擎，候选来源） |
| `verba-ai` | AI provider 抽象（trait）与实现：ocr / asr / llm / tts 四类，含本地 / 系统 / 云端实现 |
| `verba-protos` | IPC Protobuf 定义（prost 生成） |
| `verba-ipc` | IPC client / server：Windows NamedPipe、Unix Socket、Linux D-Bus（可选） |
| `verba-config` | 配置读写（TOML）、默认值、密钥库（keyring） |
| `verba-daemon` | 后台进程：启动核心、任务队列、事件分发、健康检查、自动重启 |
| `verba-cli` | 调试 CLI：直接驱动 core，模拟按键 / 命令，预览候选 |
| `frontends/*` | 平台前端（独立于 workspace，各自构建） |

## 4. 平台前端

### Windows — TSF（Text Services Framework）
- 用 `windows` crate 实现 `ITfTextInputProcessorEx` 等 TSF 接口（参考 khiin-rs `windows/ime` 与 imekit 的 TSF 实现）。
- 注册：`ITfInputProcessorProfiles::Register` 注册 GUID + 语言栏按钮；IMM32 仅作兼容回退。
- 候选窗口：TSF `ITfCandidateListUIElement` 或自绘置顶窗口（开放问题 2）。
- 安装：Inno Setup / WiX，注册 COM + 输入法。
- 注意：TSF 要求 STA；所有回调尽快返回，重活交给 daemon。

### macOS — IMK（Input Method Kit）
- `IMKInputController` 子类，`.app`（单进程托管全部控制器）装入 `~/Library/Input Methods`，用户需在系统设置中启用。
- 实现：纯 Rust `objc2-input-method-kit`（`frontends/macos/ime`），经 Unix Socket 连 daemon。
- 权限：基础输入无需辅助功能权限；麦克风需 `NSMicrophoneUsageDescription`（TCC 弹窗）；截图 OCR 需屏幕录制权限（ScreenCaptureKit）。
- 打包：`.app`，Developer ID 签名 + 公证（发布必需）。

### Linux
- 首选 **Fcitx5 原生插件**：C++ shim + Rust 静态库（fcitx5-afrim / corrosion 范式），覆盖 KDE 与多数中文发行版。
- 兼容路线（按环境自动选择）：
  - **IBus 引擎**（D-Bus，imekit `ibus` feature / zbus）——GNOME 默认环境。
  - **Wayland `zwp_input_method_v2`**（imekit）——sway / Hyprland / KDE。
  - **X11 XIM**——legacy 回退。
- 打包：.deb / .rpm / AppImage。

## 5. IPC 协议（草案）

- 传输：Windows NamedPipe（interprocess local_socket，名称 `verba-ime-{USERNAME}-{token}`）；macOS / Linux Unix Socket（用户数据目录，0700）。详见 [协议](protocol.md) §1。
- 编码：Protobuf（`verba-protos`），u32 LE 长度前缀分帧。
- 模型：`Request { id, oneof }` / `Response { id, oneof }`，`StreamEvent { id, chunk }` 支持流式（LLM token、ASR 增量）。
- 消息清单草案见 [protocol.md](protocol.md)。

### Windows 命名管道实测约束（2026-08-22，interprocess 2.4）
- `set_nonblocking`（PIPE_NOWAIT）会把「无数据」与「对端关闭」混淆 → 客户端禁用。
- `try_clone` 出的第二句柄读分帧数据会出现假 EOF → 客户端禁用。
- 结论：**客户端单句柄、单线程顺序读写**；需要并行读时（LLM 流式）另起线程持独立连接，daemon 按全局请求 id 取消。
- 客户端超时用「后台读线程 + std mpsc `recv_timeout`」实现（早期版本），后简化为「服务端协议保证必有响应/终帧（Final/Error，取消也补发）→ 阻塞读安全」。

## 6. AI 能力编排（verba-ai）

### 统一 provider trait
```rust
pub trait OcrProvider { async fn recognize(&self, img: &ImageRef) -> Result<String>; }
pub trait AsrProvider { /* start / stop / stream */ }
pub trait LlmProvider { fn stream(&self, req: LlmRequest) -> BoxStream<Result<String>>; }
pub trait TtsProvider { async fn speak(&self, text: &str) -> Result<()>; }
```
- 每个能力可配置多个 provider（本地 / 系统 / 云端），运行时按配置选择，失败可降级。
- 默认矩阵与选型依据见 [providers.md](providers.md)。

### 数据流示例

**语音输入（ASR）**
1. 用户按全局快捷键 → 前端（或 daemon 的系统级监听）开始录音。
2. 音频分片 → ASR provider（本地 whisper.cpp 流式或云端）→ 增量文本回传 daemon。
3. daemon 合并结果 → IPC 通知前端显示候选 / 直接上屏。

**截图 OCR**
1. 快捷键 → 前端截屏（macOS 需录屏权限；Windows Graphics Capture / GDI）。
2. 图片 → daemon → OCR provider（本地 PaddleOCR 优先）→ 文本。
3. 前端上屏，可先出候选再确认。

**LLM 流式**
1. AI 模式触发（`//` 前缀或快捷键）→ 前端收集 prompt → daemon → LLM provider。
2. SSE 流 → `StreamEvent` 增量 → 前端 preedit 实时刷新 → Enter 上屏 / Esc 取消。

## 7. 配置与密钥

- 配置文件：`%APPDATA%/Verba/config.toml`、`~/Library/Application Support/Verba/config.toml`、`~/.config/verba/config.toml`。
- API Key 不入配置文件，走系统密钥库（`keyring` crate：Windows DPAPI / macOS Keychain / Linux Secret Service）。
- 设置面板（apps/settings，Slint 1.17）读写同一配置，热生效；API Key 经 IPC `ApiKeySet` 写系统密钥库并热更新。

## 8. 性能预算（目标）

| 路径 | 预算 |
| --- | --- |
| 按键到上屏（直输 / 标点） | < 20ms |
| 语音流式首字 | < 1.5s（本地 whisper.cpp base） |
| 截图 OCR 完成 | < 2s（本地 PaddleOCR） |
| LLM 首 token | < 2s（取决于服务商 / 网络） |
| TTS 首字出声 | < 300ms |
| daemon 空闲内存 | < 80MB（不含模型） |

## 9. 安全与隐私

- 远程 LLM：每次调用显式提示「数据将发送至 <服务商>」；支持完全关闭远程能力。
- 录音 / 录屏：走系统权限 API，最小权限申请；录音仅内存处理，默认不落盘。
- 日志脱敏：默认不记录用户文本与密钥；调试模式才记录文本且仅本地存储。
- 更新：签名分发，校验发布包哈希。

## 10. 测试策略

- core：单元测试（状态机、候选、命令路由）+ 集成测试（模拟前端事件流）。
- verba-ai：provider 以 trait 注入 mock；本地模型跑 golden 样本（截图 / 音频 fixture）。
- IPC：client / server 回环测试；断线重连、背压、大包（图像）测试。
- 前端：每平台最小冒烟（注册、上屏、preedit）+ CI 编译门禁；真机验证用手动清单。
- 遵循通用规则：新功能带测试，lint 落进项目配置（`cargo fmt` / `clippy -D warnings`）。

## 11. 参考项目

| 项目 | 借鉴点 |
| --- | --- |
| [imekit](https://github.com/SergioRibera/imekit) | Rust 跨平台 IME 协议库；Linux Wayland / IBus / XIM 后端 |
| [khiin-rs](https://github.com/aiongg/khiin-rs) | Rust TSF 工程范式、Protobuf IPC |
| [fcitx5-afrim](https://github.com/fodydev/fcitx5-afrim) | Rust 编写原生 Fcitx5 插件（corrosion + C++ shim） |
| [Rime / librime](https://github.com/rime/librime) | 中文输入引擎，M5 集成对象 |
| 素言 SuYan 输入法 | 同类产品（RIME 系、离线语音、截图），体验参考 |

## 12. 开放问题（实现中决策）

> M1 已解决：Windows 前端采用 windows 0.62 + `#[implement]`；因 `ITfThreadMgr` 未导出 `AdviseSink`，
> 不挂 ThreadMgrEventSink，改为 Activate 时直接 `AdviseKeyEventSink`、`OnKeyDown` 带回 context。
> TSF 档案/类别注册需管理员（安装程序/verba-reg 提权路径）。
## 12. 开放问题（实现中决策）

1. 中文拼音引擎：**单引擎 = Rime（librime）**（daemon 内，启动预热）；此前内置 `verba-pinyin` 已移除。
2. TSF 候选窗口：原生 `ITfCandidateListUIElement` vs 自绘 overlay。
3. macOS appex 常驻限制下的 daemon 启动 / 生命周期策略。
4. 截图实现：Windows Graphics Capture vs GDI；macOS ScreenCaptureKit vs CGWindowList。
5. imekit 是否作为 Linux / Wayland 基座（评估后决定依赖 or fork，Apache/MIT 双许可）。
6. LLM 多轮上下文与隐私边界的默认策略（默认单轮，可配置）。