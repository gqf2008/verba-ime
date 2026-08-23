# 中文引擎选型与集成评估（M5）

> 更新：2026-08-22 · 状态：决策建议（待实现验证）
> 背景：`verba-pinyin` 自研引擎已落地（拼音 + 模糊音 + 简拼 + 整句 DP + 提示词中文）。
> 本文件评估是否/如何引入 librime 等成熟引擎，以及候选融合路线。

## 1. 目标与约束

- Verba 定位：**开源 + 三平台（Windows/macOS/Linux）+ AI/LLM 融合**。
- 中文引擎是"地基"，差异化在 **LLM/AI 候选融合**，不宜过度投入。
- 需要：拼音整句质量、五笔/注音等可选方案、模糊音自定义、低延迟、跨平台一致、打包可控。

## 2. 引擎对比

| 引擎 | 许可 | 语言 | 整句/语言模型 | 跨平台 | 集成成本 | 备注 |
|---|---|---|---|---|---|---|
| **自研 verba-pinyin**（现状） | MIT | Rust | 频率 + 词/字 DP（弱） | 天然 | 零 | 已可用；缺 n-gram、方案扩展 |
| **librime (Rime)** | BSD-3 | C++ | 默认一般，octagram/predict 插件补 n-gram | 三端成熟（Weasel/Squirrel/fcitx5-rime） | 中高（FFI + 数据打包） | 生态最强：拼音/五笔/注音/仓颉 + 上千社区方案 |
| **libime（fcitx5 中文引擎）** | LGPL-2.1 | C++ | **自带 n-gram，拼音整句更好** | Linux 为主，有移植 | 中高 | 绑定 fcitx5 体系，可定制性弱于 Rime |
| **inputx-pinyin** | MIT/Apache-2.0 | Rust | 带 bigram | Rust 天然 | 低（同语言） | 纯 Rust，macOS/iOS 商用，较新、生态小 |
| **ibus-libpinyin** | GPL | C | bigram | Linux | 中 | 维护一般，ABI 不稳 |
| 搜狗/百度/微软拼音 | 闭源 | — | 云端模型，体验最好 | — | — | 不符合开源定位 |

## 3. librime 的 Rust 集成路径（FFI，不重写）

librime 的护城河是 **schema 生态 + 十年跨平台打磨**，因此**用 FFI 站在护城河上，而非重写**：

- **librime-sys**（[lotem/librime-sys](https://github.com/lotem/librime-sys)，官方维护，2025-02 仍在更新）：原始 FFI 绑定，`build.rs` 定位 librime 库目录；Windows 需先构建 librime（CMake/MSVC 或 vcpkg）。
- **librime-rs**（bczhc）：`rime_api.h` 的安全 Rust 包装。
- **rime-ls**（wlh320）：基于 librime-sys 的 LSP 服务，是"Rust 集成 librime"的现成参考。
- 自带插件：octagram（n-gram）、librime-predict（预测）、lua（脚本扩展）。

### 集成架构两种方案

| 方案 | 引擎位置 | 延迟 | 体积 | 跨平台一致性 | 复杂度 |
|---|---|---|---|---|---|
| **A. 前端进程内**（如 Weasel：librime 进 TSF DLL） | 各平台前端 | 最低 | DLL/插件变大（含词库） | 各端各自打包 | 中（三端重复接入） |
| **B. daemon 进程内** | verba-daemon | 有 IPC 往返（命名管道 <1ms） | DLL 保持薄 | 引擎一份、三端共用 | 中（IPC 会话协议） |

**建议：M5 采用 B（daemon 内）做验证**——不增加前端 DLL 体积、引擎崩溃可隔离、三端一致；若延迟不可接受再回退 A（参考 Weasel 模式）。

## 4. 决策建议（阶段化）

1. **M5-默认**：保持 `verba-pinyin` 自研引擎为默认（已满足 M5 拼音需求）。
2. **M5-评估**：用 librime-sys 在 daemon 内做一个**可选引擎 spike**，验证：整句质量（octagram）、五笔/注音方案加载、Rime 词库生态。能跑通且质量明显更好 → 提供 `config 引擎=rime|builtin` 开关；否则维持自研。
3. **M5-融合**：候选融合 = **词库候选（自研/librime）+ LLM 候选**（经 daemon 的 verba-ai）。LLM 候选按需触发（如候选页 2 提供 AI 联想），控制延迟与成本。
4. **候选窗口**：从内联候选升级为独立候选窗（分页/主题），M4/M5 共用。

## 5. 风险与开放问题

- librime Windows 构建链（CMake/MSVC）与当前 mingw 工具链的差异；打包体积（librime + 词库 ~10-50MB）。
- LGPL 的 libime 若采用，需注意动态链接/分发合规（librime 的 BSD 无此限制，故优先 librime）。
- IPC 每键往返延迟是否可接受（需实测 >10 键/秒 手感）。
- octagram/大数据模型体积（10MB+）与首启加载策略。

## 6. 下一步任务（M5）

- [ ] 构建 librime-sys spike（Windows，daemon 内），验证拼音整句 + 五笔方案（延后：需预编译
      rime.dll 或全量编译 librime，环境风险高；待候选融合实机验收后单独做）
- [ ] 对比自研 vs librime：整句准确率采样（50 句日常对话）
- [x] 候选窗（独立窗口、分页、主题）（2026-08-23 实机验收通过 + 代码完成）
- [x] 候选融合（词库 + LLM 候选，IPC 协议扩展 `LlmCandidates`/`Candidates`，mock 端到端冒烟通过，
      待实机验收）（2026-08-23）
