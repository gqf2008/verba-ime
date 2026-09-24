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
| `verba-candidate` | 候选窗共享逻辑与 CPU 渲染器（tiny-skia：分页 / 主题；`render_png` example 无窗口渲染验证；光标避让在 Windows 前端实现） |
| `frontends/*` | 平台前端（独立于 workspace，各自构建） |

## 4. 平台前端

### Windows — TSF（Text Services Framework）
- 用 `windows` crate 实现 `ITfTextInputProcessorEx` 等 TSF 接口（参考 khiin-rs `windows/ime` 与 imekit 的 TSF 实现）。
- 注册：`ITfInputProcessorProfiles::Register` 注册 GUID + 语言栏按钮；IMM32 仅作兼容回退。
- 候选窗口：**自绘置顶弹窗**（`verba-candidate` tiny-skia 光栅化 + 裸 Win32 置顶窗贴图；选型理由见 §12.2，曾评估 `ITfCandidateListUIElement`，弃用）。
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

**悬浮 AI 回复气泡（float-button v2，2026-09-24，issue verba-float-button-v2，用户裁定）**

v2 架构（macOS 已落地）：**进程内 NSPanel 气泡 + 点击/键盘触发无头采集**，激活路径零跨进程。
1. session 激活且 `float_button_enable = true`（默认关；密码类安全输入字段不弹）→ IME 进程内创建/复用 NSPanel（`Borderless | NonactivatingPanel` styleMask，**NSPanel 本体**——AppKit 对普通 NSWindow 忽略 Nonactivating 位；`NSFloatingWindowLevel`、透明背景、shadow），与候选窗同址生命周期：激活显、失活隐。锚点 = **光标所在行正下方（左对齐光标右缘，行底 + 16pt 间隙，22pt 气泡；真机验收两轮：右下压插入列遮挡光标 → 改行正下方；44pt 太大 → 缩半 22pt、间隙 8→16pt；独立评审：锚行顶+间隙仍压行下部，故锚行底，下方放不下翻转到行上方）**最终 **clamp 收进前台窗口 bounds**（`client_window_bounds`：CGWindowList 取客户端 pid 的 layer-0 窗口，光标包含优先、z 序最前兜底）；光标矩形无效（终端类客户端）退窗口右上内缩。v1 激活瞬间的垃圾光标矩形（D1）由「收进窗口」语义接管：任何方向 clamp 后都不可能画到屏幕外。
2. 点击气泡或 **`//`+TAB**（裸 `//` 空提示词 Tab，`Action::StartCapture`，与点击同一入口）→ busy 守卫（在途忽略）→ 后台 spawn **无头** `verba-trigger float-run`（截前台窗口 → daemon `OcrRecognize` → 识别文本写 stdout，**点击频率级 spawn**，同 /// 旧模式不在激活频率级 spawn）→ 文本落 `float_ocr_slot`，drain 定时器主线程消费：按 `float_button_prompt` 模板（`{ocr}` 占位，缺占位记日志拒绝）拼 prompt → **IME 进程内 `start_llm` 流式预览**（与 `//` 改写同通道，Enter 上屏/Esc 取消），helper 不再持有模板/会话。
3. `///` 选区 OCR 绑定**随 v2 下线**（machine 第三 `/` 臂删除，回落普通提示词字符；region-ocr 子命令保留为调试工具；Windows `Ctrl+Alt+O` 热键不受影响）。
4. **v1 教训（D2，验收阻塞级）**：per-activation spawn GUI helper 进程在飞书/终端类客户端引发 IME 会话 ≥10Hz 自激抖动（activate/spawn/deactivate 刷屏，无法输入）；v2 激活路径零跨进程从根上消灭自激环，点击级 spawn 与 `///` 选区同模式已被真机验证可接受。v1 `float-button --at x,y --session-id N --session-key S` 子命令保留（`run_float_button` 全管道在 helper 内跑），Windows TSF v1 窗模式仍在用；Windows v2 跟进前，`Action::StartCapture` 在 Windows 端为显式未接线臂（按键吞掉 + `log::warn!` 告警，不静默成功）。

