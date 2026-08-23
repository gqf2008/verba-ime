# AI 服务商矩阵（OCR / ASR / LLM / TTS）

> 更新：2026-08-22 · 原则：**本地优先（隐私 + 免费 + 离线），云端可插拔**；每个能力一个 trait、多个 provider，运行时按配置选择并支持降级。
> 关联：[架构设计](architecture.md) §6、[IPC 协议](protocol.md)。

## OCR（图片 / 截图 → 文字）

| 方案 | 类型 | 说明 | Rust 接入 | 评价 |
| --- | --- | --- | --- | --- |
| RapidOCR / PaddleOCR（PP-OCRv4） | 本地 | 中文最强开源 OCR，det + rec + cls 小模型约 10-20MB | ONNX Runtime（`ort` crate）；本机 `x86_64-pc-windows-gnu`（无 MSVC）无 `ort` 预编译，故原生 Rust ONNX（`ort` + `rapidocr-core`，PP-OCRv5 中文 mobile，模型自动下载到 data/models-rapidocr；需 MSVC 工具链编译，`ocr_provider=rapid`，2026-08-23 实机 OK，无 Python） | ✅ 默认本地方案 |
| PaddleOCR-VL / DeepSeek-OCR | 本地 | 新式 VLM OCR，效果好、模型大（GB 级） | `ort` / candle | 备选，视硬件 |
| Windows.Media.Ocr | 系统 | Win10+ 内置 OCR，多语言 | `windows` crate | ✅ Windows 快速路径 |
| Apple Vision（VNRecognizeTextRequest） | 系统 | macOS 原生 OCR，质量好 | Swift / ObjC（或 objc2） | ✅ macOS 快速路径 |
| Tesseract（`leptess`） | 本地 | 老牌 OCR | `leptess` crate | 中文一般，兜底 |
| 百度 / 腾讯 / 阿里 / Google Vision | 云端 | 准确率高、按量计费 | HTTP | 可选 |

## ASR（语音 → 文字）

| 方案 | 类型 | 说明 | Rust 接入 | 评价 |
| --- | --- | --- | --- | --- |
| whisper.cpp | 本地 | 开源最强本地 ASR，base / small 中英可用，可流式 | `whisper-rs` | ✅ 默认本地方案 |
| 系统听写（Windows Speech / macOS Dictation） | 系统 | 平台级，受系统语言限制 | 平台 API | 备选 |
| 讯飞 / 百度 / 腾讯 | 云端 | 中文流式强、低延迟 | HTTP / WS | 可选 |
| OpenAI Whisper API | 云端 | 质量高、按分钟计费、非流式 | HTTP | 可选 |
| OpenAI 兼容 `audio/transcriptions` | 云端 | 复用 LLM base_url+key，whisper 系列模型 | HTTP multipart（`verba-asr::openai`，2026-08-23 已实现） | ✅ 在线默认（`asr_provider=openai`） |
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

## TTS（文字 → 语音）

| 方案 | 类型 | 说明 | Rust 接入 | 评价 |
| --- | --- | --- | --- | --- |
| 系统 TTS | 系统 | SAPI5 / AVSpeechSynthesizer / espeak-ng | 平台 API | ✅ 零成本离线兜底 |
| Piper | 本地 | 离线神经 TTS，中文模型可用、延迟低 | `piper-rs` / 子进程 | ✅ 推荐离线神经音色 |
| edge-tts | 在线 | 微软 Edge 神经音色（非官方接口），免费、音色好 | WS（Rust 已实现，2026-08-23 实机 OK） | ✅ 推荐在线免费音色 |
| OpenAI TTS | 云端 | 音色自然、按字符计费 | HTTP | 可选 |
| OpenAI 兼容 `audio/speech` | 云端 | 复用 LLM base_url+key，音色自然 | HTTP JSON（`verba-tts::openai`，2026-08-23 已实现） | ✅ 在线可选（`tts_provider=openai`） |
| audio.cpp（TTS） | 本地 | ggml 本地神经 TTS，模型族丰富 | audio.cpp 预编译包子进程 | 本地可选（后续） |
| Azure / 讯飞 | 云端 | 企业级、可定制音色 | HTTP | 可选 |

## 默认配置（v1 建议）

| 能力 | 默认 | 备选 | 说明 |
| --- | --- | --- | --- |
| OCR | rapid（本地 RapidOCR/PaddleOCR，经 Python `rapidocr_onnxruntime`） | platform 原生（Windows.Media.Ocr）/ vision LLM | 云端仅当用户配置 key |
| ASR | openai（在线，OpenAI 兼容 `audio/transcriptions`） | mock / whisper.cpp / audio.cpp | 无 key 时回退 mock |
| LLM | 无默认服务商，首次引导配置 | DeepSeek / OpenAI 兼容 | 纯远程，必须显式配置 |
| TTS | edge（在线，微软音色）/ openai（OpenAI 兼容） | mock / Piper / audio.cpp | 无网络时本地兜底 |

## 多模态 vision（`//看图` / 眼睛直读图像）

- `//看图` 或 `eye_mode=vision` 时，把「眼睛区域」（光标上方屏幕，见候选窗避让逻辑）直接发给多模态 LLM（OpenAI 兼容 `image_url` 内容块），例如 `gpt-4o-mini` / `qwen2.5-vl` / `GLM-4V`。
- 配置：`llm_vision_model`（为空则复用 `llm_model`，需模型支持 vision）、`eye_mode=ocr|vision`。
- 与 OCR 的区别：vision 由 LLM 直接「理解 + 提取」，擅长版面、表格、图表与上下文；OCR 只做「文字识别」转文本。

## 降级与失败策略

- provider 失败 → 按优先级链降级（如本地模型未下载 → 系统能力 → 云端）。
- LLM 超时 / 网络错误 → 明确报错提示，不静默失败；支持取消（Esc）。
- 本地模型：首次使用按需下载并显示进度，存储于平台数据目录。
