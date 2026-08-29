# macOS 手动验收清单（M6 · IMK）

> 目标：安装/启用 → 直输 → 候选窗 → // AI 流式 → OCR → 冻结能力负向验证。
> 口径：OCR=正式能力；ASR/TTS=实验性冻结（默认关闭、入口隐藏，M6 不验收效果）。
> 前置：`frontends/macos/ime` 构建 → `scripts/package.sh` → 装入 `~/Library/Input Methods` → 系统设置→键盘→输入法启用「拾言输入法」。

## 安装与启用
- [ ] Verba.app 装入 ~/Library/Input Methods，系统设置可见「拾言输入法」
- [ ] 切换输入法后 IMK 控制器激活（可输入）

## 直输
- [ ] 英文/数字/标点直输上屏（无残留 preedit）
- [ ] 退格删除、Esc 取消组合、Enter 提交

## 候选窗（Rime 单引擎）
- [ ] 拼音组合 → 候选窗自动展示（数字键/点击选择）
- [ ] ←/→ 翻页；主题（light/dark）与布局配置生效
- [ ] 中文候选上屏正确

## // AI 流式（LLM 核心链路）
- [ ] 输入 // → AI 模式 → 提示词 → Enter → 流式 preedit → Enter 上屏
- [ ] 流式中 Esc 取消无残留；快速流切换旧流不混入（5 轮连测）

## OCR（正式能力，P0）
- [ ] //截图 或截图入口 → 选区/屏幕捕获 → OCR 结果上屏（需屏幕录制权限 ScreenCaptureKit）
- [ ] 权限弹窗：拒绝→重试、授权→可用；无选区取消无残留

## 冻结能力负向验证（ASR/TTS）
- [ ] 设置中 ASR/TTS 默认关闭；入口隐藏（菜单/快捷键不可见或置灰）
- [ ] 触发热键不产生录音/合成行为（不承诺、不验收效果）

## 权限与发布
- [ ] 麦克风 TCC 弹窗（NSMicrophoneUsageDescription）行为确认
- [ ] Developer ID 签名 + 公证（发布前置，需证书）

## 判定
- P0/P1 清零、CI 全绿 → macOS IMK 验收通过