# 路线图

> 更新：2026-08-25 · 当前状态：**单引擎化（Rime）已定案并实机验证**（Windows TSF + macOS IMK 共用 daemon 内
> librime；内置 `verba-pinyin`、`config engine` 开关、打字过程 LLM 候选融合均已移除）；多模态（OCR/ASR）与
> TTS（edge-tts / OpenAI 兼容）已打通；Slint 设置面板已落地（apps/settings）。剩余：whisper.cpp / audio.cpp
> 本地 ASR（可选）、Piper / 系统 TTS、性能预算、日志脱敏、M6 发布（签名/公证/安装包）。
> 原则：每个里程碑都有可验收的端到端结果；先打通一条完整链路（Windows + LLM），再铺平台，再加能力，最后打磨发布。

## 里程碑总览

| 阶段 | 主题 | 交付物 / 验收 | 依赖 |
| --- | --- | --- | --- |
| M0 | 地基 | 仓库骨架、Cargo workspace、CI、verba-cli、core 状态机雏形 | — |
| M1 | Windows 垂直切片 | Windows TSF 前端 + LLM 远程直输：安装输入法 → 打字上屏 → `//` 唤起 AI → 流式上屏 | M0 |
| M2 | 三端齐平 | macOS IMK、Linux Fcitx5 / IBus 前端，直输 + LLM 与 M1 对齐 | M1 |
| M3 | 多模态 | OCR（截图）与 ASR（语音）在至少一个平台跑通，其余平台跟进 | M1 / M2 |
| M4 | 体验打磨 | TTS（在线）、候选窗口、Slint 设置面板、性能预算、隐私开关 | M3 |
| M5 | 中文引擎 | **Rime（librime）单引擎**已落地（拼音/五笔 + 候选窗/分页/主题，实机验收通过）；内置 `verba-pinyin` 已移除 | M4 |
| M6 | 发布 | 打包、签名、公证、Alpha / Beta、文档与社区运营 | M5 |

## M0 详细任务（已完成）

- [x] 命名与品牌定稿（Verba · 拾言，见 [naming.md](naming.md)）
- [x] 架构与技术选型文档（[architecture.md](architecture.md)）
- [x] 仓库骨架 + Cargo workspace + AGENTS.md
- [x] CI：cargo check / test / clippy / fmt 三平台 matrix（2026-08-24 main 全绿）
- [x] verba-core：模式状态机 + composition 缓冲 + 命令路由（含测试）
- [x] verba-protos / verba-ipc：回环打通（含测试）
- [x] verba-cli：命令驱动 core（模拟前端）

## M1 详细任务（Windows 垂直切片）

- [x] verba-ai：LLM provider（OpenAI 兼容，SSE 流式）
- [x] verba-config：配置读写 + keyring 密钥
- [x] Windows TSF 前端（windows 0.62）：注册（HKCU CLSID）、上屏、preedit、流式；COM 实例化与 Activate/Deactivate 冒烟通过
- [x] daemon 与 TSF 的 IPC 打通（daemon 自动拉起 + 流线程）
- [x] `//` 进入 AI 模式 → 流式 preedit → Enter 上屏（代码完成）
- [x] Inno Setup 安装脚本 + [Windows 手动验收清单](manual-acceptance-windows.md)
- [x] 隐私提示（[docs/privacy.md](privacy.md)）
- [x] **真机验收**（2026-08-22 实机通过：Notepad-- 内 `//translate hello world` → 流式中文回复 → 上屏；其余清单项见 [manual-acceptance-windows.md](manual-acceptance-windows.md)）

## M2 详细任务（macOS / Linux）

