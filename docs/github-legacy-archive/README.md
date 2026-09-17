# GitHub 历史归档（issue / discussion）

仓库主仓已迁 walgit（`origin` = walgit，GitHub 仅只读镜像），GitHub 的
**Issues / Wiki / Projects / Discussions 已于 2026-09-17 全部关闭**。
本目录是关闭前导出的一次性归档，用于让团队在 walgit 内仍能查阅历史讨论与需求。

| 文件 | 内容 | 规模 |
|---|---|---|
| `issues.md` | 全部 issue（含已关闭）+ 评论 | 55 个 issue / 134 条评论 |
| `discussions.md` | 全部 discussion + 评论 | 5 个 discussion |

## 为什么留档

关闭的是 GitHub 的**入口**，数据本身未删（重新打开即可恢复）。但以下几点只有归档里才方便查：

- **Discussions #77《三端交互验收方案征求意见》**：前端 16 项待修 + QA 按 P0/P1/P2 定的
  门禁口径（P0=TSF/IMK 注册激活、直输、`//` AI 流式、候选窗、OCR、性能预算、日志脱敏、CI 全绿）。
  这是 macOS/Windows 交互验收的**标准来源**。
- Discussions #70《Beta 用户反馈收集》（模板帖，0 回复）、#71 里的 Owner 范围决策
  （OCR 保留为正式能力、ASR/TTS 冻结、性能预算缩减为 LLM+OCR）。
- 已关闭的体验批次：#42 / #87 / #88 / #89 / #90 / #101 —— 这些**修过但未在真机验收**，
  2026-09-17 真机试用暴露的崩溃与安装启用问题即落在这批里。

## 后续

新需求、验收记录、协作留痕一律走 **walgit collab 线程**（`walgit collab board|thread`），
不再往 GitHub 写。本归档只读，不再更新。

> 原始 JSON（含 API 全字段）未入库，保存在导出机 `/Volumes/DataExt/tmp/verba-ime-test/`。
