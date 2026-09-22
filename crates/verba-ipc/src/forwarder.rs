//! 上屏文本后台投递器：把用户提交上屏的文本异步追加进 daemon 的窗口级
//! AI 上下文会话。
//!
//! 为什么需要：前端 `CommitImmediate` 路径在宿主 UI 线程上执行，每条上屏
//! 文本一次同步 IPC 会拖慢打字（每词一次连接/往返）。本组件用常驻 worker
//! 线程批量投递；daemon 不可达时积压重试，不阻塞、不打扰输入。
//!
//! 相邻上屏文本的合并由 **daemon 侧**完成（同一窗口的连续上屏并入一条
//! 消息，上限 `MAX_COMMIT_MERGE_CHARS`），投递层保持逐条原样转发，
//! 避免两处语义漂移。
//!
//! 已知时序边界：投递是异步的，紧跟在 Enter 前上屏的文本可能赶不上
//! 同一次 `//` 请求的历史快照（下一轮可见）。这是「不阻塞输入」与
//! 「即时可见」的取舍，输入法场景可接受。
//!
//! 顺序保证（单发送者假设）：全部 `push`/`push_end` 均发生在宿主 UI
//! 线程（按键/会话激活回调），即 mpsc 只有一个发送者——窗口结束通知
//! 严格排在该窗口全部入队上屏之后，daemon 先收全部上屏、再删会话。
//! 若未来新增非 UI 线程的调用点，必须改走显式排序（如携带窗口会话
//! 代际，daemon 拒绝过期代际的上屏），否则「删除后旧文本复活会话」
//! 的防护会失效。

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use crate::client::{LlmSession, VerbaClient};

/// 一条待投递的项：上屏文本，或窗口会话结束通知。
/// 单通道 FIFO 保证顺序：窗口结束通知一定排在该窗口已入队的上屏文本之后，
/// daemon 先收全部上屏、再删会话，不会出现删除后旧文本又复活会话。
enum CommitItem {
    Commit {
        session_id: u64,
        session_key: Option<String>,
        text: String,
    },
    SessionEnd {
        session_id: u64,
        session_key: Option<String>,
    },
}

impl CommitItem {
    fn session(&self) -> LlmSession<'_> {
        match self {
            CommitItem::Commit {
                session_id,
                session_key,
                ..
            }
            | CommitItem::SessionEnd {
                session_id,
                session_key,
            } => LlmSession {
                id: *session_id,
                key: session_key.as_deref(),
            },
        }
    }
}

/// daemon 不可达时的重试间隔。
const RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// 上屏文本后台投递器（Clone 共享；内部一个常驻 worker 线程）。
#[derive(Clone)]
pub struct CommitForwarder {
    tx: Sender<CommitItem>,
}

impl CommitForwarder {
    /// 启动 worker 线程。
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("verba-commit-forwarder".to_owned())
            .spawn(move || worker(rx))
            .expect("spawn verba-commit-forwarder");
        Self { tx }
    }

    /// 投递一条上屏文本（非阻塞：仅入队；daemon 不可达时 worker 积压重试）。
    /// 空文本直接忽略。
    pub fn push(&self, session: LlmSession<'_>, text: String) {
        if text.trim().is_empty() {
            return;
        }
        let item = CommitItem::Commit {
            session_id: session.id,
            session_key: session.key.map(str::to_owned),
            text,
        };
        if self.tx.send(item).is_err() {
            log::warn!("上屏上下文投递队列已关闭（worker 线程退出），本条丢弃");
        }
    }

    /// 投递窗口会话结束通知（与上屏文本同通道 FIFO，保证删除顺序）。
    pub fn push_end(&self, session: LlmSession<'_>) {
        let item = CommitItem::SessionEnd {
            session_id: session.id,
            session_key: session.key.map(str::to_owned),
        };
        if self.tx.send(item).is_err() {
            log::warn!("会话结束通知投递失败（worker 线程退出），会话将由 daemon LRU 兜底");
        }
    }
}

/// 尝试把积压条目全部投递；连接失败或中途断连时保留剩余条目待下轮重试。
fn flush(pending: &mut VecDeque<CommitItem>) {
    match VerbaClient::connect_verified() {
        Ok(mut client) => {
            while let Some(item) = pending.front() {
                let session = item.session();
                let result = match item {
                    CommitItem::Commit { text, .. } => client.llm_append_context(text, session),
                    CommitItem::SessionEnd { .. } => client.llm_session_end(session),
                };
                match result {
                    Ok(()) => {
                        pending.pop_front();
                    }
                    Err(e) => {
                        log::debug!("上屏上下文投递中断（下轮重试）: {e}");
                        break;
                    }
                }
            }
        }
        Err(e) => log::debug!("daemon 不可达，上屏上下文积压待投: {e}"),
    }
}

fn worker(rx: Receiver<CommitItem>) {
    let mut pending: VecDeque<CommitItem> = VecDeque::new();
    loop {
        // 先消化存量积压（失败则退避后重试，不无限热循环）。
        if !pending.is_empty() {
            flush(&mut pending);
            if !pending.is_empty() {
                std::thread::sleep(RETRY_INTERVAL);
                continue;
            }
        }
        // 阻塞等新条目；所有发送端关闭时投完剩余即退出。
        let first = match rx.recv() {
            Ok(item) => item,
            Err(_) => {
                flush(&mut pending);
                return;
            }
        };
        pending.push_back(first);
        // 吸干此刻在途积压，批量投递（减少连接与握手次数）。
        while let Ok(item) = rx.try_recv() {
            pending.push_back(item);
        }
        flush(&mut pending);
    }
}
