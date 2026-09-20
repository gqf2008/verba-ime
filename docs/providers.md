# AI 服务商矩阵（OCR / ASR / LLM / TTS）

> 更新：2026-09-21 · 原则：**本地优先（隐私 + 免费 + 离线），云端可插拔**；每个能力一个 trait、多个 provider，运行时按配置选择并支持降级。
> **2026-08-29 Owner 决策：ASR/TTS 冻结为实验性（代码保留、默认关闭、入口隐藏，不承诺，不在 M6 范围）；OCR/LLM 为正式能力。**
> 关联：[架构设计](architecture.md) §6、[IPC 协议](protocol.md)。

## OCR（图片 / 截图 → 文字）

| 方案 | 类型 | 说明 | Rust 接入 | 评价 |
| --- | --- | --- | --- | --- |
| RapidOCR / PaddleOCR（PP-OCRv4） | 本地 | 中文最强开源 OCR，det + rec + cls 小模型约 10-20MB | ONNX Runtime（`ort` crate）；本机 `x86_64-pc-windows-gnu`（无 MSVC）无 `ort` 预编译，故原生 Rust ONNX（`ort` + `rapidocr-core`，PP-OCRv5 中文 mobile，模型自动下载到 data/models-rapidocr；需 MSVC 工具链编译，`ocr_provider=rapid`，2026-08-23 实机 OK，无 Python）；**构建期 ort 预编译 dist 的坑与自愈见 [building.md](building.md)「ONNX Runtime（ort）预编译 dist 自愈」** | ✅ 默认本地方案 |
| PaddleOCR-VL / DeepSeek-OCR | 本地 | 新式 VLM OCR，效果好、模型大（GB 级） | `ort` / candle | 备选，视硬件 |
| Windows.Media.Ocr | 系统 | Win10+ 内置 OCR，多语言 | `windows` crate | ✅ Windows 快速路径 |
| Apple Vision（VNRecognizeTextRequest） | 系统 | macOS 原生 OCR，质量好 | Swift / ObjC（或 objc2） | ✅ macOS 快速路径 |
| Tesseract（`leptess`） | 本地 | 老牌 OCR | `leptess` crate | 中文一般，兜底 |
| 百度 / 腾讯 / 阿里 / Google Vision | 云端 | 准确率高、按量计费 | HTTP | 可选 |

### 候选评估：`jamjamjon/usls`（2026-09-21 实测，**不采用**）

触发：用户提出「换 onnx 运行时 jamjamjon/usls」。评估结论按证据分三层。

**1. 定位：usls 不是运行时，是建在 `ort`/onnxruntime 之上的模型库**（MIT）。"换运行时"实际等于换模型集成层；运行时仍是 `ort`（版本反而被它钉住）。它覆盖 YOLO/SAM/CLIP/深度/VLM 等大量视觉模型，OCR 侧只有 `DB`(检测) + `SVTR`(识别) 两个封装（含 `ppocr_det/rec_v5_mobile|server` 预设与中文 vocab），**没有 det→rec 合成管线，也不含文本行方向分类（cls）**。

**2. 硬阻塞：与现有 OCR 路径无法共图。** usls 0.1.11 硬钉 `ort = "=2.0.0-rc.10"`，`rapidocr-core` 要 `^2.0.0-rc.12`（本仓锁到 rc.13）→ cargo 直接报版本冲突。要"新老两条路并存"只能走**未发布**的 main（0.2.0-alpha.4：ort=rc.13、MSRV 1.88、feature 重组为 `vision`/`vlm`）。即：稳定版不能共存，能共存的版本不稳定。

**3. 实测（macOS aarch64，同一份 PP-OCRv5 mobile 权重）**

