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
4. 编译（`/DMyAppVersion` 可选，默认 0.2.4；发布流水线注入根 Cargo.toml 版本）：
   ```powershell
   cd frontends/windows/installer
   & "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe" /DMyAppVersion=0.2.4 verba-ime.iss
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
2. 部署到 daemon 同目录 `rime/`（daemon 默认从此加载**引擎库与共享数据**；Windows `rime.dll` / macOS `librime.dylib`）：
   ```
   <daemon 同目录>/rime/(rime.dll | librime.dylib)
   <daemon 同目录>/rime/data/            # schema/dict + opencc/（只读共享数据）
   ```
   **`user_data`（首次查询部署生成的用户词库/编译产物）默认落用户数据目录**，
   不在 daemon 同目录：安装态下 `C:\Program Files\Verba` / `Verba.app` 包内标准用户
   不可写（会 502），macOS 管理员可写又会改动已签名 bundle 破坏 seal。实际位置
   （`ProjectDirs("dev","verba","Verba")` 的 `data_dir()/rime`）：
   - Windows：`%APPDATA%\verba\Verba\data\rime\`
   - macOS：`~/Library/Application Support/dev.verba.Verba/rime/`
   - Linux：`~/.local/share/verba/rime/`

   或指定 `VERBA_RIME_DLL` / `VERBA_RIME_DYLIB` / `VERBA_RIME_SHARED` / `VERBA_RIME_USER` 环境变量整体覆盖三要素。
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
- **macOS**：`frontends/macos/ime/scripts/package.sh` 构建全 Rust IMK `.app`（`dist/Verba.app`，含 `verba-mac` / `verba-daemon` / `verba-register`，ad-hoc 签名）。
  - **发布 DMG 一键安装**：双击「安装.command」→ 拷贝到 `~/Library/Input Methods`（用户级，无需管理员）→ `verba-register` 走 TIS C API 注册并启用输入源（系统弹一次确认，macOS 26 已验证）。卸载 = 删除 `~/Library/Input Methods/Verba.app`。
  - **手动安装**：`cp -R dist/Verba.app "$HOME/Library/Input Methods/"` → 运行 `Verba.app/Contents/MacOS/verba-register`（或系统设置「键盘 → 输入法」手动启用）；`verba-register --list` 可只读查看已注册输入源（CI 冒烟同款）。
  - 正式发布需 Developer ID 签名 + 公证。
- **Linux**：CMake + corrosion 构建 Fcitx5 插件 → `sudo make install` → `fcitx5 -r` 重启；IBus / Wayland 后端为独立二进制。

## CI（GitHub Actions）

三平台 matrix（windows-latest / macos-latest / ubuntu-latest）：
`cargo fmt --check` → `cargo clippy -D warnings` → `cargo test`。
本地可完整验证的仓库以本地门禁为准，CI 主要作为发布前强制门禁。

## 发布产物与签名（M6）

| 平台 | 产物 | 签名 / 公证 |
| --- | --- | --- |
| Windows | `verba-ime-setup-<版本>.exe`（Inno Setup） | Authenticode 代码签名（可选，`WIN_SIGN_PFX` 配置时启用）；未签名 SmartScreen 会提示 |
| macOS | `Verba-<版本>.dmg`（内含 Verba.app + Applications 快捷方式） | Developer ID + notarization + staple（必需，`APPLE_*` secrets 配置时启用）；未配置时产出未签名 DMG（仅 dry-run） |

### 发布流程（`.github/workflows/release.yml`）

打 tag `v*`（如 `git tag v0.2.4 && git push origin v0.2.4`）自动触发；也可 `workflow_dispatch` 干跑（只出 artifact，不发 Release）：

1. **macOS job**（macos-14，Apple Silicon）：拉取 Rime vendor → `package.sh` 组装（版本注入 + Rime 捆绑）→ 逐二进制 + .app 签名（hardened runtime + timestamp）→ notarytool 公证 + staple → Rime 冒烟 → 打包 DMG + 签名 + 公证 + staple
2. **Windows job**（windows-latest，MSVC）：构建 workspace + 前端 → PE 子系统守卫（daemon 必须 GUI 子系统，防控制台回归）→ Inno Setup 打包（`/DMyAppVersion` 注入）→ Rime 冒烟 → 可选 signtool 签名
3. **release job**（仅 tag 触发）：下载两平台产物 → SHA256SUMS → 自动生成 release notes（前置「仅 Apple Silicon」提示）→ 发布 GitHub Release

### secrets 配置（一次性，值复用 abb/ossfs 同一套凭证）

**推荐脚本化**（自动从钥匙串导出含私钥的 P12 并逐项设置，交互输密码不落盘；自动化场景可用
`VERBA_P12_PASSWORD` / `VERBA_APPLE_ID` / `VERBA_APPLE_APP_PASSWORD` 环境变量覆盖输入）：

```bash
bash scripts/setup-release-secrets.sh            # 默认 gqf2008/verba-ime
```

手动等价命令（注意 P12 必须带私钥——`-t identities` 而非 `-t certs`，后者 CI 会报找不到私钥；
`-P '<密码>'` 会进 shell 历史，用完删除临时文件）：

```bash
security export -k ~/Library/Keychains/login.keychain-db -t identities -f pkcs12 -P '<密码>' -o /tmp/verba-cert.p12 "Developer ID Application: <姓名> (<TEAM_ID>)"
gh secret set APPLE_CERT_P12 -R gqf2008/verba-ime --body "$(base64 < /tmp/verba-cert.p12)"
rm -f /tmp/verba-cert.p12
```

- `APPLE_CERT_P12`：Developer ID Application 证书 + 私钥的 PKCS12 base64
- `APPLE_CERT_PASSWORD`：P12 导出密码；`APPLE_TEAM_ID` / `APPLE_ID` / `APPLE_APP_PASSWORD`：Apple 账号与 App 专用密码
- `WIN_SIGN_PFX`（可选）：Windows 代码签名证书 base64 + `WIN_SIGN_PASSWORD`

### 产物校验（发布前）

- macOS：`codesign -dv --strict`、`spctl -a -t open --context context:primary-signature`、`stapler validate`、逐个二进制 `codesign -dvv | grep Timestamp`
- Windows：PE 子系统断言（流水线内自动）、安装后切换输入法无控制台、`verba-cli rime nishishui` 出词

## 调试建议

- 用 `verba-cli` 驱动 core，无需装输入法即可验证状态机与 AI provider。
- 平台前端先用「临时英文 + 标点直输」冒烟，再逐步加 AI 链路。
- 记录每端手动验收清单（注册、上屏、preedit、候选、模式切换、权限弹窗）。
