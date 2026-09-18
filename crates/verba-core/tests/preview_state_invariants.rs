//! 预览态不变量：**穷举搜索**「主状态与预览槽不一致」的可达组合。
//!
//! 背景（真机 2026-09-18）：用户报「启动/首次激活后第一次输入 nihao 选候选
//! 不上屏，第二次起正常」。日志显示首个空格打的是 core 的 `改写预览键: Space`
//! 分支、`commit text=` 是空串——即 `state == Idle` 却让对照预览槽继续生效，
//! 吞掉确认键并上屏陈旧文本。
//!
//! 定点单测只能钉住想到了的那条路径；这里用「合法武装 → 有界穷举后续操作」
//! 的方式验证不变量：**任何 ≤4 步操作序列都不得让「主状态已离开 ResultReady
//! 而对照槽还在」成立**。搜索深度 4 覆盖 34^4 ≈ 133 万条序列（实测 1-3s）。
//!
//! 判据刻意直探槽位（`feed_rewrite_preview(Enter)` 能否拿到陈旧上屏），
//! 而不是拿被收紧的 `rewrite_previewing()` 去判它自己——后者会让测试恒真。
//! 证伪方式（已实测）：临时去掉 `feed_escape` 非 Idle 臂里的
//! `clear_composition_state()`，本测试立刻报红并给出最小序列 `[0,0,0,8]`。
//! `rewrite_previewing()` 的状态判别与 `begin_rewrite_preview` 的拒绝门本身由
//! machine.rs 内的白盒单测钉住（`stale_rewrite_slot_outside_result_ready_never_hijacks_keys`、
//! `preview_slots_are_mutually_exclusive` 等），它们都验证过「还原修复即红」。
//!
//! **已知局限**（独立审查指出，如实记录）：探针用 `feed_rewrite_preview(Enter)`
//! 间接读槽，若将来给它加状态门，即使槽仍泄漏本守卫也会假绿——届时应改为
//! 直接断言私有字段（把本文件移进 `mod tests`）。此外穷举只驱动 core 的公开
//! 方法：不覆盖真实 IMK/TSF 事件序、IPC 延迟、跨控制器、`apply_action` 重入、
//! 前端镜像槽与 UI 可见性，也不推进 `expire_stale_ocr_preview` 的 TTL 时钟。
use verba_core::machine::{AiKey, CompositionMachine, MachineState, PreviewKey};

fn pk(i: usize) -> PreviewKey {
    match i {
        0 => PreviewKey::Enter,
        1 => PreviewKey::Space,
        2 => PreviewKey::Digit1,
        3 => PreviewKey::Digit2,
        4 => PreviewKey::Escape,
        _ => PreviewKey::Other,
    }
}

/// 执行第 i 个操作；返回该步之后是否处于「违反不变量」的组合。
fn step(m: &mut CompositionMachine, i: usize) -> bool {
    match i {
        0..=7 => {
            let c = ['n', 'i', 'A', ' ', '/', '\t', '\n', '\u{8}'][i];
            let _ = m.feed_char(c);
        }
        8 => {
            let _ = m.feed_escape();
        }
        9 => {
            let _ = m.feed_backspace();
        }
        10 => {
            let _ = m.feed_enter();
        }
        11 => {
            let _ = m.feed_arrow_up();
        }
        12 => {
            let _ = m.feed_arrow_down();
        }
        13 => {
            let _ = m.feed_page_down();
        }
        14 => {
            let _ = m.begin_ocr_preview("O".into());
        }
        15..=19 => {
            // feed_ocr_preview 的前置是 OcrPreviewing（debug 断言），按真实
            // 调用契约只在预览态喂。
            if m.state() == MachineState::OcrPreviewing {
                let _ = m.feed_ocr_preview(pk(i - 15));
            }
        }
        20 => m.end_ocr_preview(),
        21 => {
            let _ = m.expire_stale_ocr_preview();
        }
        22 => {
            let _ = m.begin_rewrite_preview("R".into(), "S".into());
        }
        23..=26 => {
            let _ = m.feed_rewrite_preview(pk(i - 23));
        }
        27 => {
            let _ = m.on_llm_chunk("c");
        }
        28 => {
            let _ = m.on_llm_done();
        }
        29 => {
            let _ = m.on_llm_error("e");
        }
        30 => {
            let _ = m.feed_ai_preview(AiKey::Enter);
        }
        31 => {
            let _ = m.feed_ai_preview(AiKey::Escape);
        }
        32 => {
            let _ = m.feed_ai_preview(AiKey::Retry);
        }
        33 => {
            let _ = m.feed_ai_preview(AiKey::Edit);
        }
        34 => {
            let _ = m.on_llm_candidates("n", &["你".to_owned()], true);
        }
        _ => unreachable!(),
    }
    // 不变量（**可证伪**判据：直探槽位本身，而不是用被收紧的
    // rewrite_previewing() 去判它自己）：主状态离开 ResultReady 后，
    // 对照槽必须已被回收——否则直喂预览键仍会产出陈旧上屏。
    residue_leaks(m)
}

