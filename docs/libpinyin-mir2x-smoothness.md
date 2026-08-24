# 手感丝滑的输入法是怎么做的 —— 以 mir2x / libpinyin 为参考

> 更新：2026-08-24 · 状态：技术参考（供 M5 打磨 / 体验调优参考，不构成引擎选型决策）
> 来源：用户反馈 `../mir2x` 里的 libpinyin 输入法「用起来挺丝滑」，本文件拆解其实现，并映射到 Verba 现状。
> 关联：[中文引擎选型与集成评估](chinese-engine-evaluation.md)（引擎选型），本文聚焦「集成/交互工程」层面的手感来源。

> **2026-08-24 更新（单引擎化）**：Verba 中文候选已收敛为**单引擎 Rime（librime）**，内置自研
> `verba-pinyin` 已移除，`config engine` 开关也已删除；候选由 daemon 内 Rime 提供（Windows
> `rime.dll` / macOS `librime.dylib`）。正文为当前状态，mir2x/libpinyin 仅作手感参考。


---

## 0. 背景

- `../mir2x` 是开源 C++ 传奇游戏（SDL3 + FLTK），为 **SDL 全屏模式** 实现了内嵌 IME。
- 实现文件：
  - `client/src/ime.cpp` / `ime.hpp` —— 封装 libpinyin 的 IME 核心（`_IME_Instance`，内含工作线程）。
  - `client/src/imeboard.cpp` / `imeboard.hpp` —— IME 面板（候选/交互/渲染）。
  - `ports/libpinyin` —— vcpkg 端口，拉取 `etorth/libpinyin`（**GPL-3.0-or-later**，BerkeleyDB 词库，`model20.text` 数据）。
- 代码里显式注释：`// for better implementation, check https://github.com/libpinyin/ibus-libpinyin.git`。

> 一句话结论：**丝滑 = 异步不阻塞 + 高容错候选 + 分段可确定 + 面板跟手**。Verba 以**单引擎 Rime**（本地、后台线程、启动预热）实现前三者；「候选即时性」议题已关闭，「分段承诺」保留结构、当前为整句提交。

---

## 1. 为什么「不卡」：工作线程 + 轮询（核心）

`_IME_Instance` 持有一个**独立 `std::thread`**：

- UI 线程只做两件事：
  1. 发事件：`feed(ch)` / `backspace()` / `select(i)` / `assign(prefix, input)` / `clear()` —— 都是在 `mtx` 锁下改字段，然后 `cond.notify_one()`。
  2. 轮询结果：`candidates()` / `result()` / `done()` / `empty()` —— 在 `update()` 里锁住读。**没有回调**。
- 工作线程 `cond.wait(lock)` 被唤醒后，把所有 libpinyin 重活都做掉：
  - `pinyin_parse_more_full_pinyins(instance, input)`
  - `pinyin_guess_sentence_with_prefix(instance, prefix)`
  - `pinyin_guess_candidates(...)` / `pinyin_get_n_candidate(...)` / `pinyin_get_candidate(...)`
  - 选中后 `pinyin_choose_candidate(...)`
- 原注释点明了为什么**用轮询不用回调**：
  > *problem of callback is not by IME, it's by SDL. SDL texture creation is not thread-safe, if we add callback like onCandidateListChanged, we can't allocate texture inside.*

**结论**：打字动作与候选计算完全解耦。候选计算跟不上打字时，UI 只是「稍后呈现」，绝不阻塞主循环 / 输入事件分发 —— 这是「丝滑」的第一前提。

---

## 2. 让拼音「更容易中」的 4 个 libpinyin 选项

```cpp
pinyin_set_options(context,
    PINYIN_INCOMPLETE |
    PINYIN_CORRECT_ALL |
    USE_DIVIDED_TABLE |
    USE_RESPLIT_TABLE |
    DYNAMIC_ADJUST );
```

| 选项 | 作用 | 对手感的影响 |
|---|---|---|
| `PINYIN_INCOMPLETE` | 允许**不完整拼音 / 简拼**（如 `nishs`、`nhya`） | 不用把拼音打全也能出候选 |
| `PINYIN_CORRECT_ALL` | **自动纠正拼音拼写错误**（zhong/zong 类） | 轻微手误也命中 |
| `USE_DIVIDED_TABLE` + `USE_RESPLIT_TABLE` | 更好的音节切分 / 重切分 | 连续拼音切分更准 |
| `DYNAMIC_ADJUST` | 按用户使用频率**动态调整候选排序** | 越用越贴近个人用词 |