- [x] **共享 Rust 核心跨平台验证**（2026-08-23：CI check 矩阵 windows/macos/ubuntu 建设与测试共享核心（crates + apps/settings）；Rust 核心仅有 Windows 额外 crate 用 `cfg(windows)` 隔离，本质可跨平台。
- [x] **macOS IMK 前端（全 Rust，objc2 + objc2-input-method-kit）**（2026-08-23：`frontends/macos/ime`，`MacIme::connect/ping` + `imk`（`define_class!` 子类化 `IMKInputController`，实现 `IMKStateSetting`）+ `ffi`（C ABI）；CI `frontend-macos` 在 macos-latest 构建验证）
- [x] **macOS IMK 输入处理与注册**（2026-08-24：`inputText:key:modifiers:client:` 收键 → verba-core 状态机 → 上屏/标记文本/候选窗；
  `//` AI 模式 LLM 流式经 daemon；`app/Info.plist` + `ComponentInputModeDict` + `TISInputSource`；`scripts/package.sh` 打包 `dist/Verba.app`
  （含 verba-mac 与 verba-daemon，ad-hoc 签名）；修复 verba-librime 非 Windows 链接 kernel32 问题使 daemon 可在 macOS 构建）
- [ ] macOS IMK 真机交互验收（2026-08-24 librime 单引擎链路已真机验证通过；候选窗自动展示、
  输入法切换、TIS 注册待完整交互验收；多客户端会话语义为已知限制，见「风险与开放问题」）
- [ ] Linux Fcitx5 插件（C++ shim + Rust 核心）—— **低优先（用户确认）**
- [ ] Linux IBus / Wayland 兼容（imekit 评估）—— 低优先
- [ ] 三端功能对齐矩阵 + 各端手动验收清单

## M3 详细任务（多模态）

- [ ] OCR provider：本地 PaddleOCR（ONNX）+ 平台原生（Windows.Media.Ocr / Vision）
  - [x] **mock**（确定性，2026-08-23：`verba-ocr` crate + IPC `OcrRecognize` + daemon 路由 + `verba-cli ocr`）
  - [x] **Windows.Media.Ocr**（2026-08-23 实机验证：英文识别 OK；中文需装 OCR 语言包，代码优先 zh-Hans-CN 并有回退）
  - [x] **rapid（本地 RapidOCR/PaddleOCR PP-OCRv4）**（2026-08-23：`verba-ocr::rapid`，原生 Rust ONNX（`ort`/`rapidocr-core`，PP-OCRv5 中文 mobile；MSVC 编译，模型自动下载，无 Python），中文实测正确；本机 GNU 工具链无 `ort` 预编译故走子进程；`ocr_provider=rapid` + `ocr_rapid_python`/自动探测 venv）
  - [x] **多模态 vision（`//看图`）**（2026-08-23：`LlmRequest.image` + IPC `image/image_mime` + daemon 透传 + OpenAI 兼容 `image_url`；`eye_mode=vision` 直接把眼睛区域发图给 LLM）
  - [x] **眼睛区域按光标智能取屏**（2026-08-23：复用候选窗工作区避让逻辑，默认上方、放不下翻下方/贴边）
- [ ] 截图链路：权限、选区、预览、OCR 结果上屏
  - [x] **触发能力地基**（2026-08-23：`verba-trigger` 全屏截图→BMP→daemon OCR 端到端实机验证，Windows.Media.Ocr 真识别）
  - [x] **TSF 热键/`//截图` 命令接线**（2026-08-23：Ctrl+Alt+O 或 `//截图` → 截图 OCR 结果上屏；待实机验收）
  - [x] **选区截图（工具层）**（2026-08-23：`verba-trigger region-shot/region-ocr`，交互拖选 + `--rect` 脚本化；`--rect` 实机验证，交互拖选待验收）
  - [x] **TSF 内接线**（2026-08-23：`//截图` / Ctrl+Alt+O 改为调 `verba-trigger region-ocr` 选区拖选 → OCR 上屏，失败回退全屏；新 DLL target_dev16，待实机验收）
- [ ] ASR provider：本地 whisper.cpp（whisper-rs）+ 可选云端
  - [x] **mock**（确定性，2026-08-23：`verba-asr` crate + IPC `AsrTranscribe` + daemon 路由 + `verba-cli asr`）
  - [x] **openai 在线**（OpenAI 兼容 `audio/transcriptions`，复用 LLM base_url+key；config `asr_provider=openai` + `asr_model`/`asr_base_url`，2026-08-23）
  - [ ] whisper.cpp（whisper-rs，本地模型）/ audio.cpp 子进程（本地，可选）
