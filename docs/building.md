# 构建与打包

> 更新：2026-08-22 · 适用于 M0 骨架与后续各平台前端。

## 环境要求

- Rust 工具链（stable，当前 1.97+），含 `rustfmt`、`clippy`。
- 平台前端额外依赖：
  - Windows：**须 MSVC Build Tools + `x86_64-pc-windows-msvc` target**（Rust 仅为该 target 提供 `ort`/onnxruntime 预编译，因此整个项目用 MSVC 构建）；打包用 Inno Setup 或 WiX。
  - macOS：Xcode Command Line Tools；发布签名需 Apple Developer 账号。
  - Linux：Fcitx5 dev（`fcitx5-dev`）、CMake、ECM、corrosion（Fcitx5 插件）；IBus / Wayland 后端走 Rust crate。
- AI 能力（按需）：
  - OCR：ONNX Runtime（`ort` crate 自动拉取或系统安装）。
  - ASR：默认在线（OpenAI 兼容 `audio/transcriptions`，复用 LLM base_url+key）；whisper.cpp / audio.cpp 本地可选。
  - TTS：默认在线（edge-tts / OpenAI 兼容 `audio/speech`）；系统 TTS / Piper / audio.cpp 本地可选。

## 构建核心与 CLI

```powershell
# Windows：先进 MSVC 环境再构建（RapidOCR/ort 需要 x86_64-pc-windows-msvc）
scripts\build-msvc.cmd build --workspace --target x86_64-pc-windows-msvc
scripts\build-msvc.cmd test --workspace --target x86_64-pc-windows-msvc
cargo fmt --all -- --check
scripts\build-msvc.cmd clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings
scripts\build-msvc.cmd run -p verba-cli -- --help
```

## 设置面板（apps/settings，Slint 1.17）

- Slint 版本线说明：crates.io 无 0.17，用户所说的 `slint-0.17.x` 即 `1.17.x`，固定 `=1.17.1`。
- 运行：`cargo run -p verba-settings`（需 daemon 在跑：`verba-cli daemon`）。
- 密钥经 IPC `ApiKeySet` 写系统密钥库（需 keyring 平台后端已启用，见 workspace Cargo.toml）。

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

### Rime 引擎（单引擎，librime）

中文候选由 librime 提供（daemon 内动态加载 rime.dll，**唯一引擎**，无引擎开关）。构建与部署：

1. 获取第三方二进制/数据（rime.dll x64 + Rime 数据 + wubi86 + opencc）：
   ```powershell
   pwsh spikes/librime-sys/fetch-vendor.ps1   # 输出到 spikes/librime-sys/vendor/
   ```
2. 部署到 daemon 同目录 `rime/`（daemon 默认从此加载）：
   ```
   <daemon 同目录>/rime/rime.dll
   <daemon 同目录>/rime/data/            # schema/dict + opencc/
   <daemon 同目录>/rime/user_data/       # 首次查询自动部署生成
   ```
   或指定 `VERBA_RIME_DLL` / `VERBA_RIME_SHARED` / `VERBA_RIME_USER` 环境变量。
3. 配置方案：`verba-cli config set rime_schema=luna_pinyin_simp`
   （五笔：`rime_schema=wubi86`）。
4. 验证：`verba-cli rime nishishui`（→ 你是谁）、`verba-cli rime wqvb wubi86`（→ 你好）。
   > 首次查询会部署 schema/词典（数秒）；整句基准见 [chinese-engine-evaluation.md](chinese-engine-evaluation.md) §8。

- **Windows**：`cargo build -p verba-ime-windows`（TSF DLL）→ 注册脚本（regsvr32 / 安装器）→ Inno Setup 打包。
- **macOS**：`frontends/macos/ime/scripts/package.sh` 构建全 Rust IMK `.app`（`dist/Verba.app`，含 `verba-mac` 与 `verba-daemon`，ad-hoc 签名）
  → `cp -R dist/Verba.app "$HOME/Library/Input Methods/"` → 系统设置「键盘 → 输入法」启用；正式发布需 Developer ID 签名 + 公证。
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
