// ============================================================================
// ChatActiveRuns — 活跃 chat 运行的控制注册表
// ============================================================================
//
// chat 链路的 looper 由 SSE 请求任务独占持有（见 `handler::stream_chat`），
// 请求之外无从触达；暂停/恢复必须在**另一条 HTTP 请求**里对在途 looper 施加
// 控制，故这里给出「会话 → 控制通道」的注册表：
//
//   POST .../pause  ─┐
//                    ├─→ ChatActiveRuns ──mpsc──→ SSE 任务 ──→ LooperHandle::pause()/resume()
//   POST .../resume ─┘
//
// 条目与 SSE 任务同寿命：任务结束由 [`ChatRunGuard`] 移除。同一会话的重复
// stream 请求**顶替**旧条目 —— 顶替即丢弃旧发送端，旧任务看到控制通道关闭
// 便退出，避免出现「旧连接仍握着暂停开关」的错配。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

/// 控制命令 —— SSE 任务转译为 `LooperHandle::pause()` / `resume()`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatControl {
    Pause,
    Resume,
}

/// 控制通道容量。命令在 SSE 任务的 `select!` 上即时消费，容量只作背压缓冲。
const CONTROL_CHANNEL_CAPACITY: usize = 8;

struct Entry {
    /// 注册代次标识 —— 顶替后旧守卫不得误删新条目。
    token: u64,
    tx: mpsc::Sender<ChatControl>,
}

type Registry = Arc<Mutex<HashMap<String, Entry>>>;

/// 会话级控制注册表（每会话至多一个在途 run）。
pub struct ChatActiveRuns {
    inner: Registry,
    next_token: AtomicU64,
}

impl Default for ChatActiveRuns {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatActiveRuns {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            next_token: AtomicU64::new(1),
        }
    }

    /// 注册一个会话的运行，返回控制接收端与清理守卫。
    ///
    /// 同一 `conversation_id` 重复注册会顶替旧条目：旧发送端随旧 `Entry` 一起
    /// 丢弃，旧任务据此从 `control_rx` 收到 `None` 并退出。
    pub fn register(&self, conversation_id: &str) -> (mpsc::Receiver<ChatControl>, ChatRunGuard) {
        let (tx, rx) = mpsc::channel(CONTROL_CHANNEL_CAPACITY);
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        self.inner
            .lock()
            .unwrap()
            .insert(conversation_id.to_string(), Entry { token, tx });
        (
            rx,
            ChatRunGuard {
                inner: Arc::clone(&self.inner),
                conversation_id: conversation_id.to_string(),
                token,
            },
        )
    }

    /// 请求暂停；`false` 表示该会话当前无在途运行（或控制通道已关闭）。
    pub fn pause(&self, conversation_id: &str) -> bool {
        self.send(conversation_id, ChatControl::Pause)
    }

    /// 请求恢复；语义同 [`Self::pause`]。
    pub fn resume(&self, conversation_id: &str) -> bool {
        self.send(conversation_id, ChatControl::Resume)
    }

    fn send(&self, conversation_id: &str, cmd: ChatControl) -> bool {
        let map = self.inner.lock().unwrap();
        map.get(conversation_id)
            .is_some_and(|entry| entry.tx.try_send(cmd).is_ok())
    }

    /// 该会话当前是否注册了在途运行。
    pub fn is_active(&self, conversation_id: &str) -> bool {
        self.inner.lock().unwrap().contains_key(conversation_id)
    }
}

/// 注册表条目的生命周期守卫 —— 随 SSE 任务 drop 时移除。
///
/// 只在条目**仍属于本次注册**（token 相同）时移除：被新 stream 顶替后，旧守卫
/// 不得删掉新条目，否则新连接会失去暂停控制。
pub struct ChatRunGuard {
    inner: Registry,
    conversation_id: String,
    token: u64,
}

impl Drop for ChatRunGuard {
    fn drop(&mut self) {
        let mut map = self.inner.lock().unwrap();
        if map
            .get(&self.conversation_id)
            .is_some_and(|e| e.token == self.token)
        {
            map.remove(&self.conversation_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_is_exclusive_and_guard_cleans_up() {
        let registry = ChatActiveRuns::new();
        let (_rx, guard) = registry.register("c1");
        assert!(registry.is_active("c1"));

        drop(guard);
        assert!(!registry.is_active("c1"), "守卫 drop 清理条目");
        assert!(!registry.is_active("c2"), "其他会话不受影响");
    }

    #[tokio::test]
    async fn pause_and_resume_deliver_commands() {
        let registry = ChatActiveRuns::new();
        let (mut rx, _guard) = registry.register("c1");

        assert!(!registry.pause("c2"), "无 run 时暂停失败");

        assert!(registry.pause("c1"));
        assert_eq!(rx.recv().await, Some(ChatControl::Pause));
        assert!(registry.resume("c1"));
        assert_eq!(rx.recv().await, Some(ChatControl::Resume));
    }

    #[tokio::test]
    async fn re_register_supersedes_old_channel() {
        let registry = ChatActiveRuns::new();
        let (mut old_rx, _old_guard) = registry.register("c1");
        let (mut new_rx, _new_guard) = registry.register("c1");

        assert_eq!(old_rx.recv().await, None, "顶替后旧通道关闭");

        assert!(registry.pause("c1"), "控制落到新条目");
        assert_eq!(new_rx.recv().await, Some(ChatControl::Pause));
    }

    #[tokio::test]
    async fn stale_guard_does_not_remove_new_entry() {
        let registry = ChatActiveRuns::new();
        let (_old_rx, old_guard) = registry.register("c1");
        let (mut new_rx, _new_guard) = registry.register("c1");

        drop(old_guard);
        assert!(registry.is_active("c1"), "旧守卫不得删除新条目");

        assert!(registry.resume("c1"));
        assert_eq!(new_rx.recv().await, Some(ChatControl::Resume));
    }
}
