# verba-ime GitHub issues 归档（关闭前导出）
> 导出时间：2026-09-17T17:08:28　源：github.com/gqf2008/verba-ime
> GitHub 已改为只读镜像（issue/wiki/projects/discussions 关闭），协作记录以 walgit 为准。

共 55 个 issue（含已关闭），134 条评论。

---

## #5 docs: 文档漂移修复——单引擎化后 README/roadmap/评估文档旧表述收口（批次）
- 状态：closed　创建：2026-08-25　标签：—

## 现状

2026-08-24 单引擎化（Rime）后，多份文档仍残留旧表述（`verba-pinyin`、`config engine=builtin|rime`、LLM 候选融合、macOS Swift 前端、三平台「规划中」等），与代码/CI 现状矛盾。

## 目标

文档与当前代码状态一致，README 与 docs/ 交叉引用不再漂移。

## 批次清单

- [ ] README：支持平台表状态（Windows TSF 已实机验收 / macOS IMK 已实现打包 / Linux 未开始·低优先）
- [ ] README：快速开始「目前仅有核心骨架」过时说明；架构图 macOS 前端 (Swift) → (Rust)
- [ ] README：顶部项目状态行与路线图摘要对齐（M0-M5 完成度、「候选融合」表述、M6 未开始）
- [ ] roadmap：M5 里程碑行与检查项「内置 verba-pinyin」「config 引擎=builtin|rime」单引擎化收口
- [ ] roadmap：M0 标题与检查项（CI 三平台矩阵绿 / core / protos-ipc / cli 均已完成）
- [ ] roadmap：变更记录「目录残留待清」清理（目录已删）
- [ ] roadmap：macOS 多客户端会话语义限制移入「风险与开放问题」
- [ ] chinese-engine-evaluation.md：§4 决策建议标注已被 2026-08-24 单引擎化决策取代
- [ ] frontends/README.md：macOS IMK「Swift 薄壳 + Rust 核心」→ 全 Rust（objc2）

## 验收

- 全库检索无 `engine=builtin|rime`、`内置 verba-pinyin`（作为现状描述）、macOS Swift 前端等过期表述（历史记录/标注除外）
- 仅文档改动，代码零改动；`cargo fmt --all -- --check` 不受影响

---

## #7 🔄 [处理中][wt-console-fix] fix(windows): 切换输入法弹出控制台窗口
- 状态：closed　创建：2026-08-25　标签：—

## 现状

Windows 切换输入法（TSF 激活）时弹出黑色控制台窗口。

## 根因

切换输入法 → `text_service.rs:217` prewarm_daemon → `ipc::ensure_daemon` 用裸 `Command::spawn()`（`frontends/windows/ime/src/ipc.rs:38`）拉起控制台子系统的 `verba-daemon.exe`，被 GUI 宿主 spawn 时新建控制台窗口。对照：trigger 的两个 spawn 点（text_service.rs:1191/1237）已有 `creation_flags(0x08000000)`；`apps/settings/src/main.rs:6` 已有 `windows_subsystem` 属性。

## 目标 / 验收

切换输入法（激活/停用）绝不允许出现控制台窗口；daemon 启动日志仍可查（TeeLog → verba-daemon.log）。

## 方案

1. `crates/verba-daemon/src/main.rs` 顶部加 `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`（debug 保留控制台便于 `verba-cli daemon` 调试）
2. `frontends/windows/ime/src/ipc.rs` 加 `use std::os::windows::process::CommandExt;`，spawn 加 `.creation_flags(0x08000000)`（CREATE_NO_WINDOW）
3. release.yml windows job 加 PE 子系统守卫（subsystem==2 GUI），防回归

---

## #9 🔄 [处理中][wt-m6-packaging] chore(release): 打包工程补全（Rime vendor/ISS/package.sh 版本联动）
- 状态：closed　创建：2026-08-25　标签：—

## 现状

M6 发布前置的打包工程缺口（均为同类「发布产物完整性」问题，批次处理）：

1. **Rime 引擎 vendor 缺失**：`vendor/rime/` 不存在（git 未跟踪）；安装包/CI 发布无 rime.dll/librime.dylib + data。librime 官方 1.17.0 release 资产已确认（macOS-universal.tar.bz2 / Windows-msvc-x64.7z），数据需从 Weasel 安装包取（spike 已验证）
2. **Windows 安装包不全**：verba-ime.iss 只打包 DLL+reg+daemon，缺 verba-trigger.exe（IME 运行时依赖）、verba-settings.exe、vendor/rime；版本号硬编码无 /D 注入；[Icons]「设置」错误指向 verba-reg.exe 且带「开发占位」
3. **macOS package.sh 版本硬编码**：Info.plist 0.1.0 无 Cargo 联动；rime 捆绑分支存在但从未生效（vendor 缺失），缺完整性校验
4. **.gitignore / docs**：无 /vendor/ 忽略；docs/building.md Rime 一节未引获取脚本

## 目标 / 验收

- `scripts/fetch-rime-vendor.ps1`（Windows）与 `scripts/fetch-rime-vendor.sh`（macOS）可一键产出 vendor/rime（引擎库 + data + wubi86），CI 与本地共用
- verba-ime.iss 支持 `/DMyAppVersion` 注入，打包 6 组件（dll/reg/trigger/daemon/settings/rime）
- package.sh 以根 Cargo.toml 为版本源注入 Info.plist；vendor/rime 存在但不完整时报错
- CI 新增 vendor job（路径过滤）验证两脚本

## 批次清单

- [ ] scripts/fetch-rime-vendor.ps1（新）
- [ ] scripts/fetch-rime-vendor.sh（新）
- [ ] frontends/windows/installer/verba-ime.iss 补全（/D 注入 + 3 组件 + Icons 修正）
- [ ] frontends/macos/ime/scripts/package.sh 版本注入 + rime 校验
- [ ] .gitignore 加 /vendor/
- [ ] docs/building.md Rime 一节更新
- [ ] ci.yml 加 vendor job（paths 过滤）

---

## #11 🔄 [处理中][wt-m6-release] feat(release): M6 发布流水线（tag v* → 签名/公证/DMG/安装包/Release）
- 状态：closed　创建：2026-08-25　标签：—

## 现状

M6 发布流水线缺失。打包工程（Rime vendor 获取 / ISS 补全 / package.sh 版本联动）已合入（#9），但无 tag 触发的发布 workflow；verba-ime 仓库无 Apple 签名/公证 secrets（需配置，值复用 abb/ossfs 同一套凭证）。

## 目标 / 验收

- 参照 abb/ossfs 发版模式：push tag v*（+ workflow_dispatch 干跑）→ macOS 签名/公证/DMG + Windows Inno Setup 安装包 → SHA256SUMS + GitHub Release
- secrets 缺失时优雅降级（未签名产物 + warning；tag 分支强制失败防残缺 release）
- Windows job 含 PE 子系统守卫（subsystem==2，防控制台回归）
- 产物含 Rime 引擎（vendor/rime 捆绑）并做 Rime 冒烟断言

## 批次清单

- [ ] .github/workflows/release.yml（macos / windows / release 三 job）
- [ ] docs/building.md 补「发布」一节（流程 + secrets 清单 + 配置命令）
- [ ] secrets 配置（用户操作：APPLE_CERT_P12 / APPLE_CERT_PASSWORD / APPLE_TEAM_ID / APPLE_ID / APPLE_APP_PASSWORD）

## 验收

- workflow_dispatch 干跑：macos/windows 产出 artifact；tag 分支不发 release
- macOS 产物：codesign -dv --strict、spctl、stapler validate、逐二进制 Timestamp、Rime 冒烟出词
- Windows 产物：PE subsystem==2、安装后 6 组件、无控制台
- 打 tag v0.1.0 后 release job 产出正式 Release（SHA256SUMS）

---

## #13 🔄 [处理中][wt-secrets-script] chore(release): secrets 配置脚本化（含私钥 P12 导出）
- 状态：closed　创建：2026-08-25　标签：—

## 现状

发布 secrets 配置（Apple 签名/公证 5 项）是逐仓库手动操作，且有两处易踩坑：`security export -t certs` 不含私钥（CI set-key-partition-list 报 item not found）、`gh secret set` 交互粘贴易粘空值（命令替换失败也显示成功）。docs/building.md 只有文字命令，无脚本化入口。

## 目标 / 验收

- `scripts/setup-release-secrets.sh` 一条命令完成：本机钥匙串导出 P12（**含私钥**，`-t identities`）→ base64 → 设置 5 个 GitHub secrets
- 密码/账号交互输入，不落盘、不进 shell 历史；临时 P12 trap 清理
- 完成后 `gh secret list` 核对清单
- docs/building.md 改为引用脚本（保留手动命令说明）

---

## #15 🔄 [处理中][wt-vendor-fix] fix(release): 干跑暴露——ps1 Copy-Item 容器冲突 + secrets 占位符重设
- 状态：closed　创建：2026-08-25　标签：—

## 现状

发布干跑（32839272335）暴露两个问题：

1. **fetch-rime-vendor.ps1 第 47 行失败**：`Copy-Item "weasel\data\*" $dataDir -Recurse -Force` 在 `$dataDir` 不存在时对子目录条目报 `Container cannot be copied onto existing leaf item`（PowerShell 已知行为）——Windows 发布 job 失败。
2. **macOS 公证 401**：notarytool `Invalid credentials`——`APPLE_ID` / `APPLE_APP_PASSWORD` secrets 被占位符原样设置（用户操作，需重设，非代码问题）。

## 目标 / 验收

- ps1 修复后 Windows vendor 获取成功（ci.yml vendor-rime job 双平台实测）
- 用户重设两个 secrets 后公证通过

## 修复

- fetch-rime-vendor.ps1：拷贝前 `New-Item -ItemType Directory -Path $dataDir -Force`

---

## #17 🔄 [处理中][wt-dylib-fix] fix(release): DMG 公证 Invalid——捆绑 librime 版本化 dylib（@rpath 依赖）
- 状态：closed　创建：2026-08-25　标签：—

## 现状

干跑（32849155322）：macOS .app 签名+公证通过，**DMG 公证 status: Invalid**。

## 根因（证据链）

- `otool -L vendor/rime/librime.dylib` → 依赖 `@rpath/librime.1.dylib`
- librime 1.17.0 资产 `dist/lib/` 含 `librime.dylib` + **`librime.1.dylib`**（版本化）+ `librime.1.17.0.dylib`
- fetch 脚本只拷了 `librime.dylib` → 公证 dyld 级检查解析 @rpath 依赖失败 → Invalid
- dlopen 全路径加载不受影响（本地实测通过），所以此前从未暴露

## 目标 / 验收

- fetch 脚本捆绑 `librime.1.dylib`（含实体/链接关系），package.sh 完整性校验同步
- DMG 公证步骤失败时打印 notarytool log（诊断增强，next 失败直接可见原因）
- 重新干跑：macOS DMG 公证通过

## 修复

- scripts/fetch-rime-vendor.sh：`cp -a` 拷贝 librime.dylib + librime.1.dylib（保留链接）
- scripts/fetch-rime-vendor.ps1：补拷 librime.1.dylib（及实体版本文件）
- release.yml DMG 公证：失败时 `xcrun notarytool log <id>` 打印原因

---

## #19 🔄 [处理中][wt-rate-fix] fix(release): vendor 脚本 API 限流（403）——改 gh api + GH_TOKEN
- 状态：closed　创建：2026-08-25　标签：—

## 现状

tag v0.1.0 触发的发布 run（32854231579）两平台均在「获取 Rime vendor」失败：
`curl: (56) The requested URL returned error: 403`——GitHub API 未认证调用限流
（60 次/时/IP，CI 共享 IP 偶发触发）。v0.1.0 tag 指向的 commit 不含修复，需重打。

## 目标 / 验收

- fetch 脚本 API 调用改 `gh api`（runner 上自动用 GITHUB_TOKEN，5000 次/时）不再限流
- release.yml / ci.yml 的 fetch 步骤显式注入 GH_TOKEN（LESSON：runner 上 gh 需显式 GH_TOKEN）
- 重新干跑全绿 → 重打 v0.1.0 → 正式发布

## 修复

