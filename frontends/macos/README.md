# macOS 前端（IMK）

- 技术：**全 Rust**（`objc2` + `objc2-input-method-kit`），薄壳 + 共享 Rust 核心（`verba-core`）+ daemon（`verba-daemon`）。
- 形态：`.app`（`Verba.app`），装入 `~/Library/Input Methods`；`verba-register` 自动写 `com.apple.inputsources` 白名单并启用，无需手动添加。
- 能力（M2 对齐）：
  - 拼音组合 / 候选窗（数字键与候选窗点击选择，←/→ 翻页）
  - 英文与标点直输、退格 / Esc 取消组合、Enter 提交
  - `//` 进入 AI 模式 → LLM 流式（组合持短状态串「✨ 生成中…」，结果截断显示
    在候选面板浮层，#89 一期；多行自绘浮层列二期）→ Enter/空格/`1` 上屏全文、
    `r` 重试、`e` 改提示词（经 daemon）
  - `//` 多模态命令：`//朗读`（TTS 播放）、`//短语`（配置短语直插）、`//截图`/
    `//听写`（verba-trigger ocr/asr → OCR 预览管线）；`//看图` 一期回退普通生成
  - OCR 结果预览期间继续打字：先提交识别文本，再继续处理当前键；Enter/空格/1
    显式确认，Esc 取消
- 关键点：
  - 基础输入**无需**辅助功能权限。
  - 麦克风：需 `NSMicrophoneUsageDescription`（TCC 弹窗）；截图 OCR：需屏幕录制权限（ScreenCaptureKit）。
- 打包：`.app` 内含 `verba-mac`（IMK 主程序）、`verba-daemon`（Rust 核心）与 `verba-register`（TIS 注册/启用）；`verba-register` 会写 `com.apple.inputsources` 第三方输入源白名单并刷新 TextInputMenuAgent，无需用户手动添加。正式发布需 Developer ID 签名 + 公证。
- PKG 系统级安装：`scripts/package-pkg.sh` 的 postinstall 不直接调用 CLI，而是经 LaunchServices 在用户会话启动 `verba-mac --register` 短命 helper；helper 调用同 bundle 的 `verba-register`，失败会返回非零并保留日志，避免 package_script_service 沙盒导致假成功。

## 构建与安装

```bash
cd frontends/macos/ime
scripts/package.sh
cp -R dist/Verba.app "$HOME/Library/Input Methods/"
# 然后运行 verba-register（安装脚本会自动执行；无需手动到系统设置添加）
```

系统级 PKG：

```bash
scripts/package-pkg.sh
# 双击 dist/Verba-<版本>.pkg；postinstall 会在 console 用户会话内自动注册/启用
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
- [x] `.app` 打包（含 daemon + verba-settings 设置面板）+ ad-hoc 签名 + 安装脚本
- [x] 输入法菜单「设置…」入口（打开设置面板）
- [ ] 真机交互验收（候选窗自动展示、输入法切换、权限弹窗等需 macOS 真机确认）
