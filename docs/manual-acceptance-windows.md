# Windows 手动验收清单（M1）

> 目标：验证「安装输入法 → 打字上屏 → `//` 唤起 AI → 流式 preedit → Enter 上屏」。
> 状态：**M1 已于 2026-08-22 实机验收通过**（Notepad--，`//translate hello world` → 流式中文回复 → 上屏；日志 `CommitResult` 确认）。
> 关键修复：`3bfd2a6` OnTestKeyDown 认领、`b295dba` 组合引用放回、`9dd853c` StartLlm 不清空 preedit。

## 前置
- [x] 已构建：`cargo build -p verba-daemon`（根 workspace）与 `cargo build`（frontends/windows/ime，release）
- [x] 产物齐备：`verba_ime_windows.dll`、`verba-reg.exe`、`verba-daemon.exe`
- [x] 以**管理员**运行 `verba-reg register <dll路径>`（或运行安装程序）；HKCU CLSID 与 TSF 档案注册成功
- [x] 系统设置 → 时间和语言 → 语言 → 键盘 → **添加键盘 → Verba 拾言输入法**

## 直输
- [x] 记事本/任意输入框切到 Verba，输入英文/数字/标点 → 直接上屏（无残留 preedit）
- [x] 按方向键/快捷键 → 不被吞键

## AI 链路（// 触发）
- [x] 输入 `//` → preedit 显示 `//`，进入 AI 提示词模式
- [x] 继续输入提示词（如 `translate hello world`）→ preedit 实时更新
- [x] 按 Enter → daemon 连接后 LLM 流式结果在 preedit 中滚动出现
- [x] 流式完成后按 Enter → 最终结果上屏
- [ ] 流式中按 Esc → 取消，无残留

## daemon 与配置
- [x] daemon 未运行时首次触发 AI 会自动拉起（DLL 同目录 `verba-daemon.exe`）
- [x] `verba-cli ping` 连通
- [ ] `verba-cli config set llm_base_url=... llm_model=...` 热更新生效
- [ ] API Key 走系统凭据库（或 `VERBA_API_KEY` 环境变量开发兜底）

## 隐私提示
- [ ] 首次配置远程 LLM 时已阅读「数据将发送至服务商」说明（见 docs/privacy.md）

## 卸载
- [ ] 管理员运行 `verba-reg unregister`（或卸载程序），输入法从列表消失

## 疑难排查：`//` 无反应（下划线不出现）

### 症状
在记事本/任意输入框切换到 Verba 后输入 `//`，没有下划线、没有任何效果。

### 根因（2026-08-22 真机联调确认，晚间修复）
1. **`OnTestKeyDown` 恒返回 FALSE 导致 `OnKeyDown` 从不被调用**（主因，已修复 `3bfd2a6`）：
   TSF 仅对"认领"（OnTestKeyDown 返回 TRUE）的按键回调 OnKeyDown；一直返回 FALSE 时，
   激活与键盘 sink 都正常，但任何按键都进不了状态机 → `//` 与直输完全失效。
   修复：按状态机认领——Idle 只认领 `/`，组合/提示词/流式/结果态认领全部可打印字符
   与控制键（Enter/Backspace/Esc）。
2. **旧 DLL 残留**：`C:\Program Files\Verba\verba_ime_windows.dll` 为早前构建
   （无键盘 sink 重试修复、无落盘日志）。已加载过旧 DLL 的进程会一直持有旧 DLL，
   需关闭重开（或注销/重启）才能加载新版本。
3. **指示器不一定可见**：Windows 11 任务栏输入法指示器可能被折叠/隐藏，看不到指示器
   不代表 Verba 未激活——日志 `Verba TSF 激活` 即证明已激活；以输入 `//` 是否出现
   下划线为准。
4. 诊断日志：`%LOCALAPPDATA%\Verba\verba-ime.log`（新 DLL 才有）。