已知限制（v2）：
- 屏幕录制权限的授予对象是 **verba-trigger helper 本体**（非输入法宿主 App）：macOS 首跑需在「系统设置 → 隐私与安全性 → 屏幕录制」勾选 verba-trigger，否则点击后管线失败（stderr/前端日志可见原因）。进程内 NSPanel 本身不截屏，无需权限。
- 截「前台窗口在屏幕上的可见区域」，被遮挡部分会带遮挡内容；不追求离屏窗口像素。气泡不会被自捕获误拍：`capture_active_window` 的 focused 判定 = NSWorkspace 活动应用 pid，nonactivating panel 不改变 frontmost application。
- 平台状态：macOS = v2 进程内 Panel（本节描述）；Windows = v1 helper 窗模式 + StartCapture 未接线告警；Linux Fcitx5 前端未就绪：共享层（capture_active_window / float-run 子命令）已平台中立落地，Fcitx5 前端就绪后按同模式接线（AGENTS.md 跨平台默认）。
- Wayland 焦点豁免以合成器为准（X11 已 override_redirect）。

**LLM 流式**
1. AI 模式触发（`//` 前缀或快捷键）→ 前端收集 prompt → daemon → LLM provider。
2. SSE 流 → `StreamEvent` 增量 → 前端 preedit 实时刷新 → Enter 上屏 / Esc 取消。

### AI 结果浮层的三相位与「候选框零延迟占位」（2026-09-16）

发送 → 首个 token 之间有 1–3s 空窗（取决于服务商 / 网络）。此前这段窗口里只有
**应用内的组合串**换成短状态串（`✨ 生成中…`），而用户视线所在的**候选框**完全
空着——观感是「按了没反应 / 什么都没发生」。契约（Windows TSF 与 macOS IMK 两端
同语义；组合串文案与占位正文收口在 `verba-core`，前端不得各写一份）：

| 阶段 | 组合串（preedit / marked） | 结果浮层（候选框） | 状态行 |
| --- | --- | --- | --- |
| 发送瞬间（**占位**） | `✨ 生成中…` | `PLACEHOLDER_RESULT_BODY`（`…`） | `result_hint(Streaming)` |
| 首个 chunk 到达 | 同上（**不随流变长**） | 流式全文（`Action::UpdateResult.body`） | 同上 |
| 完成 / 失败 | `✨ 已就绪` / `✨ 生成失败` | 结果全文 / 已生成的部分结果 | `result_hint(Ready/Failed)` |

- **零延迟**：占位在 `//`（含 `//看图`、改写管道 `//内容`+Tab）发送的**同一次
  按键处理**里**同步**弹出——这条路径只做「构造 controller + 渲染位图 +
  `cw.update(anchor)`」这类纯本地操作，不得出现 IPC / 网络 / 文件 IO / 线程
  spawn / 锁等待 / 重试定时器。「零延迟」靠同步构造，不靠动画或定时器刷新循环。
- **不引入额外 composition update**：组合串在发送时已刷成短状态串，占位只写
  浮层，不再调 `set_preedit` / `setMarkedText`（每次宿主往返都要省）。
- **幂等覆盖、不闪断**：首块到达走**同一条**浮层路径覆盖占位；浮层宽度恒为主题
  配置宽度，锚点固定（组合光标下方），行数增加只让底边向下长，顶边与左缘不跳动，
  同一批 chunk 内不反复重排。
- **占位期按键**：Enter / 空格 / `1` 一律**无操作**（空结果绝不结算——提交空串会
  抹掉提示词并触发「空组合文本 → 应用终止组合」陷阱，真机 Notepad-- 教训）；
  Esc = 取消流 + 收起浮层 + 结束组合，不留幽灵浮层；其余按键语义与流式态一致。
- **失败兜底**：daemon 连不上 / `llm_start` 失败 → 占位被 `Failed` 浮层覆盖
  （提示词保留，`r` 重试 / `e` 改提示词仍可用），不留「永远生成中」的僵尸浮层。
- **实现落点**：Windows `frontends/windows/ime/src/text_service.rs` 的
  `show_result_placeholder`（复用 `show_overlay_window` 口径）、macOS
  `frontends/macos/ime/src/imk.rs` 的 `start_llm`（`show_ai_result` 同一条面板
  路径）；两端共用 core 的 `PLACEHOLDER_RESULT_BODY` 与 `result_hint`。真机验收
  项见 [manual-acceptance-windows.md](manual-acceptance-windows.md) /
  [manual-acceptance-macos.md](manual-acceptance-macos.md)。

