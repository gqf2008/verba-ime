# 路线图

> 更新：2026-08-23 · 当前状态：**M5 已收口**（2026-08-23 实机 OK：候选窗即时内置 + Rime 去重追加，
> 验收清单逐项勾选，见 manual-acceptance-windows.md）。
> 下一批：**M3 多模态 + M4 TTS 的 Rust 核心**（provider 抽象 + daemon 路由 + IPC + CLI，全部本机可验证）。
> 已开工（2026-08-23）：TTS mock provider 端到端——`verba-cli tts` 产出合法 WAV；edge/Piper/系统 TTS 与 OCR/ASR 跟进。
> 原则：每个里程碑都有可验收的端到端结果；先打通一条完整链路（Windows + LLM），再铺平台，再加能力，最后打磨发布。

## 里程碑总览

| 阶段 | 主题 | 交付物 / 验收 | 依赖 |
| --- | --- | --- | --- |
| M0 | 地基 | 仓库骨架、Cargo workspace、CI、verba-cli、core 状态机雏形 | — |
| M1 | Windows 垂直切片 | Windows TSF 前端 + LLM 远程直输：安装输入法 → 打字上屏 → `//` 唤起 AI → 流式上屏 | M0 |
| M2 | 三端齐平 | macOS IMK、Linux Fcitx5 / IBus 前端，直输 + LLM 与 M1 对齐 | M1 |
| M3 | 多模态 | OCR（截图）与 ASR（语音）在至少一个平台跑通，其余平台跟进 | M1 / M2 |
| M4 | 体验打磨 | TTS、候选窗口、Tauri 设置面板、性能预算、隐私开关 | M3 |
| M5 | 中文引擎 | 内置轻量拼音引擎已落地（`verba-pinyin`）；继续评估 librime（五笔 / 模糊音 / Rime 生态） | M4 |
| M6 | 发布 | 打包、签名、公证、Alpha / Beta、文档与社区运营 | M5 |

## M0 详细任务（进行中）

- [x] 命名与品牌定稿（Verba · 拾言，见 [naming.md](naming.md)）
- [x] 架构与技术选型文档（[architecture.md](architecture.md)）
- [x] 仓库骨架 + Cargo workspace + AGENTS.md
- [ ] CI：cargo check / test / clippy / fmt 三平台 matrix
- [ ] verba-core：模式状态机 + composition 缓冲 + 命令路由（含测试）
- [ ] verba-protos / verba-ipc：回环打通（含测试）
- [ ] verba-cli：命令驱动 core（模拟前端）

## M1 详细任务（Windows 垂直切片）

- [x] verba-ai：LLM provider（OpenAI 兼容，SSE 流式）
- [x] verba-config：配置读写 + keyring 密钥
- [x] Windows TSF 前端（windows 0.62）：注册（HKCU CLSID）、上屏、preedit、流式；COM 实例化与 Activate/Deactivate 冒烟通过
- [x] daemon 与 TSF 的 IPC 打通（daemon 自动拉起 + 流线程）
- [x] `//` 进入 AI 模式 → 流式 preedit → Enter 上屏（代码完成）
- [x] Inno Setup 安装脚本 + [Windows 手动验收清单](manual-acceptance-windows.md)
- [x] 隐私提示（[docs/privacy.md](privacy.md)）
- [ ] **真机验收**：需管理员安装（TSF 档案注册）+ 交互会话逐项验证（清单见上）。当前「直输上屏 / `//` preedit / 流式 preedit / Enter 提交 / 空闲态不吞键 / 激活注销」均已有 TSF API 层自动化测试覆盖（frontends/windows/ime 的 tsf_smoke），真机仅剩系统注册与交互冒烟。

## M2 详细任务（macOS / Linux）

- [ ] macOS IMK 前端（Swift 薄壳 + IPC，Unix Socket）
- [ ] Linux Fcitx5 插件（C++ shim + Rust 核心）
- [ ] Linux IBus / Wayland 兼容（imekit 评估）
- [ ] 三端功能对齐矩阵 + 各端手动验收清单

## M3 详细任务（多模态）

- [ ] OCR provider：本地 PaddleOCR（ONNX）+ 平台原生（Windows.Media.Ocr / Vision）
- [ ] 截图链路：权限、选区、预览、OCR 结果上屏
- [ ] ASR provider：本地 whisper.cpp（whisper-rs）+ 可选云端
- [ ] 语音链路：快捷键、录音、流式转写、上屏

