# 手感丝滑的输入法是怎么做的 —— 以 mir2x / libpinyin 为参考

> 更新：2026-08-24 · 状态：技术参考（供 M5 打磨 / 体验调优参考，不构成引擎选型决策）
> 来源：用户反馈 `../mir2x` 里的 libpinyin 输入法「用起来挺丝滑」，本文件拆解其实现，并映射到 Verba 现状。
> 关联：[中文引擎选型与集成评估](chinese-engine-evaluation.md)（引擎选型），本文聚焦「集成/交互工程」层面的手感来源。

---

## 0. 背景

- `../mir2x` 是开源 C++ 传奇游戏（SDL3 + FLTK），为 **SDL 全屏模式** 实现了内嵌 IME。
- 实现文件：
  - `client/src/ime.cpp` / `ime.hpp` —— 封装 libpinyin 的 IME 核心（`_IME_Instance`，内含工作线程）。
  - `client/src/imeboard.cpp` / `imeboard.hpp` —— IME 面板（候选/交互/渲染）。
  - `ports/libpinyin` —— vcpkg 端口，拉取 `etorth/libpinyin`（**GPL-3.0-or-later**，BerkeleyDB 词库，`model20.text` 数据）。
- 代码里显式注释：`// for better implementation, check https://github.com/libpinyin/ibus-libpinyin.git`。

> 一句话结论：**丝滑 = 异步不阻塞 + 高容错候选 + 分段可确定 + 面板跟手**。Verba 已具备其中大部分，最值得补的是「分段承诺」与「候选即时性」。

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

## 5. 与 Verba 现状的对比

### 已对齐（Verba 不差）

| 维度 | mir2x | Verba 现状（`crates/verba-core/src/machine.rs` + `frontends/windows/ime/src/text_service.rs`） |
|---|---|---|
| 异步不阻塞 | 工作线程 + 轮询 | 前端 `start_rime_candidates` / `start_llm_candidates` 在**后台线程**，结果经 `chunks` 队列 + `on_timer` 合并分发，不阻塞按键 ✅ |
| 候选即时 | 每键立即 | `refresh_candidates()` 始终启用内置词库，打字即有即时候选；Rime/LLM 候选异步去重追加 ✅ |
| 独立候选窗 | 内联面板 | 独立浮窗（跟随光标、分页 9→27、主题/皮肤、水平/垂直布局）✅ |
| 候选排序 | 词频/长度 | 内置按词频；Rime 按词典质量 ✅ |

### 差异 / 可借鉴

| 维度 | mir2x / libpinyin | Verba 现状 | 建议 |
|---|---|---|---|
| 候选引擎 | libpinyin 进程内（GPL-3.0） | Rime（daemon IPC）+ 内置 | **保持 Rime**（BSD-3，整句 84% vs 自研 6%，见 `chinese-engine-evaluation.md` §8）。不推荐切 libpinyin：GPL 传染、无明显整句优势 |
| 候选计算时机 | 每键立即（不防抖） | Rime 候选经 ≈320ms 防抖（`CANDIDATE_REQ_DEBOUNCE_TICKS=4`×80ms） | 实测手感。「整句候选」晚半拍可接受（它本就是补充增强）；若首候选也延迟明显，缩短/取消 Rime 防抖或改「立即 + 合并」 |
| 打字容错 | `incomplete` + `correct_all` + `dynamic_adjust` | 内置支持 模糊音/简拼，无逐字 auto-correct | 可选：给 `verba-pinyin` 评估弱「自动纠错」，或确认 Rime `luna_pinyin_simp` 已覆盖常见错音 |
| **分段承诺** | 支持（stk 逐段确定 + 可回退） | ✅ 已实现（2026-08-24）：内置候选经 `lookup_segmented` 支持逐段确定 + 可回退 | 之前是最值得补的缺口，现已落地；Rime/LLM 整句候选仍走整句 |
| 面板拖动 | 可拖动 | 未确认（候选窗跟随光标，通常无需拖动） | 低优先，可选 |

### 许可风险（重要）

- `etorth/libpinyin` 是 **GPL-3.0-or-later**；Verba 是 **MIT**。直接链接会把 Verba 传染为 GPL，**不建议作为默认 / 编译期内嵌引擎**。
- 若确要体验 libpinyin 手感，应借鉴其**工程模式**（异步 / 容错选项 / 分段承诺 / 面板跟手），而**不是换引擎**。Rime（BSD-3）更适合 Verba 的开源 + 三平台 + 打包可控定位。

### 跨平台一致性（2026-08-24）

- 「分段承诺」核心逻辑落在共享 `verba-core`（`CompositionMachine`），**三端共用**，无需在前端重复实现。
- `engine=rime` 整句候选此前仅在 Windows TSF 接入；macOS IMK 已对齐：`start_candidates` 读取
  `config.engine/rime_schema`，`engine=rime` 时经 `rime_candidates` IPC 一次性请求 Rime 候选并压入
  候选队列（同 `feed_candidates_event` 融合/去重）。Linux 前端尚未落地（低优先）。
- 已知差异：Windows 对 Rime/LLM 候选请求做了 ≈320ms 防抖（`CANDIDATE_REQ_DEBOUNCE_TICKS`），
  macOS 候选请求即时触发。属性能调优差异，非逻辑差异；如需统一，可在 `verba-config` 增加候选
  请求防抖配置供两端读取。

---

## 6. 给 M5 打磨的落地清单（按价值排序）

1. **分段承诺**（P0，手感）✅ 已实现（2026-08-24）：`verba-pinyin::PinyinEngine::lookup_segmented` 返回带 `consumed` 覆盖长度的候选；`CompositionMachine` 增加 `committed`/`commit_offset`，选候选只推进段偏移、`Backspace` 弹回上一段、已消费完自动整句提交。Pinyin 态（内置候选）走分段，Rime / LLM 候选仍覆盖整句（`on_llm_candidates` 一律 `consumed = 活跃拼音全长`）。
2. **候选即时性校准**（P1，量测）：打点测「按键 → Rime 首候选呈现」延迟；若 >150ms 且首候选常来自 Rime，则缩短防抖或去掉对 Rime 的防抖。
3. **容错选项**（P2，可选）：确认 `verba-pinyin` 的模糊音/简拼是否覆盖「打字手误」，必要时补弱 auto-correct。
4. **面板拖动**（P3，可选）：候选窗增加可拖动（不跟随光标时）；低优先。

---

## 7. 源码位置与关键 API

- mir2x：`../mir2x/client/src/ime.{hpp,cpp}`、`imeboard.{hpp,cpp}`、`ports/libpinyin/*`。
- libpinyin 关键 API（mir2x 调用）：`pinyin_init` / `pinyin_set_options` / `pinyin_alloc_instance` / `pinyin_parse_more_full_pinyins` / `pinyin_guess_sentence_with_prefix` / `pinyin_guess_candidates` / `pinyin_choose_candidate` / `pinyin_get_n_candidate` / `pinyin_get_candidate` / `pinyin_reset` / `pinyin_save` / `pinyin_fini`。