## 7. 配置与密钥

- 配置文件：`%APPDATA%/Verba/config.toml`、`~/Library/Application Support/Verba/config.toml`、`~/.config/verba/config.toml`。
- API Key 不入配置文件，走系统密钥库（`keyring` crate：Windows DPAPI / macOS Keychain / Linux Secret Service）。
- 设置面板（apps/settings，Slint 1.17）读写同一配置，热生效；API Key 经 IPC `ApiKeySet` 写系统密钥库并热更新。

## 8. 性能预算（目标）

> 范围（2026-08-29 Owner 决策）：ASR / TTS 冻结为实验性（代码保留、默认关闭、入口隐藏、不承诺），
> 不纳入性能预算。预算覆盖**正式能力**：直输、OCR、LLM 核心输入链路、daemon 常驻内存。
> 实测口径：Windows 11 + RapidOCR PP-OCRv5，daemon + IPC 全链路（2026-09-07）。

| 路径 | 预算 | 实测 |
| --- | --- | --- |
| 按键到上屏（直输 / 标点） | < 20ms | 待真机打点 |
| 截图 OCR 完成 | < 2s（本地 RapidOCR / PaddleOCR） | 原始分辨率：720p 1976ms ✅；1080p 3909ms ❌；4K 6099ms ❌ |
| LLM 首 token | < 2s（取决于服务商 / 网络） | 待测（远程依赖服务商） |
| daemon 空闲内存 | < 80MB（不含模型） | 待测 |

- OCR 延迟随分辨率近似线性增长，4K 全屏超预算显著。
- 落地路径：`verba-ocr` 识别前对超长边降采样到 `OCR_MAX_EDGE = 1600px`（4K / 1080p 压缩到 ≤1600×900），
  在保证屏幕文本可读性前提下压低延迟（降采样采用 Triangle 过滤，低开销）。
- ⚠️ 降采样后的实际延迟需 **Windows 真机复测**（待 #58 收口前补齐）；若复测仍 >2s，则应下调 `OCR_MAX_EDGE`（如 ≤1280），
  预算按「复测达标」口径验收。极端小字号场景若需更高分辨率，再按分辨率分档。

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
2. ~~TSF 候选窗口：原生 `ITfCandidateListUIElement` vs 自绘 overlay~~ → **已定：自绘置顶弹窗 + tiny-skia CPU 光栅化**（2026-08-23 落地，实机验收）。
   - 弃用系统候选 UI：TSF `ITfCandidateListUIElement` 样式受限、生命周期与焦点管理复杂；macOS `IMKCandidates` 是老 AppKit 面板，主题/分页/光标避让接不上，且两端 UX 分叉——自绘对齐微软拼音/搜狗的可主题化路线。
   - 渲染器选 tiny-skia 而非：① 系统绘制 API（CoreGraphics / Direct2D 要写两套实现，且无法跨平台无头验证）；② Slint/femtovg（候选窗运行在注入宿主进程的 TSF DLL 内——Slint 自带 winit 事件循环会与宿主消息循环冲突，femtovg 面向 OpenGL 上下文、宿主进程内起 GL 上下文代价高；候选窗只要光栅化器，不要 UI 框架）。
   - tiny-skia：纯 Rust、CPU 光栅化、无上下文、像素确定性，`render_png` example 可在无窗口环境（含 CI）渲染验证；前端用裸 Win32 置顶弹窗（`WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`）贴图，不抢焦点、不激活宿主应用。
   - Slint 用于独立进程的设置面板（apps/settings，默认 femtovg 渲染）——两者运行环境不同，各取所需。
3. macOS appex 常驻限制下的 daemon 启动 / 生命周期策略。
4. 截图实现：Windows Graphics Capture vs GDI；macOS ScreenCaptureKit vs CGWindowList。
5. imekit 是否作为 Linux / Wayland 基座（评估后决定依赖 or fork，Apache/MIT 双许可）。
6. LLM 多轮上下文与隐私边界的默认策略（新装默认 50 轮，0=关闭，可配置；上屏文本默认并入窗口级上下文会话，见 docs/privacy.md）。