/// 主状态不是 ResultReady，却仍有对照槽（直喂预览键能拿到陈旧上屏）。
///
/// 用 `feed_rewrite_preview(Enter)` 当探针：槽在 → `Some(CommitImmediate(..))`；
/// 槽已回收 → `None`。探针本身有副作用（命中即清槽），因此只在确认违规时调用。
fn residue_leaks(m: &mut CompositionMachine) -> bool {
    if m.state() == MachineState::ResultReady {
        return false;
    }
    m.feed_rewrite_preview(PreviewKey::Enter).is_some()
}

const OPS: usize = 35;

/// 合法武装前奏：`//A` + Tab 起改写流 → chunk → Final（on_llm_done 产出
/// RewriteReady）→ 前端 apply_action 调 begin_rewrite_preview。
const PROLOGUE: [usize; 7] = [4, 4, 2, 5, 27, 28, 22];

fn armed() -> CompositionMachine {
    let mut m = CompositionMachine::new();
    for &s in &PROLOGUE {
        let _ = step(&mut m, s);
    }
    m
}

fn replay_hit(seq: &[usize]) -> bool {
    let mut m = armed();
    seq.iter().any(|&s| step(&mut m, s))
}

fn rec(path: &mut Vec<usize>, depth: usize) -> Option<Vec<usize>> {
    if path.len() == depth {
        return None;
    }
    for i in 0..OPS {
        // 22 = begin_rewrite_preview：重复武装是最平凡的构造，排除掉逼搜索
        // 走「武装之后如何被错误地留在非 ResultReady 态」这条真问题路径。
        if i == 22 {
            continue;
        }
        path.push(i);
        if replay_hit(path) {
            return Some(path.clone());
        }
        if let Some(found) = rec(path, depth) {
            return Some(found);
        }
        path.pop();
    }
    None
}

#[test]
fn preview_is_active_only_in_result_ready() {
    let m = armed();
    assert!(
        m.rewrite_previewing() && m.state() == MachineState::ResultReady,
        "前奏应武装出对照预览态，实际 state={:?} previewing={}",
        m.state(),
        m.rewrite_previewing()
    );

    // 判据的阴性方向：armed（ResultReady）不算违规。
    //
    // 阳性对照不在这里——集成测试读不到私有槽，构造不出「非 ResultReady 且槽在」
    // 的现场；该判据的「有牙」由变异测试证明：删掉 feed_escape 非 Idle 臂的
    // clear_composition_state() 后本测试报红并给出最小序列 [0,0,0,8]（见文件头）。
    let mut probe = armed();
    assert!(
        !residue_leaks(&mut probe),
        "ResultReady 态不应被判为残留泄漏"
    );

    let mut path = Vec::new();
    if let Some(seq) = rec(&mut path, 4) {
        panic!("发现可达的「主状态已离开 ResultReady 而对照槽仍泄漏」序列: {seq:?}");
    }
}