- [ ] 语音链路：快捷键、录音、流式转写、上屏
  - [x] **触发能力地基**（2026-08-23：`verba-trigger` 麦克风录音→WAV→daemon ASR、TTS 合成→播放（rodio）端到端实机验证）
  - [x] **TSF 热键/`//听写` `//朗读` 命令接线**（2026-08-23：Ctrl+Alt+M 或 `//听写` → 录音 ASR 上屏；`//朗读 <文本>` → TTS 播放；待实机验收）
  - [ ] 流式转写：边录边出字

## M4 详细任务（体验）

- [ ] TTS provider：系统 TTS / edge-tts / Piper（可选）
  - [x] **mock**（确定性 WAV，2026-08-23：`verba-tts` crate + IPC `TtsSynthesize`/`Audio` + daemon 路由 + `verba-cli tts`，CLI/验收通过）
  - [x] **edge-tts**（微软 Edge 在线神经音色，2026-08-23 实机验证：`verba-tts` Edge provider 接入（WSS + SSML + Sec-MS-GEC 签名），`verba-cli tts` 出真实 MP3，`voice` 可覆盖，默认 zh-CN-XiaoxiaoNeural）
- [x] **openai 在线 TTS**（OpenAI 兼容 `audio/speech`，复用 LLM base_url+key；config `tts_provider=openai` + `tts_model`/`tts_base_url`/`tts_voice`，2026-08-23）
- [ ] 系统 TTS（Windows SAPI）/ Piper（本地）/ audio.cpp 子进程（本地，可选）
- [x] 候选窗口样式与交互（分页、主题、皮肤）（随 M5 完成，2026-08-23 实机验收）
- [x] Slint 1.17 设置面板（`apps/settings`：LLM/多模态/引擎/快捷键/隐私，GetConfig/SetConfig/ApiKeySet IPC 热生效；2026-08-23）
- [ ] 性能与内存预算达标（见 architecture §8）
- [x] AI 模式多轮上下文（2026-08-23：`ai_context_turns` + LlmRequest.history + daemon 会话历史 + `//重置` 清空 + 设置面板可配；端到端验证第2轮携带历史）
- [x] 诊断与日志（2026-08-23：daemon 写 `data/logs/verba-daemon.log`；`verba-cli diag` 输出健康/关键配置/日志尾/相关进程/rapid 就绪状态）
- [ ] 日志脱敏与崩溃上报（本地）

## M5 详细任务（中文引擎）

- [x] 内置轻量拼音引擎（`verba-pinyin`：hanzi_db 字频 + CC-CEDICT 词库，音节切分、频率排序、前缀补全、模糊音、简拼、整句 DP、提示词拼音）——**已于 2026-08-24 单引擎化时移除**（保留作历史记录）
- [x] Windows 前端拼音组合：字母进拼音、内联候选、数字/空格选候选上屏、`//` 提示词内拼音输中文（2026-08-22）
- [x] 选型评估：[中文引擎选型与集成评估](chinese-engine-evaluation.md)——结论：librime FFI > 重写；最终定案 **单引擎 Rime**（2026-08-24）
- [x] 独立候选窗（跟随光标 + 智能避让）——tiny-skia 自绘置顶弹窗，锚点取组合屏幕坐标（只读编辑会话内 GetTextExt），默认正下方、放不下翻上方、水平防越界（2026-08-23 实机验收通过）
- [x] 候选窗分页（9→27 候选，`-`/`=` 与 PageUp/PageDown 翻页、页码脚）与主题/皮肤（light/dark 预设 + 逐项覆盖、圆角、配置热更新）（2026-08-23）
| 2026-08-23 | 候选窗 UI 现代化（横向候选栏 + 拼音组合头 + 页码脚，对齐微软拼音/手心；theme.layout 可切 vertical；`verba-candidate` renderer 重构） |
- [x] librime-sys spike（Windows）：预编译 rime.dll FFI 验证——拼音 luna_pinyin + 五笔 wubi86
  跑通（octagram 数据未捆绑，另配后再评估）（2026-08-23）