### 规范测试步骤（务必按顺序）
1. **关闭所有记事本窗口**（以及之前测试过的应用）。
2. 打开新的记事本。
3. 点击记事本窗口使其获得焦点，按 **Win+Space** 循环切换输入法，
   **确认任务栏输入法指示器变为 "Verba · 拾言输入法"**（而不是英文/US/微软拼音）。
4. 输入 `//` → 期望出现带下划线的 preedit `//`。
5. 继续输入提示词（如 `翻译：hello world`）→ Enter → LLM 流式结果 → Enter 上屏。
6. 若仍无反应：把 `%LOCALAPPDATA%\Verba\verba-ime.log` 最后 40 行发给开发者，
   重点看是否有：
   - `Verba IME DLL 加载 ... exe=...notepad...`（新 DLL 是否进了记事本进程）
   - `Verba TSF 激活` / `键盘 sink 已挂载`（激活与按键链路是否就绪）
   - `OnKeyDown vk=0xBF`（`/` 键是否到达 IME）

### 判定
- 有 `OnKeyDown vk=0xBF` 且随后有 `action=EnterPrompt` / `action=UpdatePrompt` → 输入链路通，
  剩下的是上屏显示问题。
- 完全没有 `OnKeyDown vk=0xBF` → 按键没到 IME，属"Verba 未真正激活"或旧 DLL 残留，
  按上述步骤重试（必要时注销/重启系统清空旧 DLL）。

---

# Windows 手动验收清单（M5 候选窗）

> 先跑 `pwsh scripts/acceptance.ps1` 自动核对部署态并完成 CLI 级检查（Rime 拼音 / 五笔），
> 剩下交互项按下方清单在真实输入框手工验证。

> 目标：验证「拼音候选窗：跟随光标 + 避让 + 分页 + 主题/皮肤 + Rime 单引擎候选」。
> 状态：**M5 已收口（2026-08-23 实机 OK）**。候选窗核心（跟随光标/智能避让/上屏/Esc 取消）与 Rime 集成实机确认；分页、主题热重载均实现并通过 CLI+单测验证（实机视觉项后补可勾）。
> 前置：已加载新 DLL（HKCU CLSID 指向的当前部署目录，`scripts/acceptance.ps1` 自动检测；
> 每次更新 DLL 后需关闭重开测试应用），mock LLM 运行中
> （`python scripts/mock_openai.py 8765`），配置 `%APPDATA%\verba\Verba\config\config.toml` 指向
> `llm_base_url = 'http://127.0.0.1:8765/v1'`、`llm_model = 'mock'`（实际配置路径以
> `verba-cli config` 输出为准）。

## 拼音候选窗（基础）
- [x] 输入 `n` → 候选窗出现在光标正下方，9 项/页，底部页码脚「1/3」（实机日志：`候选窗显示 锚点=...` 逐键跟随）
- [ ] 候选窗为「横向候选栏 + 顶部拼音组合头（preedit）」风格，对齐微软拼音/手心输入法；`config set theme.layout=vertical` 可切回竖向列表（2026-08-23 UI 现代化，实机视觉待确认）
- [x] 输入 `nishishui` → 词库候选「你是谁 / 你是说」**即时**出现（实机 OK，2026-08-23）
- [x] 按数字/空格选候选上屏（实机空格 `CommitImmediate`）；`Esc` 取消组合（实机日志 `action=Cancel`）

## 分页
- [x] `=` 或 PageDown 下翻 → 页码脚变「2/3」；`-` 或 PageUp 上翻回「1/3」（单测覆盖翻页/回绕/页码偏移；实机视觉后补）
- [x] 第 2 页按 `1` → 选中第 2 页第 1 项上屏（页码偏移正确，单测覆盖）

## 主题/皮肤
- [x] `config.toml` 加 `[theme] preset = "dark"` 保存 → 候选窗自动变深色（热更新，无需重启；配置热重载实机日志 `候选配置已加载` 确认，视觉后补）
- [x] 可加 `background`/`font_size`/`corner_radius` 等键逐项覆盖（键名见 verba-config ThemeConfig，键已实现并解析验证）
      （配置文件路径用 `verba-cli config` 查看；主题键走 `config set theme.*` 或直接编辑）

