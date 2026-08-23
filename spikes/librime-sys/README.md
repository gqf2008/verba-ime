# librime-sys Spike（M5 评估）

> 目的：验证「预编译 rime.dll 能否被 Rust 直接 FFI 调用，拼音整句 + 五笔方案是否可用」，
> 为「daemon 内可选引擎（config 引擎=rime|builtin）」决策提供依据。
> 结论（2026-08-23）：**可行** —— 拼音（luna_pinyin）与五笔（wubi86）均跑通，详见
> [docs/chinese-engine-evaluation.md](../docs/chinese-engine-evaluation.md) §7。

## 目录结构

```
spikes/librime-sys/
├── Cargo.toml          # 独立 workspace（不进主仓库 CI）
├── fetch-vendor.ps1    # 拉取第三方二进制/数据（rime.dll x64 + Rime 数据 + wubi86）
├── src/main.rs         # 动态 FFI：加载 rime.dll → 部署 → 拼音/五笔模拟输入
└── vendor/             # gitignore：rime.dll、rime_api.h、data/（luna_pinyin…wubi86）
```

## 运行

```powershell
cd spikes/librime-sys
pwsh fetch-vendor.ps1        # 首次/更新 vendor（需 7-Zip + 网络）
cargo run --release          # 或先把 vendor 加进 PATH：$env:PATH = "$PWD\vendor;$env:PATH"
```

## 已验证

| 项 | 结果 |
|---|---|
| 预编译 rime.dll（librime nightly msvc-x64）Rust FFI 加载 | ✅ 动态加载（LoadLibrary/GetProcAddress） |
| RimeInitialize + 首次部署（StartMaintenance/JoinMaintenanceThread） | ✅ 9 个方案编译成功 |
| 拼音 luna_pinyin：`nishishui` | ✅ 上屏「你是誰」（默认繁体；luna_pinyin_simp 为简体） |
| 拼音整句：`jintianwanshangchishenme` | ✅ 上屏「今天晚上喫什麼」 |
| 五笔 wubi86：`wqvb`（你=wq 好=vb） | ✅ 上屏「你好」 |
| octagram（n-gram 整句） | ⚠️ 未验证：Weasel/librime 预编译包未捆绑 octagram 数据，需另配 `octagram_data` |

## FFI 要点（踩坑记录）

1. **librime 1.17 的 `RimeSessionId` 是 `uintptr_t`（64 位）**，不是 `int`。
   Rust 侧必须用 `usize`；用 `i32` 会截断 session id → `RimeGetStatus`/`RimeSimulateKeySequence` 全 false。
2. **raw-dylib 在本机 GNU 工具链（x86_64-pc-windows-gnu）下不可靠**（返回垃圾值 + 退出访问违规），
   改用显式 `LoadLibraryW` + `GetProcAddress` 动态加载，稳定可复现。
3. Weasel 0.17.4 安装包里的 `rime.dll` 是 **x86（32 位）**；x64 需取 librime 官方 release 的
   `rime-<hash>-Windows-msvc-x64.7z`。
4. 默认 `default.yaml` 的 `schema_list` 不含 wubi86 → 需手动追加 `- schema: wubi86` 才会被部署编译。

## 后续（已落地，见主仓库）

- `crates/verba-librime`（主 workspace）：把本 spike 的 FFI 封装成库，提供 `RimeEngine::candidates()`。
- daemon `RimeCandidates` IPC + `config 引擎=builtin|rime` + `rime_schema`（luna_pinyin_simp/wubi86）。
- 前端 `engine=rime` 时输入停顿后请求并融合 Rime 候选。

## 整句基准（50 句日常对话，`cargo run -p verba-librime --example bench`）

| 引擎 | 首候选准确率 |
| --- | --- |
| 自研 verba-pinyin | 3/50（6%） |
| Rime luna_pinyin_simp（无 octagram） | 42/50（84%） |
| Rime + octagram（essay 模型） | 37/50（74%） |

结论：整句输入 librime 显著更优；octagram（essay 语料）对日常对话有害，默认不启用。

### 配 octagram（如需复现 74% 那列）
1. `git clone --depth 1 --branch hans https://github.com/lotem/rime-octagram-data.git <tmp>`
   （LFS，~50MB），把 `*.gram` 拷到 `vendor/user_data/`；hant 分支同理（默认 luna_pinyin 用）。
2. `vendor/data/grammar.yaml` 已在仓库脚本可复现；`vendor/data/luna_pinyin.custom.yaml`：
   `patch: __include: grammar:/hans`（简体模型）。
3. 清空 `vendor/user_data/build` 后重跑 spike/bench。
4. 不启用：删掉 `grammar.yaml` + `luna_pinyin.custom.yaml`（schema 的 `grammar:/hant?` 为可选）。