## M4 详细任务（体验）

- [ ] TTS provider：系统 TTS / edge-tts / Piper（可选）
  - [x] **mock**（确定性 WAV，2026-08-23：`verba-tts` crate + IPC `TtsSynthesize`/`Audio` + daemon 路由 + `verba-cli tts`，CLI/验收通过）
  - [ ] edge-tts（网络） / 系统 TTS（Windows SAPI） / Piper（本地）
- [x] 候选窗口样式与交互（分页、主题、皮肤）（随 M5 完成，2026-08-23 实机验收）
- [ ] Tauri 设置面板（服务商配置、快捷键、隐私开关）
- [ ] 性能与内存预算达标（见 architecture §8）
- [ ] 日志脱敏与崩溃上报（本地）

## M5 详细任务（中文引擎）

- [x] 内置轻量拼音引擎（`verba-pinyin`：hanzi_db 字频 + CC-CEDICT 词库，音节切分、频率排序、前缀补全、模糊音、简拼、整句 DP、提示词拼音）
- [x] Windows 前端拼音组合：字母进拼音、内联候选、数字/空格选候选上屏、`//` 提示词内拼音输中文（2026-08-22）
- [x] 选型评估：[中文引擎选型与集成评估](chinese-engine-evaluation.md)——结论：librime FFI > 重写，默认自研 + librime 可选（daemon 内 spike）
- [x] 独立候选窗（跟随光标 + 智能避让）——tiny-skia 自绘置顶弹窗，锚点取组合屏幕坐标（只读编辑会话内 GetTextExt），默认正下方、放不下翻上方、水平防越界（2026-08-23 实机验收通过）
- [x] 候选窗分页（9→27 候选，`-`/`=` 与 PageUp/PageDown 翻页、页码脚）与主题/皮肤（light/dark 预设 + 逐项覆盖、圆角、配置热更新）（2026-08-23）
- [x] librime-sys spike（Windows）：预编译 rime.dll FFI 验证——拼音 luna_pinyin + 五笔 wubi86
  跑通（octagram 数据未捆绑，另配后再评估）（2026-08-23）
- [x] librime daemon 集成：`verba-librime` crate（动态加载 rime.dll，拼音/五笔候选）+ IPC
  `RimeCandidates` + `config 引擎=builtin|rime` + `rime_schema`（luna_pinyin_simp/wubi86）；
  CLI `verba-cli rime` 端到端验证通过（你是谁/你好），前端 engine=rime 时请求并融合 rime 候选
  （2026-08-23 实机验收通过）
- [x] 整句基准：50 句日常对话首候选——自研 6% vs Rime 84%（无 octagram）/ 74%（+essay 模型）；
  结论 librime 整句显著更优、octagram 对口语有害不默认启用（2026-08-23，见
  [chinese-engine-evaluation.md](chinese-engine-evaluation.md) §8）
- [x] 候选融合（词库候选 + LLM 候选，IPC 协议扩展 `LlmCandidates`/`Candidates`；
  拼音态停顿 320ms 后请求，增量合并去重、按拼音校验过期结果；mock 端到端冒烟通过，
  实机验收通过）（2026-08-23）

## 风险与开放问题

- **平台审核与签名**：macOS 公证、Windows SmartScreen 需要证书与流程，提前规划。
- **Wayland 碎片化**：不同合成器对 `zwp_input_method_v2` 支持不一（GNOME 需 IBus），需多后端。
- **本地模型体积 / 性能**：whisper.cpp 模型 75MB+，PaddleOCR 10MB+；首次下载与按需加载策略。
- **LLM 成本与延迟**：远程调用不可控，需超时 / 取消 / 失败重试策略。
- **权限复杂度**：macOS 录屏 / 麦克风 TCC；Linux 不同桌面权限模型。
- **同类竞争**：讯飞、百度输入法已有 AI 功能；素言输入法（离线语音 + 截图）是近期最接近的竞品——差异化主打「开源 + 三平台 + 可插拔服务商」。

## 变更记录

| 日期 | 变更 |
| --- | --- |
| 2026-08-22 | 初版：M0-M6 里程碑与任务分解 |
| 2026-08-23 | M5 实机验收收口（候选窗即时内置 + Rime 追加）；M4 TTS mock provider 接入（`verba-tts` + IPC TtsSynthesize/Audio + daemon 路由 + CLI + 验收） |