候选排序使用 `SORT_BY_PHRASE_LENGTH_AND_PINYIN_LENGTH_AND_FREQUENCY`（词组长度 × 拼音长度 × 词频综合排序）。

---

## 3. 面板交互：内联、跟手、可拖

- 单行内联面板，展示：`已承诺句子 + 剩余拼音`（`ime.result()`） + **最多 9 个候选**，编号 `1.`~`9.`。
- 键盘：
  - `← / → / ↑ / ↓ / PgUp / PgDn` —— 候选翻页（一次 9 个）。
  - `数字 1-9` —— 选对应候选。
  - `空格` —— 选当前首候选（`m_ime.select(m_startIndex)`）。
  - `Enter` —— 提交整句（`m_onCommit(m_ime.result())`）。
  - `Backspace` —— **弹栈**：若有已承诺段则回退上一段，否则删拼音字符。
  - `Esc` —— 取消。
- 鼠标：悬停/按下高亮、点击候选选中、**按住面板空白处可拖动**（`moveBy`，限制在 renderer 内）。
- 面板宽度自适应候选（`totalLabelWidth()`），高度随字体与候选行。

---

## 4. 分段承诺（incremental commitment）—— 手感的灵魂

用 `stk` 栈维护 `(sentence, start)`：

```cpp
std::vector<std::pair<std::string, int>> stk; // (sentence, start)
```

- 用户选一个候选 → 作为**已承诺段**压栈，再对「剩余 pinyin」重新猜候选：
  - `pinyin_guess_candidates(instance, stk.empty()? 0 : stk.back().second, ...)`
- `result()` = `stk.back().first + input.substr(stk.back().second)`（已承诺句子 + 未消费拼音）。
- `done()` = 承诺段已覆盖全部拼音 → 自动提交。
- `backspace()` 在 `stk` 非空时**只弹栈**，不删拼音字符 → 可逐步回退你的选择。

**体验**：整句候选不够准时，先确定「你是」，再继续对后面的 `shishui` 选候选，**逐段确定**，不必一次性猜对整句。这是 libpinyin 手感的灵魂，也是很多 IME 里「联想/短句滚动」感的技术来源。

---

## 5. 与 Verba 现状的对比（单引擎 Rime）

> 2026-08-24：Verba 中文候选已收敛为**单引擎 Rime（librime）**；内置 `verba-pinyin`、`config engine`
> 开关、LLM 候选融合均已移除。下表为当前状态。

### 已对齐（Verba 不差）

| 维度 | mir2x | Verba 现状 |
|---|---|---|
| 异步不阻塞 | 工作线程 + 轮询 | daemon 内 Rime 查询在后台线程，候选经 IPC 事件回流前端，不阻塞按键 ✅ |
| 候选即时 | 每键立即 | 拼音变化即请求本地 Rime，daemon 启动预热（`warmup_rime`），返回即展示 ✅ |
| 独立候选窗 | 内联面板 | 独立浮窗（跟随光标、分页 9→27、主题/皮肤、水平/垂直布局）✅ |
| 候选排序 | 词频/长度 | Rime 按词典/语言模型质量 ✅ |

### 候选只走本地；LLM 仅用于「输入 → 结果」

- **候选**（打字过程）只来自**本地 Rime（librime）**：Windows `rime.dll` / macOS `librime.dylib`，
  拼音变化即经 `rime_candidates` IPC 请求候选，**不发任何远程 LLM 候选融合请求**。
- **LLM** 只在「输入 → 结果」的 **AI 直输**（`//` + 回车触发 `StartLlm`）时调用，一次一条 prompt，
  打字过程零 LLM 调用。
- 好处：候选本地、零成本、跨端一致；LLM 成本只与主动 AI 使用次数成正比。

### 差异 / 可借鉴