- scripts/fetch-rime-vendor.sh：`curl api.github.com` → `gh api repos/rime/librime/releases/tags/1.17.0`
- scripts/fetch-rime-vendor.ps1：`Invoke-RestMethod` → `gh api ... | ConvertFrom-Json`
- .github/workflows/*.yml：fetch 步骤 env 加 `GH_TOKEN: ${{ github.token }}`

---

## #21 🔄 [处理中][wt-ipc-sec] fix(ipc): IPC 信任边界——socket 用户隔离 + 目录 0700 + ping 验活
- 状态：closed　创建：2026-08-25　标签：—

## 现状

架构审查 P0-1：IPC 端点零隔离、零认证、可预占。
- macOS: `GenericNamespaced` 落到写死的 `/tmp/verba-ime`（全局共享、粘滞位）——其他用户可预占（永久 DoS）或假冒 daemon 窃取 API key/提示词/截图/录音
- Linux: abstract socket 无 inode 权限，任何用户可连
- Windows: 机器级全局管道 `\\.\pipe\verba-ime`，无用户隔离
- 前端 `try_connect` 成功即信任，不验活

## 目标 / 验收

- Unix: socket 移入用户数据目录（`~/Library/Application Support/Verba` / `~/.local/share/verba`），目录 chmod 0700——跨用户不可见不可连
- Windows: 管道名 `verba-ime-{USERNAME}` per-user 隔离
- 前端连接后 ping 验活（对端能回 Pong 才信任）
- 现有测试适配（Unix 全路径 socket）

## 批次清单

- [ ] verba-ipc: name 解析平台分支（Unix FilesystemUdSocket 全路径 / Windows GenericNamespaced per-user）
- [ ] verba-ipc: client `connect()` 用默认 spec；`connect_named` 平台分支
- [ ] verba-ipc: server `serve` 平台分支
- [ ] verba-daemon: data_dir chmod 0700（unix）+ serve 传新 spec
- [ ] 前端 windows/macos ipc.rs：try_connect 加 ping 验活
- [ ] tests: roundtrip/interprocess_raw 适配 Unix 全路径
- [ ] 已知限制记录：interprocess 2.4.3 无 uid 访问器（PeerCreds 仅 pid），对端 uid 校验留待 libc 层实现（同用户威胁可读 keyring，超出 IPC 保护范围）

---

## #23 🔄 [处理中][wt-b2-fixes] fix(core): P1 批次——取消表连接身份/粘贴截断/文字恒黑/stale socket
- 状态：closed　创建：2026-08-25　标签：—

## 现状

架构审查 P1 批次（正确性小修，互不依赖，批次处理）：

1. **P1-1 取消表全局 id 碰撞**：`cancels: HashMap<u64, ...>` 用全局键，但请求 id 每连接从 1 自增——多连接首个请求必撞 id=1，取消串扰/静默失效
2. **P1-2 macOS 粘贴截断**：imk.rs classify_key 对 keyCode=0 只取 `.chars().next()`，粘贴整串只剩 1 字符
3. **P1-3 候选文字恒黑**：renderer.rs u8 `wrapping_mul` 预乘溢出（255×255→1→0），暗色主题文字不可见
4. **P1-4 stale socket 阻塞重启**：daemon 异常退出残留 socket 文件 → 下次 bind 失败无法自愈

## 目标 / 验收

- 取消表键含连接身份（server 分配 conn_id 传入 handler），多客户端取消互不干扰
- macOS 粘贴整串进入状态机（逐字符喂入）
- 候选文字按主题色渲染（u16 中间量预乘 + a=255 边界测试）
- daemon 启动时 stale socket 自动清理（try_overwrite）

## 批次清单

- [ ] verba-ipc server: conn_id 分配 + RequestHandler::handle 签名
- [ ] verba-daemon handler: cancels 键 (conn_id, req_id)
- [ ] macOS imk.rs: 粘贴整串处理
- [ ] verba-candidate renderer: u16 预乘修复 + 测试
- [ ] daemon serve: try_overwrite(true)

---

## #25 🔄 [处理中][wt-b3-timeout] fix(core): P2 批次——IPC 超时/Rime spawn_blocking/流 id 校验
- 状态：closed　创建：2026-08-25　标签：—

## 现状

架构审查 P2 批次（超时与边界）：

1. **IPC 全链路无超时**：`IpcError::Timeout` 是死代码（定义了从未使用）；服务端读帧无 idle 超时——慢客户端可挂连接；TTS/ASR 只有 connect_timeout 无 read timeout；Rime 卡死时前端线程永久阻塞
2. **Rime 同步 FFI 阻塞 tokio worker**：`handler.rs` rime_query 同步调用跑在 async 处理器上，首次部署 2-5s 阻塞该 worker 上所有请求；无超时
3. **Windows 流 chunk 无 request_id 校验**：`text_service.rs` drain 不比对 stream_request_id（macOS 有 seq 过滤），「提交后立即新流」窗口内旧流 chunk 污染新结果

## 目标 / 验收

- 服务端读帧 idle 超时（60s），慢/死连接自动回收
- Rime 查询走 spawn_blocking + 超时（10s），tokio worker 不被阻塞
- Windows drain 跳过非当前流的 chunk（与 macOS 对齐）
- 测试全绿

## 批次清单

- [ ] verba-ipc server: 读帧 idle 超时
- [ ] verba-daemon: rime_query/warmup 走 spawn_blocking + 超时
- [ ] frontends/windows: drain 校验 stream_request_id

---

## #27 fix(windows): 取消跨连接失效——LLM 流注册在 worker 连接，取消走控制连接查不到 token
- 状态：closed　创建：2026-08-25　标签：—

## 现状

架构审查复核发现（P1-1 键控后的遗留）：**Windows 前端取消走独立控制连接（data.control），而 LLM 流注册在 worker 自己的连接上**——daemon 取消表按 `(conn_id, req_id)` 键控，控制连接的 conn_id 与流连接的 conn_id 必然不同 → `(control_conn, id)` 永远查不到 `(worker_conn, id)` 的 token → **Windows 上按 Esc/取消组合无法真正中断 LLM 流**（流跑完为止）。macOS 同连接 start/cancel/read 正常。

60s idle 超时回收控制连接只是给该问题多了一条失败路径（重连自愈已补）；根治需取消与流同连接。

## 目标 / 验收

- Windows 取消真正到达 daemon 的流注册键（(worker_conn, id)）

## 候选方案

- A. 流 worker 的 client 共享给取消路径：worker 循环改为「事件轮询 + 取消标志检查」，取消置 AtomicBool → worker 在同连接上发 llm_cancel（需 VerbaClient 支持并发或 worker 内处理）
- B. 协议层：LlmCancel 携带「目标流连接标识」，daemon 跨连接查找（破坏连接隔离语义，需谨慎）
- C. 简化：取消即断开流连接（daemon 检测连接关闭自动终止该连接上所有流）——把「取消」语义映射为「断流连接」，前端 worker 收到取消标志后直接 drop client 断开，daemon 侧连接断开清理在途流

验收：Windows 上 LLM 流式期间按 Esc，流立即停止（日志确认取消命中）

---

## #30 🔄 [处理中][wt-session-scope] fix(daemon): AI 多轮上下文会话隔离（history 按 session_id 分组）
- 状态：closed　创建：2026-08-25　标签：—

## 现状

架构审查剩余项：daemon 的 AI 多轮上下文（history）是全局共享的——多客户端（多控制器/多应用）的 `//` 对话都写进同一个 history，B 的 prompt 里混入 A 的历史。

## 目标 / 验收

- 协议层：`LlmGenerate` 加 `session_id` 字段
- daemon：history 按 `session_id` 分组（HashMap<session_id, VecDeque>），每会话独立上下文
- 前端：每控制器/每进程生成唯一 `session_id`（macOS Ivars / Windows TextServiceData），发 `llm_start` 时携带
- `//重置` / `//会话` 只作用于本会话

## 批次清单

- [ ] verba.proto: LlmGenerate.session_id
- [ ] verba-ipc client: llm_start 加 session_id
- [ ] verba-daemon handler: history 按 session_id 分组
- [ ] macOS 前端: Ivars.session_id，发请求携带
- [ ] Windows 前端: TextServiceData.session_id，发请求携带
- [ ] 测试：会话隔离（不同 session_id 的 history 互不影响）

## 验收

- 两个会话各自 `//` 对话，上下文互不串扰
- 同会话多轮上下文保持（回归）
- `//重置` 只清本会话

---

## #36 词库：luna_pinyin_simp 加入 biáng（biángbiáng 面）自定义短语
- 状态：closed　创建：2026-08-26　标签：—

## 背景

biáng 字（U+30EDD 繁体形 / U+30EDE 简体形，CJK Ext G，Unicode 13.0）在 Rime 默认词库与 Weasel 0.17.4 数据中均无条目，用户打 `biang` 出不来。字体渲染（需 Ext G 字体）属操作系统侧，本议题只解决词库层：候选能出。

## 方案

- 新增 `scripts/rime-extra/custom_phrase.txt`（词条）与 `luna_pinyin_simp.custom.yaml`（接线 table_translator@custom_phrase）
- `fetch-rime-vendor.sh/.ps1` 在 Weasel 数据拷贝后注入（幂等 + 回归守卫断言，与 wubi86 注入同风格）
- release.yml 两平台 Rime 冒烟各加一条 `verba-cli rime biang luna_pinyin_simp` 断言输出含字形

## 验收

- CI vendor-rime（win/mac）绿 + 发布流水线冒烟通过
- 出候选；显示与否取决于系统是否装有 Ext G 字体（文档注明）

---

## #38 docs: architecture.md 收口候选窗渲染器选型理由（tiny-skia vs Slint/femtovg）
- 状态：closed　创建：2026-08-26　标签：—

候选窗自绘 + tiny-skia 的选型已落地（2026-08-23），但 architecture.md §12 开放问题 2 仍是未决表述，且选型理由（为什么不用系统候选 UI / 系统绘制 API / Slint+femtovg）未记录。补：§4 crate 表加 verba-candidate 行、§4 Windows 候选窗表述改已决、§12.2 写决策与理由、roadmap M5 加交叉引用。

---

## #40 macOS 26 输入源列表显示原始 ID（dev.verba.inputmethod.Verba.Pinyin）
- 状态：closed　创建：2026-08-26　标签：—

真机（macOS 26.5.2）截图确认：系统设置 → 键盘 → 文本输入 → 添加输入法列表中，拾言显示为原始 ID `dev.verba.inputmethod.Verba.Pinyin`，而非「拾言输入法」。

根因：TIS 模式行显示名按 TISInputSourceID 从 bundle 的 InfoPlist.strings 取值，缺失时回退原始 ID；应用源名取 CFBundleName（Verba）。

修复 PR：`fix/macos-input-source-display-name`

---

## #42 fix(macos-ime): 真机验收批次——重入崩溃根治/拼音零泄漏/候选行为对齐手心
- 状态：closed　创建：2026-08-27　标签：—

## 背景

真机验收（macOS 26）连续反馈打字崩溃、卡顿、拼音漏上屏。逐项定位出根因并修复，行为语义向手心输入法看齐。

## Checklist

- [x] IMKCandidates.show() XPC 等待期间泵主 runloop → 16ms drain 定时器重入 setMarkedText → ObjC 异常穿透 Rust 清理帧 SIGABRT（host_call 深度守卫）
- [x] deactivate 走 unmarkText 不被识别：`_IPMDServerClientWrapperLegacy` 无该选择器 → 改 setMarkedText:@"" + NSNotFound range
- [x] 候选窗每页只有 5 个：librime menu/page_size 默认 5 → 部署期写 default.custom.yaml 补丁为 9 + candidates() 分页收集
- [x] 冷启动首键候选迟到致拼音裸奔上屏：activate_server 后台线程预热 daemon
- [x] 回车误上屏候选词：Enter 语义改为恒提交原始输入（英文通道）
- [x] 无候选拼音直接泄漏上屏：合成原文候选项展示（settled-only，避免抖动），盲按空格吞键不提交
- [x] 组合无上限可无限增长：MAX_PINYIN_BUFFER=48 封顶吞键（退格仍可用）
- [x] 中文标点应为全角：全角标点映射 + 引号成对交替
- [x] 会话切换组合残留：deactivate_server 全量清理
- [x] 候选查询积压响应慢：单 worker 最新槽位合并中间态查询
- [x] /tmp 调试日志转正：env_logger + VerbaDirs log_dir 文件日志

## 验收标准

- cargo fmt/clippy/test 门禁绿（verba-core 56 tests）
- 真机：连打不再崩溃；任何输入序列字母不裸奔上屏（ settles 后原文成候选项、空格二次确认）；回车上屏原文字母；每页 9 候选

> **gqf2008** (2026-09-17):
>
> 【2026-09-17 补充：同源崩溃家族还有一条未覆盖路径】🦀
> 
> 真机又复现同类 abort（`verba-mac-2026-09-17-122502.ips`）：`refresh_candidate_window()` 持有 `candidates_ui.borrow_mut()` 跨过 `IMKCandidates show:`（show 会同步泵运行循环查客户端 markedRange），泵期间重入的 `input_text` 取 `candidates_ui.borrow()` → `already mutably borrowed` panic → 穿出 objc2 的 ObjC 异常 shim 二次 panic → `rust_drop_panic` → abort。
> 
> 本 issue 当年修的是「重入窗口 + ObjC 异常穿透 Rust 清理帧」这一类；这次的具体触发点（RefCell 借用跨 host_call）不在当时的清单里。修复与验收见 #60 的最新评论。

---

## #44 fix(input): PR #43 审查遗留批次——Windows 真机验收与残余边角（实现项已合 118725c）
- 状态：closed　创建：2026-08-27　标签：—

## 背景

PR #43（已合并 9348550）独立审查通过，但以下建议项按审查意见「可与下一轮批次合流」留待本批。另含跨平台一致性审查中需真机验收的跳过项。

## Checklist

- [x] 主组合非空格通道的在途保护：标点 flush / 大写直通 / 斜杠回退不走 select_candidate 的暂缓分支，慢查询窗口内仍可能裸奔原文（machine.rs:394/377/354；冷部署期查询可达数秒）  →已随 PR #45（118725c）落地
- [x] librime env-gated 测试延伸：candidates max=9 跨页断言（当前仅 max=5 首页路径）；default.custom.yaml 幂等写入的纯 fs 单测  →已随 PR #45（118725c）落地
- [x] 状态机测试三条：顶格 48 字符后退格可删、space_deferred 后继续打字对增长 buffer 的补执行语义、数字选字在途忽略行为钉住  →已随 PR #45（118725c）落地
- [x] Windows 真机验收（代码部分）：TSF 两段式派发 / 流 token 代际 CAS——token 安装抽为纯函数 install_stream_token（同代保留先到者/旧代绝不覆盖新代），派发收集抽为 collect_steps；三条测试钉住并跑在 windows-latest。**真机 TSF 路由副作用两步**移入 docs/manual-acceptance-windows.md「v0.2.2 清扫批次」节，由用户在 Windows 真机执行
- [x] 跨平台审查跳过项裁决：Windows 键位认领标点导致全角映射不可达（text_service should_claim_key）、PendingSlash 回退 format!("/{c}") 是否应走全角——均涉行为变更需产品确认后定方案  →已随 PR #45（118725c）落地

- [x] Windows stream Error 臂与 macOS 对齐——PR #45 已落地（start_rime_candidates fail 闭包回 done=true 空结果）；本批抽为 push_rime_fail 纯函数，rime_fail_event_is_done_empty_candidates + collect_steps_settles_inflight_deferred_space 双测试钉住结算语义
- [x] Windows 修饰键守卫（代码部分）：认领判定测试跑在 windows-latest（claim_chars_cover_machine_punct）；Ctrl/Alt 路由副作用真机步骤移入验收清单「v0.2.2 清扫批次」节
- [x] '/' 通道盲窗直出裁决：**保持直出**——'/' 是显式模式切换键，非「期望候选」通道；暂缓重放会把已解决候选插进用户正在输入的提示词组合（Prompt 态活跃时插入文本），引入错序竞态，收益远小于风险。machine.rs feed_pinyin_char 已加裁决注释 + slash_in_blind_window_commits_raw_enters_pending 钉住

## 参考

- PR #43 第二轮复审报告（开放存量建议清单）
- /code-review max --fix 记录（JSON note 字段的 5 项跳过理由）

---

## #46 🔄 [处理中][wt-v020-release] fix(ci): rustfmt 1.98 漂移致 main Format 红，pin 工具链根治
- 状态：closed　创建：2026-08-27　标签：—

## 背景

PR #43/#45 合并后 main 上 frontend-macos/frontend-windows 两 job 的 Format 步骤连红。本地门禁全绿——根因是**工具链漂移**：CI `dtolnay/rust-toolchain@stable` 当天拿到 rustc 1.98.0（rustfmt 断行规则更新），本地仍是 1.97.1；同一份代码两版 fmt 结论不同。

## 方案

1. 新增 `rust-toolchain.toml` pin 到 1.98.0（dtolnay action 优先读仓库内文件；release.yml 同步受益）
2. 本地升级 1.98.0 后全仓 `cargo fmt` 重排，门禁复验
3. 小批次 PR 修复 main 预存红（前例 c18b33d）

## 验收

- 三平台 CI 全绿
- `cargo fmt --version` 本地与 CI 一致（后续升级须同步改 pin 行）

---

## #48 v0.2.2 遗留清扫批次——菜单栏图标 / 生僻字安装 / 安装UX 自动化
- 状态：closed　创建：2026-08-27　标签：—

## 背景

v0.2.1 已发布（e0dde4c）。以下三项为用户此前反馈/要求的功能遗留，统一本批清扫。另 issue #44 的残余项在 #44 内收口（含 '/' 盲窗裁决与 Windows CI 测试补钉）。

## Checklist

- [x] **菜单栏专属图标（用户反馈 2026-08-26）**：当前无 `tsInputMethodIconFileKey`，菜单栏显示通用图标，与系统「简体拼音」无区分。方向已定：模板 PDF 图标（`tsInputMethodIconFileKey` / 各 `tsInputMode*IconFileKey`），随 .app 打包。
- [x] **设置窗口支持安装生僻字功能（用户要求）**：设置面板加「安装生僻字扩展」入口，一键把 rime-extra 生僻字词条装进用户 Rime 目录并重新部署（biang 词条机制已合 main 9aeac99，复用其管线）。
- [x] **安装 UX 自动化（用户反馈「手动安装太麻烦」）**：已验证 `TISEnableInputSource` 在 macOS 26 可编程启用输入源（弹系统确认）。目标：DMG 安装流程自动注册 + 启用输入源（至少安装后一步完成），并更新安装说明。

## 验证

- 各 PR 本地门禁（三 workspace fmt/clippy/test）+ 独立审查通过后合并
- macOS 侧：真机装新 DMG 后菜单栏出图标、生僻字一键安装后 rime 候选出字、输入源自动启用
- Windows 侧不涉及（或仅文档）

> **gqf2008** (2026-08-27):
>
> 项 1（菜单栏图标）已合并：PR #50（4d89a69）——Icon.pdf 模板气泡剪影 + tsInputMethodIconFileKey/tsInputModeIconFileKey + CI 断言。待真机装 .app 验证菜单栏显示。

> **gqf2008** (2026-08-27):
>
> 项 2 已完成：PR #51 合并（498e93d）。
> 设置面板「生僻字扩展」分组 + 一键安装按钮；daemon RimeInstallExtra IPC（合并语义与 fetch-rime-vendor.sh 同构，幂等）；verba-cli rime install-extra 命令行入口。
> 真机验收：设置窗口点安装 → 输入 biang 出 𰻝𰻞 候选。

---

## #56 测试 issue
- 状态：closed　创建：2026-08-28　标签：—

测试

> **gqf2008** (2026-08-28):
>
> 清理测试 issue

---

## #57 [M6] whisper.cpp 本地 ASR 集成（阻塞项）
- 状态：closed　创建：2026-08-28　标签：—

M6 收尾阻塞项：集成 whisper.cpp 实现本地 ASR，目标：中文识别、词级时间戳、内存可控。

角色：AI 多模态（叶澜 @verba-yelan）
KPI：本地 ASR 按时落地，性能预算达标

> **gqf2008** (2026-08-29):
>
> 【2026-08-29 Owner 范围决策】本地 ASR（whisper.cpp / audio.cpp）**冻结为实验性**：代码保留、默认关闭、入口隐藏、不在 M6 承诺。叶澜不再攻坚 whisper，重心转向 OCR 链路优化 + \//看图\ 视觉体验/延迟优化。恢复条件：中文 ASR 方案成熟，或竞品（素言）逼到语音时由 Owner 解除冻结。文档同步见 PR #78。

> **gqf2008** (2026-09-08):
>
> 关闭依据：Owner 2026-08-29 已将本地 ASR（whisper.cpp / audio.cpp）冻结为实验性（代码保留、默认关闭、入口隐藏、不在 M6 承诺，见 roadmap 范围决策）。本 issue 不再是 M6 阻塞项，按冻结结论关闭；未来若中文 ASR 方案成熟或竞品波及，由 Owner 解冻后重开。docs 已同步（PR #78）。

---

## #58 [M6] OCR/ASR/LLM/TTS 性能预算表
- 状态：open　创建：2026-08-28　标签：—

建立四大多模态能力的性能预算表（延迟+内存），并实测达标。

角色：核心引擎（陆遥 @verba-luyao）
KPI：性能预算达成率 100%

> **gqf2008** (2026-08-29):
>
> 【2026-08-29 Owner 范围决策】性能预算 #58 范围缩减为「**LLM 核心输入链路 + OCR**」：ASR/TTS 冻结为实验性（代码保留、默认关闭、入口隐藏、不承诺），不再纳入预算表。请按新范围核对端侧资源占用口径（陆遥）。文档同步见 PR #78。

> **gqf2008** (2026-09-07):
>
> ## OCR 性能实测（Windows 11 + RapidOCR PP-OCRv5，daemon+IPC 全链路）
> | 输入 | 延迟 | 识别字数 |
> |---|---|---|
> | 4K 全屏 3840x2160 | 6099 ms | 3007 |
> | 1080p 1920x1080 | 3909 ms | 1568 |
> | 720p 1280x720 | 1976 ms | 720 |
> 
> 预算基准（docs/architecture.md §8）：截图 OCR < 2s（本地 PaddleOCR）。
> 
> 结论：
> - 720p 达标（1976ms < 2s）
> - 1080p / 4K 超预算，延迟随分辨率近似线性增长
> - 建议后续优化方向：OCR 前按需降采样/限制最长边（4K 全屏尤其需要），或预算表按分辨率分档

> **gqf2008** (2026-09-08):
>
> 🔄 [处理中][wt-m6-remaining] 范围已缩减为 LLM 核心输入链路 + OCR（Owner 决策）；处理中：重定 performance budget 表，去掉冻结的 ASR/TTS 行，录入 OCR 实测（720p/1080p/4K），记录 4K 降采样/限最长边优化方向。

> **gqf2008** (2026-09-08):
>
> ✅ #[wt-m6-remaining] PR #108：性能预算表重定范围（移除冻结 ASR/TTS 行）+ OCR 前超长边降采样（OCR_MAX_EDGE=1600px，Triangle 过滤）。降采样后延迟需 Windows 真机复测（未复测前不以『复测达标』口径验收）。

> **gqf2008** (2026-09-09):
>
> 已由 PR #108 合并（commit 1d24b84）：性能预算表收敛到正式能力（直输/OCR/LLM/daemon 内存）+ OCR 前超长边降采样（OCR_MAX_EDGE=1600px）。**待 Windows 真机复测降采样后延迟**方可按「实测达标」口径关闭；未复测前本 issue 保持 open。

---

## #59 [M6] 日志脱敏规则 + 测试
- 状态：closed　创建：2026-08-28　标签：—

定义日志脱敏规则（密钥/个人数据），实现并补测试。

角色：核心引擎（陆遥 @verba-luyao）
KPI：脱敏规则覆盖率 100%

> **gqf2008** (2026-09-08):
>
> 🔄 [处理中][wt-m6-remaining] 处理中：实现日志脱敏（默认不记录用户文本与密钥；调试模式才记录文本且仅本地存储），落地到 daemon 日志入口 + 单测。

> **gqf2008** (2026-09-08):
>
> ✅ #[wt-m6-remaining] PR #108：verba-daemon redact 模块 + TeeLog 写出口统一脱敏（api_key/Authorization/Bearer/各类 token/PEM 私钥）；15 单测（std+unicode 特性集隔离验证）。

> **gqf2008** (2026-09-09):
>
> 已由 PR #108 完成并合并（commit 76e3b2f → main）：verba-daemon redact 模块 + TeeLog 写出口统一脱敏（api_key/Authorization/Bearer/各类 token/PEM 私钥）+ 18 单测；CI 全绿。

---

## #60 [M6] macOS IMK 交互验收收尾
- 状态：open　创建：2026-08-28　标签：前端平台,质量测试,P0

macOS IMK 前端交互验收收尾，补齐验收清单。

角色：前端平台（顾晨 @verba-guchen）
KPI：macOS 验收清单全绿

> **gqf2008** (2026-08-28):
>
> 【巡检沉淀 08-28】前端平台（顾晨）三端交互验收报告 → macOS IMK 待修项清单（5 项）
> 
> 来源：顾晨群开工响应 + 交付报告《三端交互验收_20260828》；顾晨今日行动：执行 M1 全套真机交互验收 → 输出 docs/manual-acceptance-macos.md → 推动本 issue 收口。
> 
> | # | 待修项 | 优先级 | 状态 |
> |---|---|---|---|
> | M1 | 真机交互验收全套：候选窗自动展示、输入法切换、TIS 注册启用、Enter/翻页/←→/退格/Esc 全交互 | P0 | 待真机 |
> | M2 | 权限弹窗真机确认：麦克风 TCC（NSMicrophoneUsageDescription）、屏幕录制（ScreenCaptureKit） | P1 | 待真机 |
> | M3 | `//` AI 模式 LLM 流式 preedit 真机完整验收（librime 链路已过，AI 链路补验） | P1 | 待真机 |
> | M4 | 多客户端会话语义回归（v0.2.0 已按 controller 隔离） | P2 | 待回归 |
> | M5 | Developer ID 签名 + 公证（发布前置，需证书与流程） | P2 | 阻塞发布 |
> 
> ⚠️ 阻塞项：Developer ID 证书需 Owner 提供；验收结论将同步技术负责人（林岳）。

> **gqf2008** (2026-08-28):
>
> **认领**：前端平台（顾晨）负责，P0。今日开工推进。
> 
> 进展（2026-08-28）：
> - IMK 控制器/按键状态机/上屏/preedit/候选窗代码与 CI 已完成（08-23/24）；librime 单引擎链路真机通过（08-24）；DMG 一键安装+输入源启用已合入 v0.2.2（08-27）。
> - 待办真机交互验收：候选窗自动展示、输入法切换、TIS 注册、Enter/翻页/←→/退格/Esc 全交互；TCC 权限弹窗（麦克风/录屏）；`//` AI 流式真机补验。
> - 验收清单文档将随 PR 提交（docs/manual-acceptance-macos.md，见 #67）。
> - 看板：Verba M6 交付看板（https://github.com/users/gqf2008/projects/4），状态=开发中。

> **gqf2008** (2026-08-29):
>
> 【QA 认领·许衡】M6 macOS IMK 交互验收收尾：认领 macOS 验收清单与回归（docs/manual-acceptance-macos.md），OCR 正式验收，ASR/TTS 冻结态负向验证。

> **gqf2008** (2026-08-29):
>
> 【进展留痕 2026-08-29】按 QA 新口径推进 macOS IMK 验收：
> - 验收清单 docs/manual-acceptance-macos.md 已落地（PR #79，审阅通过）：直输 / 候选窗(Rime) / //AI 流式（Esc 无残留、快速流切换 5 轮）/ OCR 正式（ScreenCaptureKit 权限）/ ASR·TTS 冻结负向 / TCC 弹窗 / Developer ID 签名公证。
> - main CI Format 5 job 红（run 33234518956）= 验收前 P0 阻塞，修复 PR #80 已提，绿后 rebase #79 合并。
> - 下一步：真机执行清单项，本 issue 逐项留痕；P0/P1 清零 + CI 全绿即关闭。

> **gqf2008** (2026-08-29):
>
> 【进展留痕 2026-08-29】P0 阻塞已清除 + 文档交付合并：
> - main CI 全绿：PR #80（rustfmt 1.98.0 对齐 + frontend-windows clippy -D warnings 清零）已合并（44202fe），main 运行 33239541323 = success。
> - PR #79（M6 测试计划 + 三端验收清单）rebase 后 CI 全绿并已合并（28b076ca）→ Closes #76，看板已置「已完成」。
> - 下一步：按 docs/manual-acceptance-macos.md 真机执行清单（安装/启用→直输→候选窗→//AI 流式→OCR→冻结负向），本 issue 逐项留痕；P0/P1 清零 + CI 全绿即关闭。

> **gqf2008** (2026-08-29):
>
> @许衡 认领 macOS 验收（assignee=gqf2008 共用账号）。验收清单见 docs/manual-acceptance-macos.md，P0/P1 清零 + CI 全绿关闭。

> **gqf2008** (2026-09-09):
>
> 【进展留痕 2026-09-09】M5 Developer ID 签名 + 公证阻塞已解除：
> 
> - 本机已安装 Developer ID Application + Developer ID Installer 证书，`security find-identity -v` 显示 3 个 valid identity。
> - GitHub Actions secrets 已补齐 `APPLE_INSTALLER_CERT_P12` / `APPLE_INSTALLER_CERT_PASSWORD`。
> - workflow_dispatch 干跑 [run 34305219742](https://github.com/gqf2008/verba-ime/actions/runs/34305219742) 全绿（macos 16m03s、windows 19m11s），PKG 签名 + 公证 + staple 成功。
> - 本地复核 macOS artifact：
>   - `pkgutil --check-signature`：signed by Developer ID Installer、Notarization trusted、timestamp 2026-09-09 03:11:38 UTC
>   - `xcrun stapler validate`：worked
>   - `spctl --assess --type install`：accepted，source=Notarized Developer ID
>   - `Verba-0.2.9.pkg` SHA256：`6edd40cf3ca208da14240318ff4b1201bce01b14efb6b044477de4776eeaff9f`
> 
> M5 可视为完成；本 issue 剩余 M1–M4 真机交互验收，仍需 macOS 真机执行，暂不关闭。 🦀

> **gqf2008** (2026-09-09):
>
> 🔄 [处理中][wt-macos-imk-activation] 接管 macOS 输入源菜单不出现/激活崩溃收口。
> 
> 真机复核发现两个具体根因：
> 1. `InputMethodConnectionName` 仍为旧的 `Verba_1_Connection`；现代 IMK 约定必须是 `<CFBundleIdentifier>_Connection`，当前应为 `dev.verba.inputmethod.Verba_Connection`。系统日志存在 `IMKLaunchAgent -requestIMKXPCEndpointInvalid`，与连接名不匹配吻合。
> 2. 现有 crash report（verba-mac-2026-09-09-144401.ips）显示激活链路 `IMKInputController -> _IPMDServerClientWrapperLegacy markedRange -> invocationAwaitXPCReply` 抛异常后 Rust 清理 abort；需要避免 `updateComposition` 默认 `selectionRange` 再走客户端 `markedRange`。
> 
> 验收：连接名与 bundle id 一致；`verba-register` 后无需打开键盘设置即可在输入法菜单出现并可选中；选中后不再崩溃。关联 #121/#123 的菜单验收。 🦀

> **gqf2008** (2026-09-09):
>
> 【进展留痕 2026-09-09】IMK 连接名/激活崩溃根因修复已合并：PR #126 → merge commit `768e586`。
> 
> - `InputMethodConnectionName` 改为从主 bundle `CFBundleIdentifier` 运行时派生 `<bundle id>_Connection`，CI 守卫按派生关系断言。
> - 覆盖 `selectionRange` 返回 composed text 的 UTF-16 末尾，避免默认实现经客户端 `markedRange` 触发 XPC 异常后 abort；补生产 helper 回归测试。
> - macOS frontend fmt/test/clippy 与独立复审通过。
> 
> 仍未完成（阻塞 #121/#123 菜单验收）：系统级安装后的 GUI 激活。实测 `TISEnableInputSource` 从 CLI/root postinstall 调用返回 0 但不会真正启用；需要像手心输入法一样，在用户可见的 GUI 安装器进程内完成 `TISRegisterInputSource` + `TISEnableInputSource`。下一步做 GUI 安装/卸载器并发布 v0.2.13，最终需要管理员授权 + 系统启用确认各一次。 🦀

> **gqf2008** (2026-09-09):
>
> 🔄 [处理中][wt-ocr-input-race] 接管 OCR 文字不上屏收口。
> 
> 真机日志（verba-mac.log 2026-09-09T09:24）复现：OCR 识别成功（`stdout_len=687`、识别 `len=560`），但结果到达与用户首个按键 `o` 同一 tick：`drain_stream` 先进入 OCR 预览，随后同一 `inputText` 键走预览 Other 分支，立刻清预览并把 `o` 当拼音处理，560 字识别文本被丢弃。
> 
> 根因：OCR 结果在输入回调内到达时，结果处理与当前按键处理没有隔离；预览被当前键立即取消。
> 
> 修复方向：输入期间 OCR 结果到达直接提交上屏；空闲时仍保留预览确认。验收：OCR 结果不再被同 tick 按键吞掉，输入可继续，无重复/乱序。

> **gqf2008** (2026-09-09):
>
> 【OCR 文字不上屏修复】PR #127 已合并（merge commit `81109b2`）。
> 
> - 真机日志根因：OCR 识别成功 560 字，但结果与用户首个按键 `o` 同 tick 到达，先进入预览后被同一键 Other 分支清掉。
> - 修复：输入期间 OCR 直接提交；预览期间可打印键先提交再继续；Space 纳入确认键；重入队列与修饰键路径统一收口。
> - 本地已把 main 构建的 `verba-mac`/`verba-register` 安装到用户级 `~/Library/Input Methods/Verba.app`，连接名同步新值，旧 app 备份在 `~/Downloads/Verba.app.ocr-fix-backup-1788946632`；新进程无 crash。
> - 待真机复验：OCR 预览出现后立即连续输入 / timer-first / 重入积压 / Cmd 快捷键。 🦀

> **gqf2008** (2026-09-09):
>
> 【v0.2.13 发布收口】
> 
> - IMK 连接名/markedRange 修复、OCR 上屏竞态修复已随 v0.2.13 发布（PR #126/#127）。
> - 用户级 DMG 自动启用已随 v0.2.13 发布（PR #128）：`verba-register` 写 `com.apple.inputsources` 白名单并刷新 TextInputMenuAgent，无需手动添加。
> - Release v0.2.13 run `34356976512` 全绿；DMG/PKG/EXE/SHA256SUMS 完整，SHA256 与 API digest 一致；.app/PKG 公证 + staple 成功。
> - 剩余系统级 PKG 自动启用拆到 #132（v0.2.14 GUI helper）。
> 
> 本 issue 保留到 macOS 真机交互验收（候选/OCR/权限/多客户端）最终收口。 🦀

> **gqf2008** (2026-09-17):
>
> 【真机复核 2026-09-17 · 新 P0：候选窗 show() 期间持有 RefCell 借用 → 第 2 个按键 SIGABRT】🦀
> 
> 用户反馈「拼音泄漏 / 呼不出候选框 / 一点也不好用」。查真机日志 + 崩溃报告，**不是手感问题，是进程 abort**：
> 
> - 崩溃报告：`~/Library/Logs/DiagnosticReports/verba-mac-2026-09-17-122502.ips`（EXC_CRASH / SIGABRT，进程 12:22:36 启动）
> - 时间线：打 `n` 正常（候选窗弹出、候选 你/那/呢… 到达）→ 紧接着打 `i` → **当场 abort**
> - 症状因果：进程死了 → 候选框消失 + 系统顶回原输入源 → 后续字母裸奔上屏（用户感知的「拼音泄漏」）
> 
> **根因**（崩溃栈 + 日志顺序双证据）
> 
> ```
> abort ← rust_drop_panic ← panic_unwind::exception_cleanup ← __cxa_end_catch
>   ← CFRunLoop（show 内嵌套泵）← IMKServerXPCInvocationAwaitXPCReply
>   ← _IPMDServerClientWrapperLegacy markedRange
>   ← IMKUICandidateWindowPositionController inlineFrameWithClient:
>   ← IMKCandidates show: ← objc2 exception helper
>   ← VerbaIMKController::refresh_candidate_window
>   ← VerbaIMKController::drain_stream（16ms 定时器）
> ```
> 
> `refresh_candidate_window()` 里 `candidates_ui.borrow_mut()` 的 RefMut **一直持有到函数结束**，而中段 `host_call("refresh.show")` 的 `IMKCandidates show:` 会**同步泵运行循环**（向客户端查 markedRange）。泵期间到达的按键重入 `input_text`，其算 `panel_visible` 那行取 `candidates_ui.borrow()` → `already mutably borrowed` panic → 该 panic 穿出 objc2 的 C++ try/catch，在 `__cxa_end_catch` 里二次 panic（`rust_drop_panic`）→ abort。
> 
> 日志顺序坐实：`n` 那次 `inputText enter …` 与紧跟的 `inputText s=Some("n") … panel_visible=…` **两行都在**；`i` 那次**只有 `inputText enter s=Some("i") key=34` 一行**——正死在第二行之前的那个 `borrow()`。
> 
> **修复**（`fix/macos-ime-candidate-borrow-crash`，已推 walgit）
> 1. `refresh_candidate_window` / `hide_candidate_window`：先 clone `Retained<IMKCandidates>` 并 `drop` 借用，再做会泵运行循环的宿主调用；
> 2. `catch_void` 外层加 `catch_unwind`——Rust panic 不得穿出 ObjC 异常帧（宁可不做这次宿主调用，也不能让整个输入法 abort）；
> 3. 回归测试 `catch_void_swallows_rust_panic`。
> 
> Verification: `cargo fmt --check` OK；`cargo clippy --all-targets -- -D warnings` OK；`cargo test` 31 passed（lib 20 / verba-mac 2 / verba-register 9）。
> 修复版已装入 `~/Library/Input Methods/Verba.app`（Developer ID 重签，TeamIdentifier 不变）。
> 
> **遗留（本 issue 收口清单，需并入验收）**
> - [ ] 真机连打复验：`nihao` / 连续 2 键 / 快速切换输入源不再 SIGABRT（本轮因输入源切换需系统确认、且桌面正被远控使用未做）
> - [ ] `selectionRange` 之外，IMK 内部仍会直接向**客户端**查 `markedRange`（见栈 14 帧）——它不经过我们的 override，这条 XPC 路径要单独评估
> - [ ] 同类风险扫描：确认没有其它「RefCell 借用跨 host_call」点（本轮已扫，`commit`/`clear_composition` 已是 clone-out 写法）
> - [ ] 安装后输入源 enabled 状态：`verba-register` 后父源 `enabled=true`，但 `TISSelectInputSource` 仍返回 -50，需确认是否为系统确认流程所致
> 
> 关联：#42（同源崩溃家族：ObjC 异常穿透 Rust 清理帧 → SIGABRT；本次是它未覆盖到的 `candidates_ui` 借用路径）

---

## #61 [M6] Windows 手动验收清单全绿
- 状态：open　创建：2026-08-28　标签：前端平台,质量测试,P0

执行 Windows 手动验收清单，P0/P1 清零。

角色：质量测试（许衡 @verba-xuheng）
KPI：验收通过率 100%，发布前 P0/P1 清零

> **gqf2008** (2026-08-28):
>
> 【巡检沉淀 08-28】前端平台（顾晨）三端交互验收报告 → Windows TSF 待修项清单（11 项）
> 
> 来源：顾晨群开工响应 + 交付报告《三端交互验收_20260828》；顾晨今日行动：优先 W1/W2 真机两步（issue #44 收口）→ W3 回归、W4 热键链路验收 → P2 项批量验证勾选清单。
> 
> **P0（待真机）**
> - W1 修饰键守卫真机两步：VS Code `Ctrl+.`/`Ctrl+,` 不被吞、Ctrl+字母无残留（issue #44 余项）
> - W2 两段式派发/流 token 代际：快速流切换无旧流混入，5 轮连测 + daemon 无 panic（issue #44 余项）
> 
> **P1（待回归/待真机）**
> - W3 流式中 Esc 取消无残留
> - W4 截图/听写/朗读接线实机验收：`//截图`/Ctrl+Alt+O 选区 OCR、`//听写`/Ctrl+Alt+M 录音 ASR、`//朗读` TTS（target_dev16）
> 
> **P2（待验证/待视觉）**
> - W5 候选窗横向布局+拼音组合头 UI 视觉确认（theme.layout 可切 vertical）
> - W6 分页/主题热重载视觉后补
> - W7 `verba-cli config set llm_base_url/llm_model` 热更新验证
> - W8 API Key 系统凭据库（keyring）验证
> - W9 首次配置远程 LLM 隐私提示确认
> - W10 卸载流程 `verba-reg unregister` 验证
> 
> **P3（可选）**
> - W11 慢网络下开流后取消无卡死

> **gqf2008** (2026-08-28):
>
> **认领**：前端平台（顾晨）负责，P0。今日开工推进。
> 
> 进展（2026-08-28）：
> - M1 直输/AI 链路已于 08-22 实机通过；M5 候选窗（跟随光标/避让/分页/主题/Rime 单引擎）08-23 收口。
> - 待办真机项：① issue #44 余项两步（修饰键守卫、流 token 代际，见 #65）；② 流式中 Esc 无残留回归；③ 截图/听写/朗读热键链路（#66）；④ 候选窗横向布局+拼音组合头等 P2 视觉确认。
> - 待修项全量清单与优先级见 #65/#66 及本 issue 评论区跟踪。
> - 看板：Verba M6 交付看板（https://github.com/users/gqf2008/projects/4），状态=开发中。

> **gqf2008** (2026-08-29):
>
> 【QA 认领·许衡】M6 Windows 手动验收清单全绿：认领验收标准/测试计划，门禁=P0/P1 清零，与顾晨协作；main CI 红（Format 5 job, run 33234518956）为验收前 P0 阻塞，需先修复。

> **gqf2008** (2026-08-29):
>
> 【进展留痕 2026-08-29】按 QA 新口径推进 Windows 验收：
> - docs/manual-acceptance-windows.md 已更新（PR #79）：OCR=正式验收项、ASR/TTS=冻结负向标注、性能=LLM+OCR。
> - main CI Format 5 job 红（run 33234518956）= 验收前 P0 阻塞，修复 PR #80 已提。
> - #74（修饰键守卫 + 流 token 代际）继续按清单真机执行，P0/P1 清零 + CI 全绿后关闭。

> **gqf2008** (2026-08-29):
>
> 【进展留痕 2026-08-29】P0 阻塞已清除：
> - main CI 全绿：PR #80 已合并（44202fe，rustfmt 1.98.0 对齐 + clippy 清零），main 运行 33239541323 = success。
> - 文档口径合并：PR #79 已合并（28b076ca）Closes #76，Windows 清单已按新口径（OCR 正式 / ASR·TTS 冻结）落地。
> - 下一步：#74 真机两步（修饰键守卫 + 流 token 代际 5 轮）+ 清单 P0/P1 项执行，本 issue 逐项留痕。

> **gqf2008** (2026-08-29):
>
> @许衡 认领 Windows 验收（assignee=gqf2008 共用账号）。验收清单见 docs/manual-acceptance-windows.md，P0/P1 清零 + CI 全绿关闭。

---

## #62 [M6] 里程碑排期与门禁把关
- 状态：closed　创建：2026-08-28　标签：—

M6 里程碑拆解排期（Alpha 2 周/Beta+2/GA+4），把关 CI 门禁全绿。

角色：技术负责人（林岳 @verba-linyue）
KPI：里程碑按期达成，门禁通过率 100%

> **gqf2008** (2026-09-08):
>
> 关闭依据：M6 里程碑已达成——v0.2.0–v0.2.9 已发布（最新 tag bf132c9，资产校验通过），main CI 全绿（run 34181704483 success）。门禁把关完成。如需跟踪下一里程碑，建议另开新跟踪 issue。

---

## #63 [运营] 种子用户群 + Beta 报名表
- 状态：closed　创建：2026-08-28　标签：—

建立种子用户群（重度输入法用户/开发者/无障碍需求者），准备 Beta 邀请制报名。

角色：社区运营（江澈 @verba-jiangche）
KPI：种子用户群 ≥50 人，Beta 报名表上线

> **gqf2008** (2026-09-06):
>
> 销售/运营条线 issue 清理：按用户指示关闭归档。注：GitHub 不支持通过 API 删除 issue，关闭即终态。

---

## #64 [运营] 竞品对标表（素言 SuYan 等）
- 状态：closed　创建：2026-08-28　标签：—

制作竞品对比表（素言 SuYan 等），用于社区+销售。

角色：内容增长（白露 @verba-bailu）
KPI：对标表完成并同步社区

> **gqf2008** (2026-09-06):
>
> 销售/运营条线 issue 清理：按用户指示关闭归档。注：GitHub 不支持通过 API 删除 issue，关闭即终态。

---

## #65 [运营] 运营数据快照模板
- 状态：closed　创建：2026-08-28　标签：—

建立运营数据快照模板（star/激活/留存/issue 响应时长），每周更新。

角色：运营负责人（苏芮 @verba-surui）
KPI：数据快照周更

> **gqf2008** (2026-09-06):
>
> 销售/运营条线 issue 清理：按用户指示关闭归档。注：GitHub 不支持通过 API 删除 issue，关闭即终态。

---

## #66 [销售] 价值主张一页纸
- 状态：closed　创建：2026-08-28　标签：销售,P0

编写 Verba 价值主张一页纸（定位/卖点/目标客户/商业模式）。

角色：企业销售（韩非 @verba-hanfei）
KPI：一页纸完成并进入试点沟通

> **gqf2008** (2026-08-28):
>
> 【协同规范·执行确认】已认领 ✅
> - assignee：gqf2008（韩非 @verba-hanfei bot 账号未注册，暂由 Owner 账号代认领留痕，账号就绪后改派）
> - labels：销售 + 优先级（已打）；看板：[Verba 虚拟团队看板](https://github.com/users/gqf2008/projects/3) Status=In Progress、部门=销售、优先级=已设
> - 产出规则：改动用 PR 提交并关联本 issue，合并才算完成；有进展在评论留痕。
> 
> 老板指令（08-28）：今日输出价值主张一页纸（定位/卖点/目标客户/商业模式），24h 内需 PR 或看板进展。

> **gqf2008** (2026-08-28):
>
> 【晚间跟进 · 2026-08-28 21:55】拾言总管定时巡检：本 issue 当前状态 = open / 看板 In Progress，仅完成认领留痕。截至巡检时刻，仓库全天 0 commits、无关联 PR、无交付物评论。老板 08-28 指令要求 24h 内 PR 或看板进展，请韩非尽快提交产出（改动用 PR 关联本 issue，合并才算完成）。

> **gqf2008** (2026-09-06):
>
> 销售/运营条线 issue 清理：按用户指示关闭归档。注：GitHub 不支持通过 API 删除 issue，关闭即终态。

---

## #67 [销售] 定价 v1 草案（Discussion 征求意见）
- 状态：closed　创建：2026-08-28　标签：销售,P1

起草定价 v1（免费核心/个人高级/企业版/渠道），挂 GitHub Discussion 征求意见。

角色：企业销售（韩非 @verba-hanfei）
KPI：定价草案挂出并收集反馈

> **gqf2008** (2026-08-28):
>
> 【协同规范·执行确认】已认领 ✅
> - assignee：gqf2008（韩非 @verba-hanfei bot 账号未注册，暂由 Owner 账号代认领留痕，账号就绪后改派）
> - labels：销售 + 优先级（已打）；看板：[Verba 虚拟团队看板](https://github.com/users/gqf2008/projects/3) Status=In Progress、部门=销售、优先级=已设
> - 产出规则：改动用 PR 提交并关联本 issue，合并才算完成；有进展在评论留痕。
> 
> 老板指令（08-28）：定价 v1 草案已挂 Discussion #68 征求意见，收集反馈后转正，24h 内需看板进展。

> **gqf2008** (2026-09-06):
>
> 销售/运营条线 issue 清理：按用户指示关闭归档。注：GitHub 不支持通过 API 删除 issue，关闭即终态。

---

## #72 [销售] 首批试点线索池（5–10 条企业用户）
- 状态：closed　创建：2026-08-28　标签：销售,P0

从社区种子用户中筛选企业用户，建首批 5–10 条试点线索池（线索名/联系人/场景/意向等级/下一步）。

角色：企业销售（韩非 @verba-hanfei）
KPI：线索池初稿今日输出，线索 → 商机 → POC → 试点签约转化

> **gqf2008** (2026-08-28):
>
> 【协同规范·执行确认】已认领 ✅
> - assignee：gqf2008（韩非 @verba-hanfei bot 账号未注册，暂由 Owner 账号代认领留痕，账号就绪后改派）
> - labels：销售 + 优先级（已打）；看板：[Verba 虚拟团队看板](https://github.com/users/gqf2008/projects/3) Status=In Progress、部门=销售、优先级=已设
> - 产出规则：改动用 PR 提交并关联本 issue，合并才算完成；有进展在评论留痕。
> 
> 老板指令（08-28）：今日输出首批试点线索池初稿（从社区种子用户筛 5–10 条企业用户），24h 内需 PR 或看板进展。

> **gqf2008** (2026-08-28):
>
> 【晚间跟进 · 2026-08-28 21:55】拾言总管定时巡检：本 issue 当前状态 = open / 看板 In Progress，仅完成认领留痕。截至巡检时刻，仓库全天 0 commits、无关联 PR、无交付物评论。老板 08-28 指令要求 24h 内 PR 或看板进展，请韩非尽快提交产出（改动用 PR 关联本 issue，合并才算完成）。

> **gqf2008** (2026-09-06):
>
> 销售/运营条线 issue 清理：按用户指示关闭归档。注：GitHub 不支持通过 API 删除 issue，关闭即终态。

---

## #73 [销售] 企业 POC 演示包
- 状态：closed　创建：2026-08-28　标签：销售,P1

准备企业 POC 演示包：安装包 + 定制清单（功能/品牌/合规选项）+ 数据安全说明（本地处理/日志脱敏/权限边界）。

角色：企业销售（韩非 @verba-hanfei）
KPI：演示包就绪，进入试点沟通

> **gqf2008** (2026-08-28):
>
> 【协同规范·执行确认】已认领 ✅
> - assignee：gqf2008（韩非 @verba-hanfei bot 账号未注册，暂由 Owner 账号代认领留痕，账号就绪后改派）
> - labels：销售 + 优先级（已打）；看板：[Verba 虚拟团队看板](https://github.com/users/gqf2008/projects/3) Status=In Progress、部门=销售、优先级=已设
> - 产出规则：改动用 PR 提交并关联本 issue，合并才算完成；有进展在评论留痕。
> 
> 老板指令（08-28）：企业 POC 演示包（安装包+定制清单+数据安全说明）推进，24h 内需看板进展。

> **gqf2008** (2026-08-28):
>
> 【晚间跟进 · 2026-08-28 21:55】拾言总管定时巡检：本 issue 当前状态 = open / 看板 In Progress，仅完成认领留痕。截至巡检时刻，仓库全天 0 commits、无关联 PR、无交付物评论。老板 08-28 指令要求 24h 内 PR 或看板进展，请韩非尽快提交产出（改动用 PR 关联本 issue，合并才算完成）。

> **gqf2008** (2026-09-06):
>
> 销售/运营条线 issue 清理：按用户指示关闭归档。注：GitHub 不支持通过 API 删除 issue，关闭即终态。

---

## #74 [前端][P0] issue #44 真机两步余项收口（修饰键守卫 + 流 token 代际）
- 状态：closed　创建：2026-08-28　标签：前端平台,P0

关联：issue #44（已关闭，实现已合 118725c）；真机两步余项在 docs/manual-acceptance-windows.md 跟踪。

**目标**：Windows TSF 真机验证两项收口：
1. 修饰键守卫：VS Code Ctrl+. / Ctrl+, 动作触发不被吞；记事本按住 Ctrl 连打字母无残留组合/无候选窗/无卡死；正常输入回归不受影响。
2. 两段式派发 / 流 token 代际：连做 5 轮 `//` 提示词→开流→流未结束即 Esc/切窗取消→立即再开新流；每轮上屏文本完整属于最新一轮（无旧流 chunk 混入）；daemon 日志无 panic/死锁。慢网络下开流后取消无卡死（可选）。

负责人：前端平台（顾晨）。看板状态：开发中。

> **gqf2008** (2026-08-28):
>
> **认领**：前端平台（顾晨）负责。已入看板：Verba M6 交付看板（https://github.com/users/gqf2008/projects/4）状态=开发中，负责人=顾晨。方案/口径见 Discussions #77。进展将在此评论留痕。

> **gqf2008** (2026-08-29):
>
> 【进展留痕 2026-08-29】#74 真机两步进行中：
> - 前置 P0 阻塞：main CI Format 5 job 红（run 33234518956），已提修复 PR #80（rustfmt 1.98.0 对齐，本地三 workspace --check 全过）。
> - 真机两步（VS Code Ctrl+. / Ctrl+, 守卫 + 快速流切换无旧流混入 5 轮）按 manual-acceptance-windows 清单推进，完成后本 issue 逐项留痕。

> **gqf2008** (2026-08-29):
>
> 【进展留痕 2026-08-29】CI P0 阻塞已清零，真机两步可开跑：
> - main CI 全绿（PR #80 合并 44202fe；run 33239541323 success）——Format/clippy/test 三平台 + 双前端全绿。
> - #74 两项（VS Code Ctrl+. / Ctrl+, 守卫、快速流切换无旧流混入 5 轮）按 manual-acceptance-windows 真机执行，结果本 issue 留痕。

> **gqf2008** (2026-09-06):
>
> 🔄 [处理中][wt-待定] 2026-09-06 恢复推进：真机环境核毕（安装版 DLL=Program Files 9/4 构建生效；LLM=DeepSeek 正式链路已配；TSF/ctfmon 正常）。按 manual-acceptance-windows 逐项执行并在本 issue 留痕。已知缺口：VS Code 未安装 → Ctrl+. / Ctrl+, 子项待补。

> **gqf2008** (2026-09-06):
>
> ## 收口记录（2026-09-06 真机）
> 
> **第 1 项 修饰键守卫**
> - ✅ 记事本：Ctrl+A/C/V 在 Verba 激活态均为 `OnTestKeyDown ... claim=false`（未被 IME 吞），应用动作经剪贴板往返验证生效；按住 Ctrl 连打字母无残留组合/无候选窗/无卡死；`nihao`+空格→「你好」正常上屏（回归 OK）
> - ✅ VS Code：Verba 激活态按 `Ctrl+,`，设置页真实打开（窗口区域 44.7% 像素变化）——快捷键未被吞、动作触发
> - ⏳ `Ctrl+.`（Quick Fix）：未在含诊断的代码上下文目验弹窗（当前文件无 code action；机制同 Ctrl+标点已被 Ctrl+, 证明）。**非阻塞待补**
> 
> **第 2 项 两段式派发 / 流 token 代际（当前 main 构建）**
> - ✅ 前 4 轮「开流→1s 后 Esc 取消→立即再开」：每轮 `StartLlm → Cancel(state=Streaming)`，无 panic/死锁
> - ✅ 第 5 轮完整生成：`AI 结果就绪: chars=257` → `CommitResult` 提交完整「月球六事实」，与第 5 轮提示对应，**无旧流混入、无空结果**
> - ✅ 安装版（9/4）曾复现的「完整生成轮空结果 + 空串上屏」在当前 main 不复现（#98 空 Enter 防误杀 + #44 代际修复生效）
> 
> **环境/恢复**
> - 测试采用当前 main 构建（HKCU CLSID 临时覆盖），已撤销恢复安装版；测试记事本（含月球事实文本）保留供你查看

> **gqf2008** (2026-09-06):
>
> ✅ 两项已真机收口（唯一待补：VS Code Ctrl+. 在含 code action 处目验 Quick Fix，非阻塞；机制已由 Ctrl+, 与记事本 Ctrl 组合证明）。

---

## #75 [前端][P1] 截图/听写/朗读热键链路实机验收（Windows TSF）
- 状态：closed　创建：2026-08-28　标签：前端平台,P1

关联：roadmap M3（target_dev16 DLL 待实机验收）。

**目标**：Windows 真机验证热键链路：
- `//截图` / Ctrl+Alt+O：选区 OCR（region-ocr 拖选→OCR 上屏，失败回退全屏）
- `//听写` / Ctrl+Alt+M：录音 ASR（verba-trigger 麦克风→WAV→daemon ASR→上屏）
- `//朗读 <文本>`：TTS 合成播放（edge-tts / OpenAI）

负责人：前端平台（顾晨）。看板状态：开发中。

> **gqf2008** (2026-08-28):
>
> **认领**：前端平台（顾晨）负责。已入看板：Verba M6 交付看板（https://github.com/users/gqf2008/projects/4）状态=开发中，负责人=顾晨。方案/口径见 Discussions #77。进展将在此评论留痕。

> **gqf2008** (2026-08-29):
>
> 【2026-08-29 Owner 范围决策】范围调整：\//截图\ / Ctrl+Alt+O（选区 OCR）**保留**验收；\//听写\ / Ctrl+Alt+M、\//朗读 <文本>\ **冻结为实验性**（代码保留、默认关闭、入口隐藏、不承诺），不在 M6 验收范围。请按新范围更新验收清单（顾晨）。文档同步见 PR #78。

> **gqf2008** (2026-09-06):
>
> 🔄 [处理中][wt-待定] 2026-09-06 恢复推进：按 Owner 新口径仅验 OCR（//截图 / Ctrl+Alt+O），ASR/TTS 冻结不验收。真机执行并留痕。

> **gqf2008** (2026-09-06):
>
> ## 收口记录（2026-09-06 真机）
> 
> **验收结果（OCR 链路）**
> - ✅ `//截图`（全屏 OCR）：真机触发 → `触发命令: OcrFullScreen` → RapidOCR 识别成功 `OCR 结果进预览: chars=1473`
> - 🐞 发现并修复真实 bug：OCR 结果进预览后，机器 `state` 仍为 `Idle`，`should_claim_key` 只按 state 判定 → Enter/Esc/空格/1 不认领，`OnKeyDown` 不回调，预览分支死代码——预览可见但无法上屏/取消（Enter 透传成换行）。修复 PR #103 → main c04ed92
> - ✅ 修复后：预览态认领控制键+可打印键，英文模式非预览键透传宿主；本地门禁全绿（fmt / clippy -D warnings / 41 unit + 9 tsf_smoke 通过）
> 
> **范围说明**
> - 按 Owner 口径，`//听写`/Ctrl+Alt+M、`//朗读 <文本>`（ASR/TTS）冻结为实验性，不在本次验收范围
> - Ctrl+Alt+O 选区拖选（region-ocr）的真机拖选交互未自动化；全屏 `//截图` 链路已真机闭环
> 
> 清理：worktree win75-ocr-preview 已移除，分支 fix/win-ocr-preview-claim（本地+远端）已删。

> **gqf2008** (2026-09-06):
>
> ✅ OCR 全屏链路真机通过；发现并修复 OCR 预览态按键认领 bug（PR #103 → main c04ed92）。ASR/TTS 按口径冻结不验收。

---

## #76 [前端][P1] 三端功能对齐矩阵 + macOS 手动验收清单文档（docs）
- 状态：closed　创建：2026-08-28　标签：documentation,前端平台,质量测试,P1

关联：issue #60（macOS IMK 验收收尾）；roadmap M2 遗留项。

**目标**（文档产出，走 PR）：
1. docs/manual-acceptance-macos.md：macOS IMK 手动验收清单（候选窗自动展示、输入法切换、TIS 注册、Enter/翻页/←→/退格/Esc、TCC 权限弹窗、`//` AI 流式）。
2. 三端功能对齐矩阵（Windows TSF / macOS IMK / Linux 计划）：直输、preedit、候选窗、AI 模式、多模态触发、主题/分页对齐情况。

负责人：前端平台（顾晨）。方案先行：见 Discussions「三端交互验收方案征求意见」帖。

> **gqf2008** (2026-08-28):
>
> **认领**：前端平台（顾晨）负责。已入看板：Verba M6 交付看板（https://github.com/users/gqf2008/projects/4）状态=开发中，负责人=顾晨。方案/口径见 Discussions #77。进展将在此评论留痕。

> **gqf2008** (2026-08-29):
>
> 【QA 认领·许衡】三端对齐矩阵+macOS 清单 docs：认领验收口径与门禁，同步 M6 新范围（OCR=正式、ASR/TTS=冻结、性能预算=LLM+OCR）。

> **gqf2008** (2026-08-29):
>
> 【进展留痕 2026-08-29】PR #79（docs：测试计划 + 三端验收清单）已自审通过：
> - 与 QA 回帖口径一致（OCR 正式 / ASR·TTS 冻结负向 / 性能=LLM+OCR / 门禁 P0/P1+CI 全绿）。
> - main CI Format 阻塞由 PR #80 修复，绿后 rebase 本 PR 合并 → Closes #76。

> **gqf2008** (2026-08-29):
>
> @许衡 认领验收口径与门禁（assignee=gqf2008 共用账号，PR #79 已合并）。

---

## #81 [AI][P1] OCR 正式能力收尾（模型打包/解码修复/看图链路 + 验收口径）
- 状态：closed　创建：2026-08-29　标签：AI多模态,P1

### 背景
2026-08-29 范围决策（D#71 评论 / PR #78 #79）确定：OCR（截图/看图）= 正式能力、正式验收；ASR/TTS 冻结为实验性（代码保留、默认关闭、入口隐藏）。

### 现状（代码已落 main，08-29 傍晚直推）
- PP-OCRv5 模型打包进安装包（免首次下载）
- 识别前统一解码为 RGB，修 PNG 误存 .bmp 的解码失败
- OCR 解码路径回归测试（IME BMP 32bpp top-down / PNG / 非法字节）
- 安装器 OCR 模型 Files 条目 recursesubdirs 修复
- Ctrl+Alt+S 打开设置面板（移除听写热键）

### 缺口
无 issue 跟踪 OCR 正式能力的 assignee / 验收 / 性能数据；#58 性能预算表含 OCR 链路。

### 验收标准（checklist）
- [ ] 安装包内置 OCR 模型，首次使用免下载
- [ ] 截图 / 看图 OCR 链路实机通过（Windows TSF + macOS ScreenCaptureKit 权限）
- [ ] PNG / 非 BMP 输入解码正常，回归测试全绿
- [ ] OCR 性能数据回填 #58 预算表
- [ ] 与 #75 截图热键链路验收联动

### 关联
Related #58 #75；Closes 范围决策落盘项（OCR=正式能力）

> **gqf2008** (2026-08-29):
>
> @叶澜（AI 多模态）请认领：OCR 已升正式能力（2026-08-29 范围决策），代码已落 main（PP-OCRv5 打包/解码修复/回归测试），本 issue 跟踪验收/看图链路/性能数据回填 #58。assignee=gqf2008 共用账号代指派，按 #79 口径执行。

> **gqf2008** (2026-09-07):
>
> 🔄 [处理中][wt-待定] 2026-09-07 恢复推进：Windows 侧逐项收口（打包/回归/实机/性能），macOS ScreenCaptureKit 需 mac 真机单独补。

> **gqf2008** (2026-09-07):
>
> ## 收口记录（2026-09-07）
> 
> ### 验收清单逐项
> 1. ✅ **安装包内置 OCR 模型**：installer `verba-ime.iss` 已打包 `vendor/ocr/*`（ch_PP-OCRv5 det/rec + dict）→ `{app}\models-rapidocr`（recursesubdirs）；安装目录与 daemon 查找路径一致
> 2. ⚠️ **截图/看图 OCR 链路实机**：Windows TSF 已通过（#75：`//截图` 全屏 OCR 1473 字；#74 过程中 eye 区域 OCR `ocr_len=85` 正常注入）；**macOS ScreenCaptureKit 权限需 mac 真机补验**（本机为 Windows，无法执行）
> 3. ✅ **PNG/非 BMP 解码回归**：`cargo test -p verba-ocr` 10/10 通过，含 `decode_rgb_accepts_ime_bmp_and_png`
> 4. ✅ **OCR 性能数据回填 #58**：已回填（720p 1976ms 达标；1080p/4K 3909/6099ms 超预算，见 #58 评论）
> 5. ✅ **与 #75 联动**：#75 已收口，并发现修复 OCR 预览态按键认领 bug（PR #103）
> 
> ### 遗留
> - macOS ScreenCaptureKit 权限验收（需 mac 真机）
> - OCR 1080p/4K 性能超预算（已在 #58 记录，建议降采样/分档）
> 
> 清理：无 worktree/分支残留。

> **gqf2008** (2026-09-07):
>
> ✅ Windows 侧收口完成（打包/回归/实机/性能数据）；遗留 macOS ScreenCaptureKit 权限需 mac 真机、OCR 1080p/4K 性能超预算已回填 #58 跟踪。

---

## #82 feat(capture): 选区截图 OCR 跨平台统一——winit 选区 + xcap 截屏共享层，macOS 补 /// 触发
- 状态：closed　创建：2026-08-31　标签：—

## 背景

v0.2.4 的 `///` 选区截图只有 Windows 实现（Win32 遮罩 343 行 + BitBlt 201 行 + verba-trigger.exe）；macOS 端 `Action::TriggerOcr` 留待（imk.rs:1008 只清组合）。

**架构指令（用户 2026-08-31）**：所有需求默认跨平台实现，仅平台硬限制才独立实现；禁止按平台各拼一套（如 macOS shell 出 screencapture）。选 Rust 的目标就是跨平台。

## 方案

新建根 workspace 共享 crate `crates/verba-trigger`（从 frontends/windows/ime 迁出）：

- **selection.rs → winit**：透明全屏 borderless 窗 + 鼠标拖选矩形，单代码库（跨平台库内部消化 Win32/AppKit 差异）
- **capture.rs → xcap**：跨平台截屏（Win32 GDI / macOS CoreGraphics / Linux X11），替换 BitBlt
- **record/play**：cpal + rodio 原样迁入（本就跨平台）
- CLI `verba-trigger region-ocr` 同名同参，两端 spawn 同一二进制

前端接线（文本提交是真平台限制，允许差异）：
- Windows TSF：spawn 逻辑不变（verba-trigger.exe）
- macOS IMK：TriggerOcr → spawn verba-trigger → OcrRecognize IPC → insertText 提交；首次触发走系统「屏幕录制」授权

打包：package.sh 装 verba-trigger 进 Verba.app/MacOS；ISS 路径不变（target/release 产物同名）。

## 验收

- [ ] verba-trigger 在 Windows + macOS 均可构建、region-ocr 全链路出字
- [ ] macOS `///` 触发系统选区 → OCR → 上屏
- [ ] Windows 行为无回归（现网路径不变）
- [ ] CI 三平台绿；打包含 verba-trigger（.app + ISS）

> **gqf2008** (2026-08-31):
>
> 代码侧验收全绿（PR #83 合并，eb7a6fe）：
> 
> - [x] verba-trigger 三平台可构建（CI 绿：根 workspace check/test ×3 OS + 前端 job）；本机 region-ocr 全链路出字（xcap 截屏 → daemon OCR → 文本）
> - [x] macOS `///`：TriggerOcr → spawn verba-trigger → 结果槽位 → 主线程剪贴板 + 候选窗预览（Enter/空格/1 上屏、Esc 取消）
> - [x] Windows 无回归（text_service 仅导入路径替换；spawn/安装布局不变）
> - [x] CI 三平台绿（ubuntu 依次补齐 libasound2-dev / libclang-dev / libpipewire-0.3-dev / libgbm-dev —— verba-trigger 依赖链的 Linux 系统库）
> - [x] 打包：package.sh 装 verba-trigger；ISS 源路径根 target；CI .app 校验含 verba-trigger
> 
> 独立审查两项必须修复均已落实：①Retina 坐标断裂（xcap 边界=点 vs 图像=物理像素，复合画布改最近邻采样，真机 1470×956/2940×1912 验证）；②ubuntu CI 依赖链补齐。另落实审查建议：daemon spawn stdio 落 null（防 .output() 悬挂）、坏 --rect 报错不落交互。
> 
> **真机验收待用户执行**：装新 DMG（或本机构建包）后，切到拾言输入法按 `///` 划选 → 首次弹「屏幕录制」授权点允许 → OCR 文本进候选窗预览 → Enter 上屏。验收通过后关闭本 issue。

> **gqf2008** (2026-08-31):
>
> **macOS 端全链路已通，覆盖层缺陷修复**（PR #84，44fabe4）：
> 
> 真机联调发现并修复：① softbuffer 缓冲点尺寸被 Retina 物理尺寸裁掉右下（只盖左上 1/4）→ 缓冲改物理尺寸 + scale 查表采样；② winit macOS fullscreen API 假成功（状态报成功、窗口不动）→ 改 monitor 物理矩形摆位 + set_simple_fullscreen（resumed 内调用，RedrawRequested 内调用被吞）；③ CapsLock 裸 toggle 与系统「CapsLock 切 ABC」打架致输入法整体静默失灵 → revert；④ 预览槽粘滞吞键 → 会话边界强制清理 + 非预览键退出预览重走拼音路由；⑤ 大段识别文本候选显示截断 40 字符；⑥ OCR spawn 单实例守卫。
> 
> 真机验证：`///` → 全屏遮罩 → 划选 → OCR 1059 字符 → 候选窗预览 → Enter 上屏；拼音输入完全恢复（li→了）。
> 
> 剩余：Windows 侧无回归（交叉 check 绿）；等用户最终验收后关闭本 issue。

---

## #87 [核心][P0] 普通打字漏字：盲窗暂缓单槽吞键
- 状态：closed　创建：2026-09-03　标签：bug,核心引擎,P0

## 现状
快速输入时收尾键被静默吞掉。根因：`crates/verba-core/src/machine.rs` 的盲按窗口用**单槽** `deferred_intent: Option<DeferredIntent>` 暂缓空格/大写/标点，三处写入（L460/L487/L832）**无条件覆盖**——连按两个收尾键，前一个被吞。测试 `latest_intent_replaces_pending`（L2279）把它固化为「可接受损失」。打字越快、Rime IPC 越慢越频繁。

## 目标
盲窗内任意序列的收尾键一个不丢，settle 后按 FIFO 顺序正确上屏。

## 范围
- core：`deferred_intents: VecDeque` + FIFO 重放（**队首保留 has_real 语义、后续重喂键**——两条重放路径是不同契约，合并会重新引入漏字）；`on_llm_candidates` 返回 `Vec<Action>`；`MAX_DEFERRED=16` 上限（超限转即时提交，绝不丢最旧）；双击回退改看队尾（`back()` 而非 `contains()`）；重放前 `drain` 快照（防 `clear_composition_state` 自清）
- 两端前端：`collect_steps` / `feed_candidates_event` 单动作 match 改动作序列循环
- 测试：删除并替换 `latest_intent_replaces_pending`（**语义变更非回归**），新增约 12 条队列测试

## 验收标准
- [ ] 快速连打「拼音+空格+标点」「拼音+标点+标点」等组合零漏字（真机）
- [ ] 现有盲窗测试改写后全绿；增长缓冲语义（`deferred_space_settles_against_grown_buffer`）不变
- [ ] cargo fmt / clippy -D warnings / test 全绿

---

## #88 [前端][P1] macOS 菜单栏输入源图标过大（撑爆菜单栏）
- 状态：closed　创建：2026-09-03　标签：bug,前端平台,P1

## 现状
macOS 菜单栏输入源图标把菜单栏顶高。根因：`frontends/macos/ime/app/Resources/Icon.pdf` 的 `MediaBox` 为 **144×144 pt**，而菜单栏高度仅 ~22pt。该 PDF 为手写（无 SVG 源；`assets/branding/README.md` 里的 `icon.svg` 是规划中、实际不存在）。

## 目标
菜单栏图标恢复正常高度，深浅色（template）显示正常。

## 范围
- 新增 `scripts/gen-macos-icon.py`：由原始 144pt 气泡剪影几何按目标画布重新缩放导出合法单页 PDF（自动计算 `/Length` 与 `xref` 偏移——仓库曾为手写 PDF 的 `/Length` 单独修过一次，见 commit b57c08b）
- `MediaBox` 缩到 18×18 pt，字形居中填满、留薄边；`TISIconIsTemplate=true` 保持不动

## 验收标准
- [ ] 真机：菜单栏高度恢复正常、深浅色显示正常
- [ ] `package.sh` 重打包 + CI 冒烟（ci.yml 已断言 `Contents/Resources/Icon.pdf` 存在）

---

## #89 [AI][P1] AI 交互重做：结果浮层 / 单键确认 / 可改可重试
- 状态：closed　创建：2026-09-03　标签：AI多模态,P1

## 现状（用户实测三大痛点）
1. LLM 流式长结果挤在窄 preedit 里看不清（两端都整串灌进内联组合串，无换行无长度限制）
2. 要按两次 Enter，且 ResultReady 态按 Space/1 被前端吞掉无反馈
3. 不能改 prompt、不能重试、结果是替换式上屏无候选（prompt 在 `feed_enter` 被清空，无重试基础）

审查另发现两处**动作发出但无人接住**：Windows `apply_action` 的 `Action::ResultReady` 是空实现（L1026）；`collect_steps` 的 Final 分支只 push `RewriteReady`，`ResultReady` 被静默丢弃（L2071-2078）。

## 目标
AI 结果不再是 preedit 内容，而是**结果浮层**（复用改写对照预览范式）；一次确认上屏；失败/就绪后可重试（r）、可回去改 prompt（e）。

## 范围（分 4 个提交）
1. core 状态机：新增 `MachineState::Failed`、`last_prompt`、`ai_preview`（**由 core 自行置位**，防两跳缺一）；preedit 改短状态串（绝不可为空——Notepad-- 实测置空会终止组合吞掉整条流）；`AiKey` + `feed_ai_preview`；按键映射放 core；`on_llm_error` 不再回 Idle/清 prompt
2. 新增 `crates/verba-core/src/commands.rs` 统一 `//` 命令解析（现散落 Windows 前端，macOS 完全没有）
3. verba-candidate 多行结果块：`draw_text` 已支持多行，补高度模型；`measure_lines` 预测量 + `window_size`/`render` **共用同一 result_height 公式**（两公式漂移是本改动最大风险）
4. Windows 前端（补 ResultReady 实现 + collect_steps 不丢弃 + 浮层拦截 + chunk 节流）；macOS 前端一期（显示截断/提交取全文，照抄 OCR 预览纪律；删 706-740 重复段）

## 验收标准
- [ ] 长结果可读（Windows 多行块；macOS 截断+状态行）
- [ ] Streaming/Ready/Failed 三态按键语义：Enter/空格/1 上屏、r 重试、e 改提示词、Esc 取消
- [ ] 改写流对照预览不回归；Notepad-- 流式不被吞
- [ ] cargo fmt / clippy -D warnings / test 全绿

---

## #90 [前端][P0] CapsLock 跟随式中英切换：代码审查修复批次（6 项）
- 状态：closed　创建：2026-09-03　标签：bug,前端平台,P0

## 现状
`/code-review max` 对 `7515fee..HEAD`（CapsLock 跟随式中英切换）的审查发现 13 条，其中 6 项拟修复（正确性 3 条 CONFIRMED + 死代码/惯例 3 条），全部已逐条对照代码核实属实。

## 批次清单（同文件同机制，一批一 PR）
- [ ] **F1** caps 分支在 `had=false` 时只透传不清组合/流（宿主 marked 残留、16ms 定时器继续写回流式 chunk、关 caps 后空格提交陈旧候选），且位于 `drain_stream` 之后（OCR 预览同帧被销毁、积压键无 caps 复查）→ 补 `clear_composition()` + `reset()` 并整体前移到 drain 之前
- [ ] **F2** 删 L706-744 不可达重复预览块（上方 L651-705 块的所有分支都 return 或清槽，落到该块时两 ivar 恒 None）
- [ ] **F6** 提取 `clear_previews()` helper，收敛 6 处双槽同清点
- [ ] **F7** 改写 L644-648 过时注释（与 L632 的 caps 分支自相矛盾）
- [ ] **F10** `'2'` 仅在 `rewrite_preview` 为 Some 时算 pick；OCR 预览按 2 从「上屏识别文本」改为「取消并重走路由」，对齐 Windows 语义（**行为变更，已确认**）
- [ ] **F11** Esc 臂 `refresh_candidate_window()` → `hide_candidate_window()`（空候选时 refresh 直接 return 不隐藏，姊妹路径 b9de75d 已修同款）

## 不采纳
- modifierFlags 每次按键查询：刻意语义，非缺陷。

## 验收标准
- [ ] 系统「CapsLock 切换 ABC」开启下切换输入源，预览跨 caps 切换不粘滞、组合/流被正确取消
- [ ] F10 后 OCR 预览按 2 的新行为过一遍手动验收
- [ ] cargo fmt / clippy / test 全绿（macOS 前端为独立 workspace，在其目录内验证）

---

## #101 [前端][P2] OCR 预览锚点热键陈旧复用收口 + 状态串空串防御 + 锚点消费单测（v0.2.8 批次交互审查跟进）
- 状态：closed　创建：2026-09-06　标签：bug,前端平台,P2

## 背景
v0.2.8 批次（PR #98）交互复审发现 1 条 P2 + 2 条 P3，本 issue 收口为一个修复 PR（仅改 `frontends/windows/ime/src/text_service.rs`）。

## 范围
- P2：OCR/触发结果浮窗锚点跨触发陈旧复用未闭合
  - `show_ocr_preview` 的 `take()` 只保证「一次 stash 至多消费一次」，不保证「消费的是本次触发的锚点」。
  - 命令路径（`///` / `//截图` / `//听写`）stash 后存在 4 条不消费路径（结果到达时机非 Idle 直上屏兜底 / 识别文本为空 / 用户取消选区 / 采集失败无结果），残留陈旧锚点。
  - 随后无组合热键 Ctrl+Alt+O 触发不 stash 也不清槽 → `take()` 拿到旧光标位置，卡片仍可能甩到远处（正是本批要消除的缺陷门）。
  - 修法：热键触发入口（`handle_key_down` 的 `TriggerKind::Ocr` 分支）清空锚点槽，使无 stash 路径恒落视图兜底；抽 `take_ocr_anchor` 纯消费函数。
- P3a：`set_preedit_streaming_status` 缺空串防御（Idle 态 preedit() 为空会踩 Notepad-- 空组合陷阱 → 机器重置 + cancel_stream）；空串守卫下沉到唯一写点。
- P3b：OCR 锚点消费/清槽语义补单测（一次性消费、陈旧残留不被热键复用）。

## 验收标准
- [ ] 热键 Ctrl+Alt+O 触发前锚点槽被清空；空槽预览落视图兜底而非旧光标
- [ ] stash 锚点恰被消费一次，二次取落兜底
- [ ] `set_preedit_streaming_status` 空串直接返回，不触碰 TSF
- [ ] 本地门禁绿：`frontends/windows/ime` 内 fmt / clippy -D warnings / test --all-targets
- [ ] 单职责提交 + PR 关联本 issue

## 参考
- 复审记录见 v0.2.8 批次交互审查（PR #98 合并范围 4b024d5..359ac40）
- 真机 2026-09-05 `///` 全链路正常却感知「啥也没看到」的定位主嫌疑（锚点）

> **gqf2008** (2026-09-06):
>
> 🔄 [处理中][wt-win-ocr-anchor] 分支 fix/win-ocr-anchor-stale 承接本批修复（worktree: .claude/worktrees/win-ocr-anchor）。

> **gqf2008** (2026-09-06):
>
> PR #102 已开（branch fix/win-ocr-anchor-stale，1 commit 2e3aca2）：本地门禁全绿（fmt / clippy -D warnings / test --all-targets，新增 2 单测通过），待 CI + 独立审查后合并。

> **gqf2008** (2026-09-06):
>
> ✅ 已合并（PR #102 → main c51981a，2026-09-06）。
> 
> - 独立审查：通过（无 P0/P1/P2；4 条 P3 可选打磨项已记录，不阻塞，留待后续）
> - 本地门禁全绿：fmt / clippy -D warnings / test --all-targets
> - 清理完成：worktree win-ocr-anchor 已移除，分支 fix/win-ocr-anchor-stale（本地+远端）已删除
> - [wt-win-ocr-anchor] 处理中标记移除

---

## #105 follow-up(win): OCR 预览批次未修的 6 项（真机决策/结构性重构）
- 状态：open　创建：2026-09-07　标签：—

源自 PR #104 的 /code-review max 批次，以下 6 项**记录未修**，各有明确不修理由，需真机验证或产品决策后另开 PR：

1. **死键 ToUnicodeEx 冲刷**：get_char_for_vk 忽略 ToUnicodeEx 返回 -1（死键挂起）并用 oem_fallback 替换——英文态预览下死音键+字母会吞掉组合（US-Intl/捷克/AZERTY）。不修原因：honoring -1 会破坏刻意的 oem_fallback 契约（保证 '/' 在全布局可触发），需真机决策。
2. **Chromium 类宿主 TRUE→FALSE 透传兼容**：可打印键先 TRUE 认领后 FALSE 透传，依赖宿主在 sink 让位后重处理——Electron/Chromium 编辑器有吞键风险类，需真机矩阵验证。
3. **焦点切换预览失效策略**：OnSetFocus 只记日志，A 窗格武装的预览在 B 窗格数分钟后的 Enter 砸旧文本；「清理 vs 重锚 vs 保持」是产品决策（建议预览 TTL 或焦点失效）。
4. **MachineState::OcrPreviewing 单状态化重构**：预览态活在 MachineState 之外，每个消费者要记第二个正交信号（本批 on_timer 漏判即此形状的第一例；ai_previewing 是第三个）。根治=加状态变体（begin 门改 matches!(Idle|OcrPreviewing)，core 仅 4 处穷尽 match），超出 #104 批次范围。
5. **中/英状态卡渲染**：status-only payload 过不了 should_render 门——状态卡从未真正显示过，唯一效果是破坏性隐藏（正是 #104 修的预览孤儿根因之一）。需 verba-candidate 契约变更，且修好渲染会与预览像素互踩（同不可见状态类），需设计。
6. **（记录）提交粒度惯例**：本批把第二处行为变更并入审查修复提交——已在提交信息显式记录，后续审查批次注意。

优先级建议：4（结构性）> 3（数据正确性事故面）> 5（可见性）> 1/2（布局矩阵，需 Windows 真机）。

> **gqf2008** (2026-09-08):
>
> 🔄 item4（MachineState::OcrPreviewing 单状态化重构）由 [wt-ocr-refactor] 处理中：移出独立正交信号，加 OcrPreviewing 状态变体，core 4 处穷尽 match，行为等价（同 e2e/snapshot diff 为空 + cargo test 全绿）。其余 item1/2 需 Windows 真机、item3/5 需产品决策、item6 为提交粒度注记，暂挂起。

> **gqf2008** (2026-09-08):
>
> ✅ item4（MachineState::OcrPreviewing 单状态化）已完成 → PR #109。core 103 单测 + macOS 14 测试全绿；Windows 由 frontend-windows CI 兜底。item1/2（真机矩阵）、item3/5（产品决策、verba-candidate 契约）、item6（提交粒度注记）仍挂起。

> **gqf2008** (2026-09-08):
>
> ⚠️ 复审查出 Critical（macOS 只用自有 ocr_preview ivar、不复位核心 OcrPreviewing，Accept/取消后状态滞留吞键）已在 PR #109 修复（imk.rs commit/落回路径调 machine.end_ocr_preview() + core 回归测试）。verba-core 104 单测 + frontend-macos 14 单测全绿、clippy/fmt 干净。

> **gqf2008** (2026-09-08):
>
> ## 剩余 5 项拆解与决策建议（item4 已由 PR #109 收口，以下为余项）
> 
> **item3 焦点切换预览失效策略**（需产品决策）
> - 现状：`OnSetFocus` 只记日志，A 窗格武装的 OCR 预览在 B 窗格数分钟后按 Enter 会砸旧文本。
> - 选项：
>   - **A（推荐）预览 TTL**：预览带上时间戳，超时（如 10s）自动失效；防御性最好、实现小（core 加 ttl + 前端 on_timer 检查）。
>   - **B 焦点失效**：焦点离开时清预览（`clear_previews`）；简单直接，但跨窗格复制/粘贴场景会误清。
>   - **C 保持现状**：接受该已知缺陷，验收清单标注为已知限制。
> - 建议：A（TTL）兼顾安全与体验，需 Windows/macOS 真机各验一次。
> 
> **item5 中/英状态卡渲染**（需产品决策 + verba-candidate 契约）
> - 现状：`should_render() = visible && (有候选 || 有结果块)`，status-only 卡过不了 → `update` 直接 hide，状态卡从未真正显示；其存在只为「状态提示超时」的破坏性隐藏。
> - 选项：
>   - **A 让状态卡真实渲染**：`should_render()` 加 `|| self.status.is_some()`；需处理与 OCR/AI 预览像素互踩（同不可见状态类）的设计，验收清单的「状态卡超时只在无预览时隐藏」需重写。
>   - **B 移除状态卡机制**：删 `status` 渲染与「状态提示超时」逻辑，超时隐藏改为基于 `has_any_content`；更简单、无互踩，但丢失状态提示。
>   - **C 保持现状 + 标注**：状态卡作为内部隐藏开关存在，文档明确「不渲染，仅用于超时隐藏」。
> - 建议：B（移除未真正呈现的机制，收敛复杂度），或 A 若确实需要状态提示（需设计互踩）。
> 
> **item1/2 死键 ToUnicodeEx 冲刷 & Chromium 类宿主 TRUE→FALSE 透传**（需 Windows 真机矩阵）
> - 均为布局/宿主交互层面的边界，代码已按「不修 + 说明」记录（与 oem_fallback '/' 全布局契约、Electron/Chromium 吞键风险有关）。
> - 建议：纳入 Windows 手动验收矩阵（manual-acceptance-windows.md）逐项真机标注，不阻塞发布。
> 
> **item6 提交粒度惯例**（记录）
> - 已按「行为变更并入审查修复提交并在提交信息显式记录」执行；后续审查批次注意保持。
> 
> > 待决策：item3（A/B/C）、item5（A/B/C）。item1/2 走 Windows 真机；item4 已合并候选（PR #109）。

> **gqf2008** (2026-09-09):
>
> ✅ item3=A（预览 TTL 10s 自动失效）+ item5=B（移除中英切换状态卡）→ PR #110。verba-core 104 单测 + macos 14 单测全绿；Windows 由 frontend-windows CI 兜底。item1/2 仍走 Windows 真机。

---

## #111 [macOS] .pkg 系统级安装包（Installer + postinstall 注册输入源）
- 状态：closed　创建：2026-09-09　标签：—

## 目标
在现有 DMG（用户级一键安装）之外，提供 macOS `.pkg` 系统级 Installer 包。

## 范围
- 新增 `frontends/macos/ime/scripts/package-pkg.sh`：从已组装/已签名的 `dist/Verba.app`
  生成 `dist/Verba-<版本>.pkg`，安装到 `/Library/Input Methods/Verba.app`（需管理员）。
- postinstall：以当前 console 用户身份调用 `verba-register` 注册并启用输入源。
- 可选签名：`INSTALLER_IDENTITY`（Developer ID Installer）时 `productbuild --sign`；正式分发再 notarytool 公证 + staple。
- CI：`frontend-macos-app` job 增加「打包 PKG 并校验」（install-location + postinstall 内嵌）。
- release：macOS job 增加 pkg 构建 + 可选签名/公证 + 上传 `macos-dist`；SHA256SUMS 纳入 `.pkg`。
- 文档：`docs/building.md` 补 pkg 用法与产物表。

## 验收标准
- 本地 `bash scripts/package-pkg.sh` 产出可 `pkgutil --expand-full` 的 flat pkg；
  `PackageInfo` 的 `install-location="/Library/Input Methods"`、`Scripts/postinstall` 存在。
- `frontend-macos-app` CI 绿。
- 有 Installer 证书时 `pkgutil --check-signature` 通过且公证 staple 有效（无证书时产出未签名 pkg，仅 dry-run）。

## 参考
- 现有 `scripts/package.sh`、`scripts/install-dmg.command`、`.github/workflows/release.yml`（DMG 签名/公证流程）。

---

## #113 [Release] v0.2.10 发布准备与 tag 全链路验证
- 状态：closed　创建：2026-09-09　标签：—

目标：发布 v0.2.10，并验证 tag 触发的 GitHub Release 全链路。

## 范围
- 版本号统一到 `0.2.10`：根 workspace + macOS/Windows 前端 Cargo.toml + 对应 Cargo.lock + Info.plist + Inno Setup ISS + docs 示例。
- 更新 `docs/roadmap.md` 发布记录与 M6 状态。
- 通过 PR 合入 main；独立审查 + 本地门禁 + 远程 CI 全绿。
- 合并后打 annotated tag `v0.2.10` 并推送，触发 Build & Release。
- 验证 tag run 的 macos / windows / release 三个 job 全绿。
- 验证 GitHub Release 资产：DMG、PKG、Windows EXE、SHA256SUMS；下载后复核 macOS PKG 签名、公证、staple。

## 验收标准
- [ ] 仓库内版本号引用一致为 0.2.10。
- [ ] PR 本地门禁通过，远程 CI 全绿，独立审查通过。
- [ ] tag run 三个 job 全绿，Release 创建成功且资产完整。
- [ ] `SHA256SUMS` 与下载产物校验一致。
- [ ] macOS PKG `pkgutil --check-signature` / `stapler validate` / `spctl` 复核通过。

## 关联
- 前置：Build & Release workflow_dispatch 干跑 run 34305219742 已通过。
- 本次发布随版内容：日志脱敏（#59）、OCR 预览状态机/TTL/状态卡（#105 item3/4/5）、macOS PKG 安装包（#111）等。

> **gqf2008** (2026-09-09):
>
> 🔄 [处理中][wt-v0.2.10] 开始发布准备；worktree: `verba-ime-wt-v0.2.10`。 🦀

> **gqf2008** (2026-09-09):
>
> 【完成】v0.2.10 已发布并完成 tag 全链路验证：
> 
> - PR #114 已合并：merge commit `1f563da`；annotated tag `v0.2.10` 解引用后指向同一 commit。
> - Build & Release tag run [34308079932](https://github.com/gqf2008/verba-ime/actions/runs/34308079932) 全绿：`version-guard` / `macos` / `windows` / `release` 四个 job 均 success。
> - GitHub Release 已发布（非 draft / 非 prerelease），资产：`Verba-0.2.10.dmg`、`Verba-0.2.10.pkg`、`verba-ime-setup-0.2.10.exe`、`SHA256SUMS`。
> - `SHA256SUMS` 三个产物哈希与 GitHub Release API asset digest 逐项一致。
> - macOS PKG 使用 Developer ID Installer 签名，日志显示 `Notarization: trusted by the Apple notary service`，staple 成功。
> - 发布后 roadmap 已通过 PR #115 更新为「v0.2.10 已发布」（main `014d19b`）。
> 
> 验收项全部满足，关闭本 issue。 🦀

---

## #116 [macOS] 输入法菜单栏图标重设计（品牌言字模板图标）
- 状态：closed　创建：2026-09-09　标签：—

目标：重设计 macOS 输入法菜单栏图标，替换泛用气泡为品牌「言」字实心模板图标，避免菜单撑高并提升辨识度。

## 背景
- v0.2.10 已把 `Icon.pdf` 从 144×144 修正为 18×18，但旧 `verba-mac` 进程未重启时，系统菜单仍会显示旧进程缓存的巨大蓝块。
- 现有 18×18 气泡图标能避免撑高，但品牌辨识度弱，用户反馈「看不到显眼的输入法图表」。

## 范围
- `scripts/gen-macos-icon.py`：生成新图标——实心圆角气泡 + 负空间品牌「言」字，18pt 模板画布。
- 更新 `frontends/macos/ime/app/Resources/Icon.pdf`。
- `ci.yml` `frontend-macos-app`：增加生成器与产物一致性校验 + `MediaBox 18x18` 守卫。
- 保持 `TISIconIsTemplate=true`（macOS 菜单栏单色模板规范）。

## 验收标准
- [ ] `python3 scripts/gen-macos-icon.py` 可复现生成提交的 `Icon.pdf`。
- [ ] `Icon.pdf` MediaBox 为 18×18pt，菜单行高正常。
- [ ] 18pt 渲染下「言」字与气泡尾巴清晰可辨。
- [ ] CI `frontend-macos-app` 通过，含正/反向守卫。
- [ ] 记录旧进程残留处置：安装新版本后必须重启 `verba-mac` 或注销/重启。

> **gqf2008** (2026-09-09):
>
> 🔄 [处理中][wt-macos-icon] 开始设计；worktree: `verba-ime-wt-macos-icon`。 🦀

> **gqf2008** (2026-09-09):
>
> 【完成】新图标已设计并发布：
> 
> - `scripts/gen-macos-icon.py` 改为实心圆角气泡 + even-odd 负空间品牌「言」字，18pt 模板画布。
> - `Icon.pdf` 重新生成；CI 增加生成器/产物/bundle 一致性 + `MediaBox 18x18` + `TISIconIsTemplate=true` 守卫。
> - PR #117 已合并；v0.2.11 Release 已包含新图标（run 34316844410 全绿）。
> - 本地已用新图标替换安装态 `Verba.app` 并重启 `verba-mac` 预览；旧 v0.2.10 安装态保留在 `~/Library/Input Methods/Verba.app.v0.2.10-backup-*` 作为备份。
> 
> 关闭本 issue。 🦀

---

## #118 [Release] v0.2.11 发布准备与菜单图标发布
- 状态：closed　创建：2026-09-09　标签：—

目标：发布 v0.2.11（macOS 输入法菜单栏品牌「言」字图标重设计），并验证 tag Release 全链路。

## 范围
- 版本号统一到 `0.2.11`：根 workspace + macOS/Windows 前端 Cargo.toml + 3 个 Cargo.lock + Info.plist + Inno Setup ISS + docs 示例。
- `docs/roadmap.md` 更新 v0.2.11 发布准备/已发布记录。
- 合并后打 annotated tag `v0.2.11`，验证 Build & Release 四 job 与 Release 资产。
- 安装/更新后重启 `verba-mac`，确认菜单栏显示新版实心「言」字图标（#116）。

## 验收标准
- [ ] 版本号引用一致为 0.2.11。
- [ ] PR 本地门禁 + 独立审查 + 远程 CI 全绿。
- [ ] tag run `version-guard` / `macos` / `windows` / `release` 全绿。
- [ ] Release 资产 DMG/PKG/EXE/SHA256SUMS 完整，SHA 与 API digest 一致。
- [ ] 安装后重启输入法进程，菜单栏图标为 #116 新图标且行高正常。

## 关联
- #116 图标重设计已通过 PR #117 合并（b402805）。

> **gqf2008** (2026-09-09):
>
> 🔄 [处理中][wt-v0.2.11] 开始发布准备；worktree: `verba-ime-wt-v0.2.11`。 🦀

> **gqf2008** (2026-09-09):
>
> 【完成】v0.2.11 已发布并验证：
> 
> - PR #119 已合并：merge commit `24841cc`；tag `v0.2.11` 指向同一 commit。
> - Build & Release run [34316844410](https://github.com/gqf2008/verba-ime/actions/runs/34316844410) 全绿：version-guard / macOS / Windows / release。
> - Release 资产：`Verba-0.2.11.dmg`、`Verba-0.2.11.pkg`、`verba-ime-setup-0.2.11.exe`、`SHA256SUMS`；SHA256 与 API digest 一致。
> - 新菜单栏「言」字图标随包发布；本地安装态已替换为新图标并重启 `verba-mac`。
> - roadmap 已通过 PR #120 更新为「v0.2.11 已发布」（main `a71aab5`）。
> 
> 关闭本 issue。 🦀

---

## #121 [macOS] PKG 安装被 Installer 重定位到 dist/Verba.app
- 状态：closed　创建：2026-09-09　标签：—

目标：修复 macOS PKG 安装被 Installer 重定位到已有 `dist/Verba.app`，导致 `/Library/Input Methods/Verba.app` 为空、菜单栏不出现输入法的问题。

## 根因
- `pkgbuild --root` 默认 `BundleIsRelocatable=true`，生成 `<relocate><bundle .../></relocate>`。
- 安装 v0.2.11 PKG 时，PackageKit 在用户仓库发现同 bundle id 的 `frontends/macos/ime/dist/Verba.app`，把 payload 重定位到该路径，而不是 `/Library/Input Methods`。
- `/var/log/install.log` 明确记录：`Library/Input Methods/Verba.app relocated to Users/sqb/Github/verba-ime/frontends/macos/ime/dist/Verba.app`。
- 因此 PKG receipt 存在、app 也是正确 v0.2.11，但系统级路径没有 app，TIS 也不会注册。

## 范围
- `frontends/macos/ime/scripts/package-pkg.sh`：生成 component plist，设置 `BundleIsRelocatable=false`（并保留严格 identifier/version 检查）。
- `ci.yml` `frontend-macos-app`：展开 PKG 后断言 `<relocate>` 无 bundle 子元素（空 `<relocate/>`），防回归。
- 本地做正/反向对照：无 component plist 时 `<relocate>` 含 bundle 子元素，有 component plist 时为空。
- 发布修复版本（v0.2.12）并验证系统级安装路径。

## 验收标准
- [ ] PKG 内 `Verba-component.pkg/PackageInfo` 的 `<relocate>` 为空（即 `<relocate/>`，无 bundle 子元素）。
- [ ] CI 展开 PKG 后断言通过；故意移除 component plist 时断言会红。
- [ ] 新 PKG 安装到 `/Library/Input Methods/Verba.app`，不再重定位。
- [ ] 安装后输入法菜单出现，图标为 #116 新版「言」字。

> **gqf2008** (2026-09-09):
>
> 🔄 [处理中][wt-pkg-relocatable] 开始修复；worktree: `verba-ime-wt-pkg-relocatable`。 🦀

> **gqf2008** (2026-09-09):
>
> 【干净对照结论 2026-09-09】系统级安装本身没问题，失败点是 PKG `postinstall` 缺少“先启动 app”步骤。
> 
> 对照结果：
> - 干净状态（清 TIS 源 + 清 HIToolbox Verba 条目 + 移除两份 app）下，系统级 `/Library/Input Methods/Verba.app` 仅跑 CLI `verba-register`：父源 `enabled=0`，`TISSelectInputSource=-50`。
> - 同一个系统级 app 先经当前用户会话 `open` 启动，再跑 GUI/TIS 注册启用：父源 `enabled=1`、Pinyin 模式 `enabled=1`、`TISSelectInputSource=0`，当前已实际选中系统级 Verba。
> - 用户级此前看起来“无需授权”，是因为 TIS 启用状态早已持久化，并非用户级方案天然更好。
> 
> 根因：TIS 启用需要当前用户会话中的 IMKServer 已启动；PKG postinstall 以 root/package script 运行，只复制并调用 register，未先 `open` app。
> 
> 下一步：修 `package-pkg.sh` postinstall，先以 console user 经 LaunchServices 启动 `/Library/Input Methods/Verba.app`，等待 IMKServer 就绪，再执行 verba-register；随后构建测试 PKG 做干净安装验收。

> **gqf2008** (2026-09-09):
>
> v0.2.13 已发布并收口用户级自动启用；PKG relocation 本身已在 v0.2.12 修复/验证。系统级安装后的“自动启用”拆分到 #132（v0.2.14 GUI helper）跟踪，本 issue 关闭。 🦀

---

## #123 [Release] v0.2.12 发布准备（macOS PKG 重定位修复）
- 状态：closed　创建：2026-09-09　标签：—

目标：发布 v0.2.12，修复 macOS PKG 安装被 Installer 重定位到 dist/Verba.app、导致系统级输入法菜单不出现的问题（#121 / PR #122）。

## 范围
- 版本号统一到 `0.2.12`：根 workspace + macOS/Windows 前端 Cargo.toml + 3 个 Cargo.lock + Info.plist + Inno Setup ISS + docs 示例。
- `docs/roadmap.md` 更新 v0.2.12 发布准备/已发布记录。
- 合并后打 annotated tag `v0.2.12`，验证 Build & Release 四 job 与 Release 资产。
- 安装新 PKG，确认 `/Library/Input Methods/Verba.app` 存在且不再重定位，菜单出现「拾言输入法」。

## 验收标准
- [ ] 版本号引用一致为 0.2.12。
- [ ] PR 本地门禁 + 独立审查 + 远程 CI 全绿。
- [ ] tag run `version-guard` / `macos` / `windows` / `release` 全绿。
- [ ] Release 资产 DMG/PKG/EXE/SHA256SUMS 完整，SHA 与 API digest 一致。
- [ ] 新 PKG 安装到 `/Library/Input Methods/Verba.app`，Install log 不再出现 relocation。
- [ ] 输入法菜单出现「拾言输入法」，图标为 #116 新版「言」字。

## 关联
- #121 PKG relocation 修复，PR #122 已合并（merge commit `0fdfb24`）。

> **gqf2008** (2026-09-09):
>
> 🔄 [处理中][wt-v0.2.12] 开始发布准备；worktree: `verba-ime-wt-v0.2.12`。 🦀

> **gqf2008** (2026-09-09):
>
> 【发布完成，待真机安装验收】
> 
> - PR #124 已合并：merge commit `de67775`；tag `v0.2.12` 指向同一 commit。
> - Build & Release run [34323513139](https://github.com/gqf2008/verba-ime/actions/runs/34323513139) 全绿：version-guard / macOS / Windows / release。
> - Release 资产：`Verba-0.2.12.dmg`、`Verba-0.2.12.pkg`、`verba-ime-setup-0.2.12.exe`、`SHA256SUMS`；SHA256 与 API digest 一致。
> - 已下载并核验 PKG：Developer ID Installer 签名、公证 trusted、staple 成功；组件与 Distribution 的 relocation 子元素数均为 `0`，install-location 为 `/Library/Input Methods`。
> - 剩余：安装新 PKG 后确认 `/Library/Input Methods/Verba.app`、菜单出现「拾言输入法」。
> 
> Release: https://github.com/gqf2008/verba-ime/releases/tag/v0.2.12 🦀

> **gqf2008** (2026-09-09):
>
> v0.2.12 已发布；后续 macOS 菜单/自动启用问题已由 v0.2.13（用户级）与 #132（系统级 PKG，v0.2.14）跟踪。本发布 issue 关闭。 🦀

---

## #129 [Release] v0.2.13 发布准备（macOS 自动启用 + IMK/OCR 修复）
- 状态：open　创建：2026-09-09　标签：—

目标：发布 v0.2.13，收口 macOS 用户级 DMG 自动启用，并带上 IMK 连接名/markedRange 与 OCR 上屏竞态修复。

## 范围

- PR #126：IMK 连接名改为 `<bundle id>_Connection`，覆盖 `selectionRange`，避免 markedRange XPC 崩溃。
- PR #127：OCR 结果与输入回调同 tick 到达时不再被当前按键吞掉；预览期间可打印键先提交再继续。
- PR #128：`verba-register` 写 `com.apple.inputsources` 第三方输入源白名单（父源 + Pinyin mode）并刷新 TextInputMenuAgent，用户级 DMG 安装后无需手动添加。
- 版本号统一 0.2.13：根 workspace + macOS/Windows 前端 Cargo.toml + 3 个 Cargo.lock + Info.plist + ISS + docs 示例。
- 合并后打 tag `v0.2.13`，验证 Build & Release 四 job 与 Release 资产。
- 真机验收：DMG 用户级干净安装后菜单自动出现、可切换、OCR 可上屏。

## 非范围

- 系统级 PKG 自动启用：PKG postinstall 受 package sandbox 限制，v0.2.14 用用户会话 GUI helper 单独收口。

## 验收标准

- [ ] 版本号引用一致为 0.2.13。
- [ ] PR 本地门禁 + 独立审查 + 远程 CI 全绿。
- [ ] tag run `version-guard` / `macos` / `windows` / `release` 全绿。
- [ ] Release 资产 DMG/PKG/EXE/SHA256SUMS 完整，SHA 与 API digest 一致。
- [ ] DMG 用户级干净安装：无需手动添加，菜单出现「拾言输入法」，可切换/输入。
- [ ] OCR `///` 识别结果可上屏，预览期间继续打字不丢结果。

## 关联

- Refs #60 #121 #123 #126 #127 #128

> **gqf2008** (2026-09-09):
>
> 【发布准备进行中】
> 
> - 版本号已统一到 `0.2.13`（根 workspace + macOS/Windows 前端 Cargo.toml + 3 个 Cargo.lock + Info.plist + ISS + docs 示例）。
> - PR #130 已创建，等待独立审查 + 远程 CI。
> - 本地 root fmt/test(skip keyring)/clippy、macOS frontend fmt/test、Windows fmt 已通过；macOS frontend clippy 因本机构建缓存占满磁盘未复跑（PR #128 已全绿），tag 前以远程 CI 为最终门禁。

> **gqf2008** (2026-09-09):
>
> v0.2.13 已发布并完成发布后校验：
> 
> - tag `v0.2.13` → merge commit `4a211e6`
> - Release run `34356976512` 四 job 全绿
> - DMG/PKG/EXE/SHA256SUMS 资产完整，SHA256 与 API digest 一致
> - .app/PKG 公证 Accepted + staple 成功
> - roadmap 已由 PR #131 标记 v0.2.13 已发布
> 
> 系统级 PKG 自动启用留待 v0.2.14 GUI helper。 🦀

> **gqf2008** (2026-09-09):
>
> 发布本身已完成，但验收标准中的“干净环境 DMG 安装后菜单自动出现/OCR 上屏”尚未用 Release 资产在干净用户态完成，重新打开跟踪发布后验收。当前已确认：tag/run/assets/SHA/公证 staple 均通过。

---

## #132 [macOS] 系统级 PKG 安装后自动启用输入源（v0.2.14 GUI helper）
- 状态：open　创建：2026-09-09　标签：—

目标：让系统级 PKG 安装到 `/Library/Input Methods/Verba.app` 后，也在当前用户会话内自动完成 TIS 注册/启用，无需用户手动添加。

## 背景

- v0.2.13 已解决用户级 DMG 自动启用：`verba-register` 写 `com.apple.inputsources` 白名单并刷新 TextInputMenuAgent（#128）。
- 系统级 PKG 的 `postinstall` 运行在 `package_script_service` 沙盒内，以用户身份写 `~/Library/Preferences/com.apple.inputsources.plist` 会 `PermissionDenied`；以 root 写同样被沙盒拒绝。
- 需要像手心输入法一样，在用户可见/用户会话内启动一个 GUI helper，由 helper 写白名单并刷新 agents；PKG 只负责复制 app。

## 范围

- 新增/复用 GUI helper（可复用 `verba-register` 的白名单写入逻辑），在 console user 会话内执行。
- PKG `postinstall` 改为经 LaunchServices/用户会话启动 helper，而不是直接调用 CLI。
- 保持 v0.2.13 的 fail-closed/原子写/enabled 状态校验。

## 验收

- [ ] 干净系统级 PKG 安装后，不打开键盘设置、不手动添加，菜单出现「拾言输入法」。
- [ ] 父源与 Pinyin mode 均 `enabled=1`，`TISSelectInputSource` 可切换。
- [ ] helper 失败时安装/提示可见，不假成功。
- [ ] 无用户手动操作、无需重启。

## 关联

- #121（PKG relocation，已修）
- #128（用户级白名单自动启用）
- #60（macOS IMK 验收收尾）

> **gqf2008** (2026-09-09):
>
> 🔄 处理中：[wt-macos-pkg-helper]（worktree `/Users/sqb/Github/verba-ime-wt-macos-pkg-helper`，分支 `fix/macos-pkg-helper`）

> **gqf2008** (2026-09-09):
>
> v0.2.14 已发布并包含本 issue：
> 
> - PR #136 合并 commit `2e95664`，tag `v0.2.14`。
> - Release run `34369830371` 四 job 全绿。
> - PKG 签名/公证 trusted、staple 成功；最终 PKG postinstall 经用户会话 GUI helper 注册/启用，且无 `open -W`。
> - 剩余：干净 macOS 真机安装 PKG 后“菜单自动出现、无需重启”的最终验收（#135）。

---

## #133 [macOS] DMG 安装器布局错误：指向 /Applications，应改为安装/卸载文件
- 状态：open　创建：2026-09-09　标签：—

现象：v0.2.13 DMG 内含 `Applications` 快捷方式，但 Verba 是输入法，应安装到 `~/Library/Input Methods`，不是拖到 `/Applications`。用户看到 Applications 链接会误以为需要拖入应用目录，实际安装路径完全不同。

## 目标

改成手心输入法同构的 DMG 布局：去掉 `Applications` 链接，只保留 `安装.command` + `卸载.command`（可附使用说明）。

## 范围

- `release.yml` DMG staging 移除 `/Applications` symlink。
- `安装.command`：拷贝到 `~/Library/Input Methods`，调用 `verba-register` 写白名单/启用，启动 app。
- 新增 `卸载.command`：停进程、注销/移除 Verba 输入源条目、删除 `~/Library/Input Methods/Verba.app`。
- `verba-register` 增加 `--uninstall`（移除 com.apple.inputsources / HIToolbox Verba 条目并刷新 agents），供卸载脚本复用。

## 验收

- [ ] DMG 内不再出现 Applications 快捷方式。
- [ ] 双击安装后自动装到 `~/Library/Input Methods`，无需手动添加输入法。
- [ ] 双击卸载后 app/输入源条目清理干净。
- [ ] 干净环境真机验收。

Refs #129 #132

> **gqf2008** (2026-09-09):
>
> 🔄 处理中：[wt-macos-dmg-ux]（worktree `/Users/sqb/Github/verba-ime-wt-macos-dmg-ux`，分支 `fix/macos-dmg-ux`）

> **gqf2008** (2026-09-09):
>
> 代码修复已通过独立审查并合并：PR #134（merge commit `af66348`）。
> 
> 已完成：DMG 最终产物不再含 Applications，安装/卸载脚本布局齐全；卸载清理失败会保留 app；本地真实 DMG 挂载验收与负向守卫均通过；远程 CI 8/8 全绿。
> 
> 剩余：等待 v0.2.14 Release 资产在干净 macOS 环境完成安装→菜单出现→卸载清理验收后关闭（与 #129 联动）。

> **gqf2008** (2026-09-09):
>
> v0.2.14 已发布并包含本 issue：
> 
> - Release 资产 `Verba-0.2.14.dmg` 已下载验收：根目录仅 `Verba.app / 安装.command / 卸载.command / 使用说明.txt`，无 `Applications`。
> - SHA256 与 GitHub API digest 一致；DMG 公证 staple 成功、Gatekeeper accepted。
> - 剩余：干净 macOS 真机安装→菜单出现→卸载清理验收。

---

## #135 [Release] v0.2.14 发布准备（DMG 布局 + PKG 自动启用）
- 状态：open　创建：2026-09-09　标签：—

目标：发布 v0.2.14，收口 macOS DMG 安装布局与系统级 PKG 用户会话自动启用。

## 范围

- #133：DMG 移除 `/Applications`，改为安装/卸载脚本布局（已合并 PR #134）。
- #132：PKG postinstall 经 LaunchServices 在 console 用户会话启动 helper，复用 verba-register 白名单/启用逻辑；失败可见，不假成功。
- 版本号统一 0.2.14：根 workspace + macOS/Windows 前端 Cargo.toml + 3 个 Cargo.lock + Info.plist + ISS + docs 示例。
- 合并后打 tag `v0.2.14`，验证 version-guard / macOS / Windows / release 四个 job 与 Release 资产。
- 真机验收：DMG 布局正确；PKG 系统级安装后无需打开键盘设置、无需重启，菜单出现「拾言输入法」并可切换。

## 验收标准

- [ ] #132 实现经独立审查，helper 失败路径可观测。
- [ ] 版本号引用一致为 0.2.14。
- [ ] PR 本地门禁 + 独立审查 + 远程 CI 全绿。
- [ ] tag run 四 job 全绿。
- [ ] Release 资产 DMG/PKG/EXE/SHA256SUMS 完整，SHA 与 API digest 一致。
- [ ] DMG 最终产物无 Applications，安装/卸载脚本齐全。
- [ ] PKG 系统级干净安装后自动启用，无需手动添加、无需重启。

## 关联

- Refs #60 #129 #132 #133

> **gqf2008** (2026-09-09):
>
> 🔄 处理中：[wt-macos-pkg-helper]（worktree `/Users/sqb/Github/verba-ime-wt-macos-pkg-helper`，分支 `fix/macos-pkg-helper`）

> **gqf2008** (2026-09-09):
>
> v0.2.14 发布已完成：
> 
> - PR #136 + docs PR #137 已合并，tag `v0.2.14` 指向 `2e95664`。
> - Release run `34369830371`：version-guard / macOS / Windows / release 四 job 全绿。
> - 资产完整：DMG/PKG/EXE/SHA256SUMS；SHA256 与 API digest 一致。
> - DMG 无 Applications；.app/DMG/PKG staple 成功，.app 公证 Accepted、PKG 签名与公证 trusted。
> - PKG postinstall 含用户会话 GUI helper、无 `open -W`。
> 
> 剩余验收：干净 macOS 真机执行 PKG 安装，确认无需打开键盘设置、无需重启即出现「拾言输入法」并可切换。
