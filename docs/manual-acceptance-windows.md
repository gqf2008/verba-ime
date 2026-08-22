# Windows 手动验收清单（M1）

> 目标：验证「安装输入法 → 打字上屏 → `//` 唤起 AI → 流式 preedit → Enter 上屏」。
> 状态：清单已建立，随真机联调逐项打勾。

## 前置
- [ ] 已构建：`cargo build -p verba-daemon`（根 workspace）与 `cargo build`（frontends/windows/ime，release）
- [ ] 产物齐备：`verba_ime_windows.dll`、`verba-reg.exe`、`verba-daemon.exe`
- [ ] 以**管理员**运行 `verba-reg register <dll路径>`（或运行安装程序）；HKCU CLSID 与 TSF 档案注册成功
- [ ] 系统设置 → 时间和语言 → 语言 → 键盘 → **添加键盘 → Verba 拾言输入法**

## 直输
- [ ] 记事本/任意输入框切到 Verba，输入英文/数字/标点 → 直接上屏（无残留 preedit）
- [ ] 按方向键/快捷键 → 不被吞键

## AI 链路（// 触发）
- [ ] 输入 `//` → preedit 显示 `//`，进入 AI 提示词模式
- [ ] 继续输入提示词（如 `翻译：hello world`）→ preedit 实时更新
- [ ] 按 Enter → 提示词消失，daemon 连接后 LLM 流式结果在 preedit 中滚动出现
- [ ] 流式完成后按 Enter → 最终结果上屏
- [ ] 流式中按 Esc → 取消，无残留

## daemon 与配置
- [ ] daemon 未运行时首次触发 AI 会自动拉起（DLL 同目录 `verba-daemon.exe`）
- [ ] `verba-cli ping` 连通
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