- [x] librime daemon 集成：`verba-librime` crate（动态加载 rime.dll / librime.dylib，拼音/五笔候选）+ IPC
  `RimeCandidates` + `rime_schema`（luna_pinyin_simp/wubi86）；
  CLI `verba-cli rime` 端到端验证通过（你是谁/你好）；macOS 真机亦验证（2026-08-24）
- [x] 整句基准：50 句日常对话首候选——自研 6% vs Rime 84%（无 octagram）/ 74%（+essay 模型）；
  结论 librime 整句显著更优、octagram 对口语有害不默认启用（2026-08-23，见
  [chinese-engine-evaluation.md](chinese-engine-evaluation.md) §8）
- [x] ~~候选融合~~（词库候选 + LLM 候选，IPC 协议扩展 `LlmCandidates`/`Candidates`；
  拼音态停顿 320ms 后请求，增量合并去重、按拼音校验过期结果；mock 端到端冒烟通过，
  实机验收通过）（2026-08-23）——**自动触发已于 2026-08-24 移除**（见下），候选只走 Rime
- [x] **决策：移除打字过程的 LLM 候选融合自动触发**（2026-08-24）——候选只走 Rime，
  LLM 仅用于 `//` + 回车触发的 AI 直输（`StartLlm`）。理由：候选本应是本地的；LLM 只按「用户输入
  → 输出结果」使用，打字过程调用远程 LLM 会造成每键成本与延迟。`LlmCandidates` daemon handler /
  IPC 保留但前端不再自动触发。

## 风险与开放问题

- **平台审核与签名**：macOS 公证、Windows SmartScreen 需要证书与流程，提前规划。
- **Wayland 碎片化**：不同合成器对 `zwp_input_method_v2` 支持不一（GNOME 需 IBus），需多后端。
- **本地模型体积 / 性能**：whisper.cpp 模型 75MB+，PaddleOCR 10MB+；首次下载与按需加载策略。
- **LLM 成本与延迟**：远程调用不可控，需超时 / 取消 / 失败重试策略。
- **权限复杂度**：macOS 录屏 / 麦克风 TCC；Linux 不同桌面权限模型。
- **macOS 多客户端会话语义**：LLM 流/候选队列为进程级全局状态，单活跃会话可用；多会话并行需改为 per-controller 状态（M2 遗留）。
- **同类竞争**：讯飞、百度输入法已有 AI 功能；素言输入法（离线语音 + 截图）是近期最接近的竞品——差异化主打「开源 + 三平台 + 可插拔服务商」。

## 变更记录

