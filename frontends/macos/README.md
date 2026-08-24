# macOS 前端（IMK）

- 技术：**全 Rust**（`objc2` + `objc2-input-method-kit`），薄壳 + 共享 Rust 核心（`verba-core`）+ daemon（`verba-daemon`）。
- 形态：`.app`（`Verba.app`），装入 `~/Library/Input Methods`，在系统设置「键盘 → 输入法」中启用。
- 能力（M2 对齐）：
  - 拼音组合 / 候选窗（数字键与候选窗点击选择，←/→ 翻页）
  - 英文与标点直输、退格 / Esc 取消组合、Enter 提交
  - `//` 进入 AI 模式 → LLM 流式 preedit → Enter 上屏（经 daemon）
- 关键点：
  - 基础输入**无需**辅助功能权限。
  - 麦克风：需 `NSMicrophoneUsageDescription`（TCC 弹窗）；截图 OCR：需屏幕录制权限（ScreenCaptureKit）。
- 打包：`.app` 内含 `verba-mac`（IMK 主程序）与 `verba-daemon`（Rust 核心），ad-hoc 签名；正式发布需 Developer ID 签名 + 公证。

## 构建与安装

```bash
cd frontends/macos/ime
scripts/package.sh
cp -R dist/Verba.app "$HOME/Library/Input Methods/"
# 然后到 系统设置 → 键盘 → 输入法，添加「拾言输入法」
```

开发期快速验证：

```bash
cargo check --manifest-path frontends/macos/ime/Cargo.toml   # 编译门禁
cargo test  --manifest-path frontends/macos/ime/Cargo.toml   # 按键分类 / 状态机单测
```

## 结构

- `src/imk.rs` — IMK 输入控制器：`inputText:key:modifiers:client:` 收键 → 状态机 → 上屏 / 标记文本 / 候选窗；LLM 流式经全局队列 + 主线程定时器。
- `src/ipc.rs` — daemon 定位与拉起（`VERBA_DAEMON_PATH` 或可执行文件同目录 `verba-daemon`）。
- `app/Info.plist` — IMK 注册元数据（`InputMethodServerControllerClass` / `ComponentInputModeDict` / `LSUIElement`）。
- `scripts/package.sh` — 组装 `dist/Verba.app` 并 ad-hoc 签名。

## 状态

- [x] IMK 控制器类注册与 `activateServer` / `deactivateServer`
- [x] 按键 → 状态机 → 上屏 / preedit / 候选窗（拼音 + AI 模式）
- [x] LLM 流式（`//` 触发，经 daemon）
- [x] `.app` 打包（含 daemon）+ ad-hoc 签名 + 安装脚本
- [ ] 真机交互验收（候选窗自动展示、输入法切换、权限弹窗等需 macOS 真机确认）