| 维度 | mir2x / libpinyin | Verba 现状 | 建议 |
|---|---|---|---|
| 候选引擎 | libpinyin 进程内（GPL-3.0） | Rime（daemon 内 librime，单引擎） | 保持 Rime（BSD-3，整句 84% vs 自研 6%，见 `chinese-engine-evaluation.md` §8）。不推荐切 libpinyin：GPL 传染、无明显整句优势 |
| 候选时机 | 每键立即（不防抖） | 拼音变化即请求 Rime（Windows 有 ≈320ms 防抖，见下「候选即时性」） | 已关闭该议题：防抖是后端优化，非 UX 延时 |
| 分段承诺 | 支持（stk 逐段确定 + 可回退） | 当前为**整句提交**（Rime 候选 `consumed=active_len`）；`CompositionMachine` 保留分段结构，待 daemon 返回覆盖长度后启用 | 可借鉴 mir2x 的逐段确定手感；Rime 整句准确率已高（84%），优先级低 |
| 面板拖动 | 可拖动 | 未确认（候选窗跟随光标，通常无需拖动） | 低优先，可选 |

### 许可风险（重要）

- `etorth/libpinyin` 是 **GPL-3.0-or-later**；Verba 是 **MIT**。直接链接会把 Verba 传染为 GPL，**不建议作为默认 / 编译期内嵌引擎**。
- 若确要体验 libpinyin 手感，应借鉴其**工程模式**（异步 / 容错选项 / 分段承诺 / 面板跟手），而**不是换引擎**。Rime（BSD-3）更适合 Verba 的开源 + 三平台 + 打包可控定位。

### 跨平台一致性（2026-08-24）

- 单引擎（Rime）逻辑落在共享 `verba-core`（`CompositionMachine`）与 `verba-librime`（跨平台 `platform.rs`，
  libloading 统一加载），**Windows / macOS 共用**，无需在前端重复实现。
- 候选经 `rime_candidates` IPC 一次性请求并压入候选队列（`feed_candidates_event` 融合/去重）。
  Linux 前端尚未落地（低优先）。
- ~~候选即时性~~ **已关闭（2026-08-24）**：曾担心 Windows 的 Rime 候选 ≈320ms 防抖
  （`CANDIDATE_REQ_DEBOUNCE_TICKS=4`）会造成「感知延迟」，但这是**后端优化**，不是 UX 问题；
  单引擎下候选依赖 Rime 查询返回（本地、启动预热），返回前候选窗为空是正常状态。用户实测
  mir2x 无延时；防抖不该被当成「延时」议题，故不调。

---

## 6. 给后续打磨的落地清单（按价值排序）

1. **分段承诺的逐段确定**（可选，手感）：单引擎下 Rime 候选为整句（`consumed=active_len`），当前
   选中即整句提交；若要 mir2x 的「先确定一段、剩余继续选」手感，需 daemon `RimeCandidates`/
   `Candidates` 事件返回「覆盖长度」，再启用 `CompositionMachine` 已保留的 `committed`/`commit_offset`
   结构并补真实分支测试。Rime 整句准确率已高（84%），优先级低。
2. ~~候选即时性校准~~（P1）**已关闭**：单引擎下候选依赖 Rime 查询返回（本地、启动预热）；防抖是
   后端优化，非 UX「延时」，无需打点/调整（2026-08-24）。
3. **打字容错**（P2，可选）：确认 Rime `luna_pinyin_simp` 的模糊音/简拼/纠错选项是否覆盖「打字
   手误」，必要时在 `rime_schema` 或 librime 选项中开启。
4. **面板拖动**（P3，可选）：候选窗增加可拖动（不跟随光标时）；低优先。

---
## 7. 源码位置与关键 API

- mir2x：`../mir2x/client/src/ime.{hpp,cpp}`、`imeboard.{hpp,cpp}`、`ports/libpinyin/*`。
- libpinyin 关键 API（mir2x 调用）：`pinyin_init` / `pinyin_set_options` / `pinyin_alloc_instance` / `pinyin_parse_more_full_pinyins` / `pinyin_guess_sentence_with_prefix` / `pinyin_guess_candidates` / `pinyin_choose_candidate` / `pinyin_get_n_candidate` / `pinyin_get_candidate` / `pinyin_reset` / `pinyin_save` / `pinyin_fini`。