| 日期 | 变更 |
| --- | --- |
| 2026-08-22 | 初版：M0-M6 里程碑与任务分解 |
| 2026-08-23 | M5 实机验收收口（候选窗即时内置 + Rime 追加）；M4 TTS mock provider 接入（`verba-tts` + IPC TtsSynthesize/Audio + daemon 路由 + CLI + 验收） |
| 2026-08-23 | M3 OCR 接入（`verba-ocr` + IPC OcrRecognize + daemon 路由 + CLI + 验收；mock 确定性 + Windows.Media.Ocr 本地识别实机验证） |
| 2026-08-23 | M3 ASR 接入（`verba-asr` + IPC AsrTranscribe + daemon 路由 + CLI + 验收；mock 确定性；whisper.cpp 待接入） |
| 2026-08-23 | M4 TTS edge-tts 接入（`verba-tts` Edge provider：WSS + SSML + Sec-MS-GEC，`verba-cli tts` 实机出 MP3；mock 仍为默认） |
| 2026-08-23 | 前端触发能力地基（`verba-trigger`：截图→OCR / 录音→ASR / TTS→播放，capture/record/play 模块 + CLI，端到端实机验证） |
| 2026-08-23 | TSF 触发接线（Ctrl+Alt+O/M 热键 + `//朗读` `//截图` `//听写` 命令；新 DLL target_dev15，待实机验收） |
| 2026-08-23 | 选区截图工具（`verba-trigger region-shot/region-ocr`：半透明遮罩拖选 + 选区 BitBlt + OCR；`--rect` 脚本化） |
| 2026-08-23 | TSF 选区接线（`//截图` / Ctrl+Alt+O 子进程调 region-ocr，选区拖选 OCR 上屏，失败回退全屏；新 DLL target_dev16） |
| 2026-08-23 | 在线 ASR/TTS provider（OpenAI 兼容 `audio/transcriptions` + `audio/speech`，复用 LLM base_url+key；config 新增 `asr_base_url`/`asr_model`/`tts_base_url`/`tts_model`） |
| 2026-08-23 | 修复 keyring 未启用平台后端（默认 mock 内存存储不跨进程）——启用 windows-native/apple-native/linux-native-sync-persistent |
| 2026-08-23 | Slint 1.17 设置面板 `apps/settings`（替代 Tauri：LLM/多模态/引擎/快捷键/隐私 + GetConfig/SetConfig/ApiKeySet IPC 热生效；`verba-cli key` 查看/设置/清除密钥） |
| 2026-08-23 | 候选窗 UI 现代化（横向候选栏 + 拼音组合头 + 页码脚，对齐微软拼音/手心；theme.layout 可切 vertical；erba-candidate renderer 重构） |
| 2026-08-24 | 拼音分段承诺（`verba-pinyin::lookup_segmented` + `CompositionMachine` committed/commit_offset）：选子短语候选可保留剩余拼音继续组合、Backspace 弹回已选段、消费完自动整句提交；顺带实现 mir2x/libpinyin 手感参考（[docs/libpinyin-mir2x-smoothness.md](docs/libpinyin-mir2x-smoothness.md)） |
| 2026-08-24 | macOS IMK 候选引擎对齐（engine=rime）：`start_candidates` 读取 config.engine/rime_schema，engine=rime 时经 `rime_candidates` IPC 一次性请求 Rime 整句候选并压入候选队列，与 Windows TSF 的候选策略一致（分段承诺/整句候选跨平台） |
| 2026-08-24 | 候选只走本地引擎；移除打字过程的「LLM 候选融合」自动触发：前端不再在拼音变化时调 `llm_candidates_start`（Windows/macOS 一致），LLM 仅用于 `//` + 回车触发的 AI 直输（`StartLlm`），打字零 LLM 调用、零成本 |
| 2026-08-24 | **单引擎化（Rime）**：移除内置 `verba-pinyin` 候选引擎（不再生成候选），中文候选统一由 daemon 内 Rime 提供（`config engine` 默认 `rime`，启动预热）；`CompositionMachine` 候选只经 `on_llm_candidates` 注入，`//` 提示词拼音也走 Rime；`verba-cli pinyin` 改走 `verba-cli rime`；`verba-pinyin` 从 workspace 移除。全面 `cargo test`/`clippy` 通过，macOS 前端验证通过 |
| 2026-08-24 | 移除冗余的 `config engine` 开关（单引擎已无切换对象）：配置/daemon/前端（Windows+macOS）/settings/CLI 全部去掉 engine 判断，恒走 Rime；`rime_schema` 保留。`cargo test`/`clippy` 通过 |
| 2026-08-24 | 关闭「候选即时性」议题：单引擎下候选依赖 daemon Rime 查询返回（本地、启动预热），候选窗在返回前为空是正常状态；防抖是后端优化，不构成 UX「延时」问题，实测 mir2x 无延时，不再打点/调整 |
| 2026-08-24 | macOS 支持 librime（`librime.dylib`）：`verba-librime` 由 Windows-only 重构为跨平台 `platform.rs`（libloading 统一加载 rime.dll / librime.dylib）；daemon `rime_paths` 平台化；打包捆绑 `vendor/rime`。修复 macOS 单引擎化后候选为空（P1 审查项） |
| 2026-08-25 | 文档对齐（docs-only）：README 平台状态/快速开始/架构图、M0/M1 勾选、M5 单引擎化表述收口（`config 引擎=builtin|rime`、`verba-pinyin` 现状化）、macOS 多会话限制入风险、评估文档过期建议标注 |