- `Config::ppocr_rec_v5_mobile()` 的 SVTR 预设把宽度上限取成 `with_model_ixx(0,3,(320,960,3200))` 的 **opt=960**（声明里的 max=3200 从未生效）：**宽高比 >20 的文本行必报** `Size of the crop box is out of the image boundaries`（fast_image_resize CropBoxError）。边界可复现：`240×12` 过 / `241×12` 挂、`940×48` 过 / `961×48` 挂；换 usls 自家权重+vocab 同样复现——而输入法截屏的长行小字正是这个比例。
- 一行修复（`.with_model_ixx(0,3,(320,3200,3200).into())`）后质量与现状持平，但更慢：1440×900 小字 14 行取中位——现状 1594ms / 修宽 2780ms / 切分 ≤19:1 1720ms（词被切断）/ 纵向补白 1199ms（掉字）。
- 词汇表格式不同：usls 期望 **18385 行**（首行 `BLANK`、末行空格），RapidOCR 的 `ppocrv5_dict.txt` 是 18383 行；直接喂会在 rec 阶段 `index out of bounds` **panic**（内部下标取值，不是 `Err`）。
- 权重来自个人 GitHub releases（`jamjamjon/assets`），默认 feature 开 `github`（拉 ureq）；模型不在本地又关掉该 feature 时 `try_fetch` 是 `unimplemented!()` panic。国内网络需镜像或随包内置。

**结论**：**不为 OCR 换 usls**——现有需求（截图中文小字、<2s 预算）下它开箱不可用，修好后也劣于/持平现状，代价却是"下架 rapidocr-core + 无法双路并存 + 权重源变更 + 跟进未发布大改版"。

**再评估条件（本批保留）**：若要做**本地视觉能力**（本地 VLM 看图 / 元素定位 / 去背等），usls 是 Rust 生态覆盖面较广的选择，值得单独 spike；前提是 ①ort 对齐到 rc.13（走 main 或等 0.2.x 发布）、②权重来源可镜像或随包内置；且以**新增能力**形态引入，不走替换 OCR 的路径。

## ASR（语音 → 文字）—— **❄️ 已冻结为实验性（2026-08-29：代码保留、默认关闭、入口隐藏，不承诺）**

| 方案 | 类型 | 说明 | Rust 接入 | 评价 |
| --- | --- | --- | --- | --- |
| whisper.cpp | 本地 | 开源最强本地 ASR，base / small 中英可用，可流式 | `whisper-rs` | ❄️ 冻结（不实现） |
| 系统听写（Windows Speech / macOS Dictation） | 系统 | 平台级，受系统语言限制 | 平台 API | 备选 |
| 讯飞 / 百度 / 腾讯 | 云端 | 中文流式强、低延迟 | HTTP / WS | 可选 |
| OpenAI Whisper API | 云端 | 质量高、按分钟计费、非流式 | HTTP | 可选 |
| OpenAI 兼容 `audio/transcriptions` | 云端 | 复用 LLM base_url+key，whisper 系列模型 | HTTP multipart（`verba-asr::openai`，2026-08-23 已实现） | ❄️ 已实现，默认关闭（`asr_provider` 默认禁用） |
| audio.cpp（STT） | 本地 | ggml 本地 ASR/VAD，模型族丰富 | audio.cpp 预编译包子进程 / audio-cpp-rs | 本地可选（后续） |
| GLM-ASR / Fun-ASR（candle） | 本地 | 新一代开源 ASR，评估中 | candle | 未来 |

## LLM（远程，统一 OpenAI 兼容接口）

| 服务商 | 端点示例 | 模型示例 | 说明 |
| --- | --- | --- | --- |
| OpenAI | api.openai.com | gpt-4o / gpt-4.1 | 国际 |
| DeepSeek | api.deepseek.com | deepseek-chat / deepseek-reasoner | 性价比高、中文强 |
| 阿里 Qwen | dashscope（OpenAI 兼容） | qwen-plus / qwen-max | 中文强 |
| Moonshot Kimi | api.moonshot.cn | kimi-k2 / moonshot-v1 | 长上下文 |
| 智谱 GLM | open.bigmodel.cn | glm-4-plus | 中文 |
| Anthropic Claude | api.anthropic.com | claude-sonnet（经兼容层） | 推理强 |
| Google Gemini | generativelanguage（OpenAI 兼容） | gemini-2.x | 多模态 |
| 自建 Ollama / vLLM | 局域网地址 | 任意 | 仍走 OpenAI 兼容协议 |