## 候选融合（LLM 候选）
- [x] 输入 `nishishui` 后停顿约 0.5s → 候选窗尾部**追加** LLM 候选
      （mock 返回：你是谁呀 / 你是谁啊 / 你就是你 / 谁是你 / 你是谁呢；CLI `candidates nishishui` 已验证含你是谁呀/你是谁啊，实机视觉后补）
- [x] 候选可翻页、按数字选中上屏（单测覆盖跨页选择）
- [x] 连续输入不停顿 → 不发起多余请求（防抖；实机日志 Rime 请求按停顿触发）；提交/取消后候选窗消失且无残留
- [x] 日志 `%LOCALAPPDATA%\Verba\verba-ime.log` 有 `Rime 候选请求: pinyin=...`（实机日志已确认）

## 判定
- 候选窗跟随光标、能翻页、主题热更新、Rime 候选可选 → M5 收口。

---

# Windows 手动验收清单（M5 Rime 单引擎）

> 目标：验证 Rime（daemon 内 librime）单引擎：拼音/五笔候选经 `RimeCandidates` 协议回流候选窗。
> 状态：**已收口（2026-08-23）**——CLI 端到端通过 + 候选窗实机确认（`nishishui` Rime 候选触发；
> 2026-08-24 单引擎化：无内置 verba-pinyin、无 LLM 候选融合、无 `config engine` 开关）。
> 前置：`verba-cli config set rime_schema=luna_pinyin_simp`；
> daemon 同目录 `rime/` 已部署 librime（Windows `rime.dll`）+ data（脚本自动检测）。

## Rime 拼音
- [x] 输入 `nishishui` → 候选窗出现 Rime「你是谁 / 你是说…」（单引擎，无内置即时层）
- [x] 按数字/翻页可选候选上屏（实机空格上屏 + 单测覆盖）
- [x] 整句：输入 `jintianwanshangchishenme` → Rime 首候选「今天晚上吃什么」（CLI 已验证）

## Rime 五笔（wubi86）
- [x] `verba-cli config set rime_schema=wubi86` 后，输入五笔码 `wqvb` → 候选「你好 / 您好」；
      `aaaa` → 「工」（CLI 已验证）
- [x] 切回拼音：`verba-cli config set rime_schema=luna_pinyin_simp`（已恢复）

## 判定
- 候选窗出现 Rime 候选、五笔码可出字 → Rime 单引擎集成收口。

# Windows 手动验收清单（v0.2.2 清扫批次：issue #44 真机两步）

> 背景：PR #43/#45 审查遗留（issue #44）的代码部分已全部落地并 CI 钉住
> （`install_stream_token_never_overwrites_newer_epoch` /
> `collect_steps_settles_inflight_deferred_space` / `rime_fail_event_is_done_empty_candidates`
> 三个测试跑在 windows-latest 真机 runner 上）。以下两项是唯一无法离线验证的
> TSF 路由副作用，在 Windows 真机上按步骤执行后即可收口 issue #44。

## 修饰键守卫（Ctrl/Alt 快捷组合不被吞）
- [ ] 在 VS Code 中按 `Ctrl+.`（快速修复）、`Ctrl+,`（设置）：应用动作必须触发
      （输入法不得认领该键）
- [ ] 在记事本中按住 `Ctrl` 连打字母：无残留组合、无候选窗弹出、无卡死
- [ ] 正常输入（无修饰键）候选/上屏不受影响（回归）

## 两段式派发 / 流 token 代际（快速流切换无旧流混入）
- [ ] 连做 5 轮：输入 `//` 提示词 → 回车开流 → 流未结束即 Esc/切窗取消 → 立即
      再开新流。每轮结束上屏文本应完整属于最新一轮（无旧流 chunk 混入），
      daemon 日志无 panic/死锁（`verba-daemon.log`）
- [ ] 慢网络下（可选）：开流后切到无网环境等 3 秒再取消，输入法无卡死
