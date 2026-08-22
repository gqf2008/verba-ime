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