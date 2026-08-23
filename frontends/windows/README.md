# Windows 前端（TSF）

- 技术：Rust + `windows` crate（TSF 绑定），参考 [khiin-rs `windows/ime`](https://github.com/aiongg/khiin-rs/tree/main/windows) 与 imekit 的 TSF 实现。
- 形态：COM 服务 DLL，注册为系统输入法。
- 关键点：
  - 实现 `ITfTextInputProcessorEx` 等接口；STA 线程模型；回调快速返回，重活交给 daemon。
  - `ITfInputProcessorProfiles::Register` 注册 GUID + 语言栏按钮；IMM32 仅兼容回退。
  - 候选窗口：TSF `ITfCandidateListUIElement` 或自绘置顶窗口（见架构开放问题）。
- 安装：Inno Setup 打包，注册 COM + 输入法。
- 状态：M1/M5 已实机验收（LLM 直输 + 拼音/Rime 候选窗 + 融合）；多模态触发能力地基已就绪（见下）。

## 触发工具（verba-trigger）

`verba-trigger` 是 Windows 触发能力入口（与 DLL 同包构建），供手动验证与后续 TSF 热键接线复用：

- `verba-trigger shot [输出.bmp]`：截取主屏全屏（BitBlt → 32bpp top-down BMP，零依赖编码）。
- `verba-trigger region-shot [--rect x,y,w,h] [输出.bmp]`：选区截图（半透明遮罩拖选；Esc/右键取消；`--rect` 脚本化）。
- `verba-trigger region-ocr [--rect x,y,w,h] [输出.txt]`：选区 → daemon OCR。
- `verba-trigger ocr [输出.txt]`：截图 → daemon OCR（`config ocr_provider`：mock / windows）。
- `verba-trigger mic [秒=3] [输出.wav]`：麦克风录音（cpal → 16bit PCM WAV）。
- `verba-trigger asr [秒=3]`：录音 → daemon ASR（`config asr_provider`）。
- `verba-trigger tts <文本> [输出] [语音]`：TTS 合成存文件（`config tts_provider`）。
- `verba-trigger speak <文本> [语音]`：TTS 合成并播放（rodio，支持 MP3 / WAV）。

能力模块：`capture`（截图）、`record`（录音）、`play`（播放）均在 DLL crate 内，TSF 热键/命令已直接复用。

## 触发热键与命令（TSF 内）

- **Ctrl+Alt+O**：全屏截图 → daemon OCR（`config ocr_provider`）→ 识别文本上屏。
- **Ctrl+Alt+M**：麦克风录音 3s → daemon ASR（`config asr_provider`）→ 识别文本上屏。
- **`//朗读 <文本>`**：TTS 合成（`config tts_provider`）并播放，不落盘文本。
- **`//截图`**：同 Ctrl+Alt+O。**`//听写`**：同 Ctrl+Alt+M。

上屏/播放异步完成（后台线程采集 → 结果经 TSF 定时器提交），不会卡住按键处理。

> 部署：新 DLL 构建到新 target 目录（如 `target_dev15`），并把 HKCU CLSID `InprocServer32` 指向新 DLL；
> 切回旧版只需改回 `target_dev14`。重新打开输入法应用（如记事本）后生效。