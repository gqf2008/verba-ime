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
   - 前端（DLL + 注册/触发工具）：`cd frontends/windows/ime && cargo build --release`
   - daemon + 设置面板：根目录 `cargo build -p verba-daemon --release && cargo build -p verba-settings --release`
2. 获取 Rime 运行时：`pwsh scripts/fetch-rime-vendor.ps1`（见下节）。
3. 安装 Inno Setup 6（`winget install JRSoftware.InnoSetup --scope user`）。
4. 编译（`/DMyAppVersion` 可选，默认 0.1.0；发布流水线注入根 Cargo.toml 版本）：
   ```powershell
   cd frontends/windows/installer
   & "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe" /DMyAppVersion=0.1.0 verba-ime.iss
   ```
   产物：`frontends/windows/installer/output/verba-ime-setup-<版本>.exe`（需管理员运行安装）。

### Rime 引擎（单引擎，librime）

中文候选由 librime 提供（daemon 内动态加载 rime.dll，**唯一引擎**，无引擎开关）。构建与部署：

1. 获取第三方二进制/数据（统一输出到仓库根 `vendor/rime/`，gitignored，CI 与本地共用）：
   ```powershell
   pwsh scripts/fetch-rime-vendor.ps1   # Windows: rime.dll + data/ + wubi86
   bash scripts/fetch-rime-vendor.sh    # macOS:   librime.dylib + data/ + wubi86
   ```
   脚本从 librime 1.17.0 官方发行版取引擎库（资产名按 tag+名称动态解析），从
   Weasel 0.17.4 安装包取 Rime 数据（含 luna_pinyin_simp、opencc/），并补 wubi86 方案。
   开发期 spike 版本（nightly + `spikes/librime-sys/` 目录）见 `spikes/librime-sys/README.md`。
   结构：
   ```
   vendor/rime/(rime.dll | librime.dylib)
   vendor/rime/data/            # schema/dict + opencc/
   ```
   `scripts/package.sh` 会把 `vendor/rime/` 打进 `Verba.app/Contents/MacOS/rime/`；
   Windows 安装包（Inno Setup）把 `vendor/rime/` 装到 `{app}\rime\`。
2. 部署到 daemon 同目录 `rime/`（daemon 默认从此加载；Windows `rime.dll` / macOS `librime.dylib`）：
   ```
   <daemon 同目录>/rime/(rime.dll | librime.dylib)
   <daemon 同目录>/rime/data/            # schema/dict + opencc/
   <daemon 同目录>/rime/user_data/       # 首次查询自动部署生成
   ```
   或指定 `VERBA_RIME_DLL` / `VERBA_RIME_DYLIB` / `VERBA_RIME_SHARED` / `VERBA_RIME_USER` 环境变量。
3. 配置方案：`verba-cli config set rime_schema=luna_pinyin_simp`
   （五笔：`rime_schema=wubi86`）。
4. 验证：`verba-cli rime nishishui luna_pinyin`（→ 你是谁）、`verba-cli rime wqvb wubi86`（→ 你好）。
   > 首次查询会部署 schema/词典（数秒）；整句基准见 [chinese-engine-evaluation.md](chinese-engine-evaluation.md) §8。

### macOS 真机验证记录（2026-08-24）

- **brew 的 `librime` 可用，但要走现代 API**：Homebrew `librime`（1.17）只导出 `rime_get_api`
  （C++ 修饰名导出其余符号），因此 `verba-librime` 统一用 `rime_get_api()` 返回的 `RimeApi` 结构体，
  不再逐个 dlsym 单个符号（Windows rime.dll 同样兼容）。
- **部署数据**：`data/minimal`（luna_pinyin）+ `rime-wubi`（wubi86）+ `opencc` 数据
  （brew `share/opencc/*`）放入 shared 目录；`default.yaml` 的 `schema_list` 需含 `wubi86`。
- **运行**：`VERBA_RIME_DYLIB=/opt/homebrew/lib/librime.dylib VERBA_RIME_SHARED=... VERBA_RIME_USER=... verba-daemon`
- **已验证**（本机 arm64 + brew librime 1.17）：
  - `verba-cli rime nishishui luna_pinyin` → `你是誰 / 你是 / 妳是 …`
  - `verba-cli rime wqvb wubi86` → `你好 / 您好`
- **注意**：librime 为进程级单例，`verba-librime` 加载后**不 dlclose**（泄露句柄）——退出时
  dlclose 会 SIGSEGV（Squirrel/Weasel 同样从不卸载）；`finalize()` 在 drop 时调用。

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
