# 构建与打包

> 更新：2026-08-22 · 适用于 M0 骨架与后续各平台前端。

## 环境要求

- Rust 工具链（stable，当前 1.97+），含 `rustfmt`、`clippy`。
- 平台前端额外依赖：
  - Windows：无（TSF 为系统内置）；打包用 Inno Setup 或 WiX。
  - macOS：Xcode Command Line Tools；发布签名需 Apple Developer 账号。
  - Linux：Fcitx5 dev（`fcitx5-dev`）、CMake、ECM、corrosion（Fcitx5 插件）；IBus / Wayland 后端走 Rust crate。
- AI 能力（按需）：
  - OCR：ONNX Runtime（`ort` crate 自动拉取或系统安装）。
  - ASR：whisper.cpp（`whisper-rs`，模型首次运行时下载）。
  - TTS：系统 TTS 零依赖；edge-tts / Piper 视实现选择。

## 构建核心与 CLI

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p verba-cli -- --help
```

## 平台前端构建（M1 起逐步补充）

### Windows 安装包（Inno Setup）
1. 构建产物：
   - 前端（DLL + 注册工具）：`cd frontends/windows/ime && cargo build --release`
   - daemon：根目录 `cargo build -p verba-daemon --release`
2. 安装 Inno Setup 6（`winget install JRSoftware.InnoSetup --scope user`）。
3. 编译：
   ```powershell
   cd frontends/windows/installer
   & "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe" verba-ime.iss
   ```
   产物：`frontends/windows/installer/output/verba-ime-setup.exe`（需管理员运行安装）。

- **Windows**：`cargo build -p verba-ime-windows`（TSF DLL）→ 注册脚本（regsvr32 / 安装器）→ Inno Setup 打包。
- **macOS**：Xcode 工程或 SPM 构建 `.appex` → 装入 `~/Library/Input Methods` → Developer ID 签名 + 公证。
- **Linux**：CMake + corrosion 构建 Fcitx5 插件 → `sudo make install` → `fcitx5 -r` 重启；IBus / Wayland 后端为独立二进制。

## CI（GitHub Actions）

三平台 matrix（windows-latest / macos-latest / ubuntu-latest）：
`cargo fmt --check` → `cargo clippy -D warnings` → `cargo test`。
本地可完整验证的仓库以本地门禁为准，CI 主要作为发布前强制门禁。

## 发布产物与签名

| 平台 | 产物 | 签名 / 公证 |
| --- | --- | --- |
| Windows | MSI / exe（Inno Setup） | EV 代码签名（可选）；SmartScreen 提示需证书 |
| macOS | dmg（内含 .app + .appex） | Developer ID + notarization（必需） |
| Linux | .deb / .rpm / AppImage | GPG 签名（可选） |

## 调试建议

- 用 `verba-cli` 驱动 core，无需装输入法即可验证状态机与 AI provider。
- 平台前端先用「临时英文 + 标点直输」冒烟，再逐步加 AI 链路。
- 记录每端手动验收清单（注册、上屏、preedit、候选、模式切换、权限弹窗）。