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

## 下一步（daemon 集成）

- 新增 `verba-librime` crate（独立 workspace，动态加载 rime.dll），提供
  `translate(pinyin/wubi) -> Vec<Candidate>` 同步接口。
- verba-daemon 按 `config 引擎=builtin|rime` 切换：builtin 走 `verba-pinyin`，rime 走 librime。
- 候选融合沿用现有 `LlmCandidates` 协议：词库候选（builtin/rime）+ LLM 候选。
- 打包：rime.dll（x64 ~4MB）+ 数据目录（~10MB）随安装包分发；macOS/Linux 用各平台预编译包。
