# M6 测试计划（质量测试 · 许衡）

> 更新：2026-08-29 ｜ 关联：#60 / #61 / #76
> 口径：OCR=正式能力；ASR/TTS=实验性冻结（代码保留、默认关闭、入口隐藏，M6 不承诺）；
> 性能预算=LLM 核心输入链路 + OCR（#58）。

## 1. 目标
- 验证 M6「正式能力」端到端可用；三端手动+自动验收清单全绿
- 发布门禁：P0/P1 清零 + CI 全绿

## 2. 范围
| 能力 | 范围 | 验收 |
|---|---|---|
| 基础输入（直输/组合/上屏） | in | 正式验收 |
| // AI 流式（LLM 核心链路） | in | 正式验收 |
| 候选窗 / Rime 单引擎 | in | 正式验收 |
| OCR（截图/看图） | in | 正式能力，正式验收 |
| ASR（听写） | out | 冻结：仅验证默认关/入口隐藏，不验效果 |
| TTS（朗读） | out | 冻结：仅验证默认关/入口隐藏，不验效果 |
| 性能预算 | in（缩减） | LLM 核心链路 + OCR（#58） |
| Linux Fcitx5/IBus/Wayland | out | v1.1 排期 |

## 3. 环境矩阵
| 环境 | 覆盖 | 方式 |
|---|---|---|
| Windows 11 + TSF | 全量交互 | 真机 + CI（windows-latest） |
| macOS + IMK | 全量交互 | 真机 + CI（macos-latest） |
| 共享 Rust 核心 | 单元/集成 | CI 三平台矩阵 |

## 4. 用例分级
- P0（阻塞发布）：TSF/IMK 注册激活、直输、// AI 流式、候选窗跟随/分页/主题、OCR 正式链路、性能预算（LLM+OCR）、日志脱敏、CI 全绿
- P1（可短窗口修复）：多客户端会话语义、热键链路、视觉细节、macOS 权限弹窗
- P2（打磨）：卸载、隐私提示、慢网络取消

## 5. 回归与门禁
- 回归：M1 直输/AI 链路、M5 候选窗/Rime、M6 验收项
- 门禁：P0/P1 清零 + CI（fmt/clippy/test）全绿才放行
- 当前阻塞：main CI Format 失败 5 job（run 33234518956，前端直接推送批次引入）→ 验收前须先修复

## 6. 交付物
- docs/manual-acceptance-windows.md（更新：OCR 正式项、ASR/TTS 冻结标注）
- docs/manual-acceptance-macos.md（新增）
- docs/three-platform-acceptance-matrix.md（新增）