- 统一抽象：`base_url + api_key + model`，SSE 流式；Rust 用 `reqwest` + `eventsource-stream`（或 `async-openai`，支持自定义 base_url 以适配各服务商）。
- 功能模板：翻译、润色、续写、扩写、总结、自定义 Prompt、多轮上下文（默认单轮，可配置）。

## TTS（文字 → 语音）—— **❄️ 已冻结为实验性（2026-08-29：代码保留、默认关闭、入口隐藏，不承诺）**

| 方案 | 类型 | 说明 | Rust 接入 | 评价 |
| --- | --- | --- | --- | --- |
| 系统 TTS | 系统 | SAPI5 / AVSpeechSynthesizer / espeak-ng | 平台 API | ❄️ 冻结（不实现） |
| Piper | 本地 | 离线神经 TTS，中文模型可用、延迟低 | `piper-rs` / 子进程 | ❄️ 冻结（不实现） |
| edge-tts | 在线 | 微软 Edge 神经音色（非官方接口），免费、音色好 | WS（Rust 已实现，2026-08-23 实机 OK） | ❄️ 已实现，默认关闭（实验性） |
| OpenAI TTS | 云端 | 音色自然、按字符计费 | HTTP | 可选 |
| OpenAI 兼容 `audio/speech` | 云端 | 复用 LLM base_url+key，音色自然 | HTTP JSON（`verba-tts::openai`，2026-08-23 已实现） | ❄️ 已实现，默认关闭（实验性） |
| audio.cpp（TTS） | 本地 | ggml 本地神经 TTS，模型族丰富 | audio.cpp 预编译包子进程 | 本地可选（后续） |
| Azure / 讯飞 | 云端 | 企业级、可定制音色 | HTTP | 可选 |

## 默认配置（v1 建议）

| 能力 | 默认 | 备选 | 说明 |
| --- | --- | --- | --- |
| OCR | rapid（本地 RapidOCR/PaddleOCR，原生 Rust，无需 Python） | 内部可经 CLI 覆盖 windows/mock | 设置页不暴露 provider；vision LLM 只用于 `//看图` 理解 |
| ASR | ❄️ 冻结为实验性，默认关闭 | mock | 入口隐藏，不承诺 |
| LLM | 无默认服务商，首次引导配置 | DeepSeek / OpenAI 兼容 | 纯远程，必须显式配置 |
| TTS | ❄️ 冻结为实验性，默认关闭 | mock | 入口隐藏，不承诺 |

## 多模态 vision（`//看图`）

- `//看图` 把「眼睛区域」（光标上方屏幕，见候选窗避让逻辑）直接发给当前配置的 LLM（OpenAI 兼容 `image_url` 内容块），例如 `gpt-4o-mini` / `qwen2.5-vl` / `GLM-4V`；模型复用 `llm_model` / `llm_base_url` / 同一个 API Key。**Windows / macOS 前端均已接入；Linux 前端尚未开始，共享 `verba-trigger` 截图/PNG API 已平台中立，前端落地后直接接线**。
- 设置页不暴露 vision 模型或眼睛模式：普通 `//` 的眼睛区域固定走内置 OCR，只有 `//看图` 显式走 LLM vision。
- 模型不支持图片时，daemon 把客户端拒绝（HTTP 400/422，或带 vision 关键词的 404/流错误）转成可执行提示：换用支持图片输入的模型，或改用 `//截图` 走内置 OCR；服务端原始错误一并展示。
- 与 OCR 的区别：vision 由 LLM 直接「理解 + 提取」，擅长版面、表格、图表与上下文；OCR 只做「文字识别」转文本。

## 降级与失败策略

- provider 失败 → 按优先级链降级（如本地模型未下载 → 系统能力 → 云端）。
- LLM 超时 / 网络错误 → 明确报错提示，不静默失败；支持取消（Esc）。
- 本地模型：首次使用按需下载并显示进度，存储于平台数据目录。
