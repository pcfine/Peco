// ============================================================================
// ChatActiveRuns — 活跃 chat 运行的控制注册表
// ============================================================================
//
// 一轮 chat 的 looper 由 run 任务独占持有（见 `chat::run`），请求之外无从触达；
// 暂停 / 恢复 / 取消必须在**另一条 HTTP 请求**里对在途 looper 施加控制，故这里
// 给出「会话 → 控制通道」的注册表：
//
//   POST .../pause   ─┐
//   POST .../resume  ─┼─→ ChatActiveRuns ──mpsc──→ run 任务 ──→ LooperHandle::pause()/resume()/cancel()
//   POST .../cancel  ─┘
//
// 条目与 run 任务同寿命（任务结束由 [`ChatRunGuard`] 移除），条目里同时挂着这一轮
// 的 [`TurnFeed`]：两者必须同寿命，否则会出现「feed 还在、控制没了」或反之。
//
// 断连不是事件 —— 连接只是这一轮的一个出口，不是它的所有权。所以 HTTP 入口与
// 唤醒入口都用 [`ChatActiveRuns::try_register`]：有在途轮就是忙（409），不再有
// 「新连接顺手挤掉旧连接」的顶替语义。[`ChatActiveRuns::register`] 保留顶替形态，
// 供注册表自身的守卫与 token 保护被单测直接驱动。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use super::feed::{LiveFrames, TurnFeed};

/// 控制命令 —— run 任务转译为 `LooperHandle::pause()` / `resume()` / `cancel()`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatControl {
    Pause,
    Resume,
    /// 会话级取消：收尾在 looper 的步进边界，可能晚于 HTTP 响应。
    Cancel,
}

/// 控制通道容量。命令在 SSE 任务的 `select!` 上即时消费，容量只作背压缓冲。
const CONTROL_CHANNEL_CAPACITY: usize = 8;

struct Entry {
    /// 注册代次标识 —— 同会话两代条目并存时，旧守卫不得误删新条目。
    token: u64,
    tx: mpsc::Sender<ChatControl>,
    /// 这一轮的帧扇出。与条目同寿命：条目在就说明这一轮在跑（或刚刚跑完、行已落库）。
    feed: Arc<TurnFeed>,
}

type Registry = Arc<Mutex<HashMap<String, Entry>>>;

/// 一次运行的控制端：控制接收端 + 清理守卫。
///
/// 由注册表产出，因为两者（通道与条目生命周期）必须同源：谁注册的条目就该由谁
/// 的守卫移除，否则顶替后新旧两代守卫会互相删除。
pub(crate) struct RunRegistration {
    pub(crate) control_rx: mpsc::Receiver<ChatControl>,
    pub(crate) guard: ChatRunGuard,
    /// 这一轮的出口；由注册表创建，故条目与运行侧拿到的是同一个 `Arc`。
    pub(crate) feed: Arc<TurnFeed>,
}

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

    /// 顶替形态的注册：返回控制接收端与清理守卫。
    ///
    /// 同一 `conversation_id` 重复注册会顶替旧条目：旧发送端随旧 `Entry` 一起
    /// 丢弃，旧任务据此从 `control_rx` 收到 `None` 并退出。两条产品入口都不走
    /// 它（见文件头），这里保留的是注册表自身的原语 —— [`ChatRunGuard`] 的
    /// token 校验只有在两代条目可能并存时才有意义。
    pub fn register(&self, conversation_id: &str) -> (mpsc::Receiver<ChatControl>, ChatRunGuard) {
        let reg = self.insert_locked(&mut self.inner.lock().unwrap(), conversation_id);
        (reg.control_rx, reg.guard)
    }

    /// 无客户端的运行注册：该会话已有条目时返回 `None`，**不顶替**。
    ///
    /// 「查 + 插」必须在同一把锁内完成：分成两步做，两个投递者会同时看到空闲，
    /// 后一个把前一个刚起的运行顶掉。
    pub(crate) fn try_register(&self, conversation_id: &str) -> Option<RunRegistration> {
        let mut map = self.inner.lock().unwrap();
        if map.contains_key(conversation_id) {
            return None;
        }
        Some(self.insert_locked(&mut map, conversation_id))
    }

    /// 会话是否有在途运行：条目存在且控制发送端未关闭。
    ///
    /// 语义是「这一轮真的在跑，与连接无关」：run 任务把一轮跑完才 drop 守卫，
    /// 客户端断连不影响它。页面开着而本轮已结束的会话在这里不算忙 —— 唤醒队列
    /// 因此不会为一条已经关掉的连接永久让路。
    ///
    /// 表里有条目不等于跑着：条目要到守卫 drop 才移除，而任务可能早已退出。
    /// 据此判「忙」会把已死的运行当成在跑的。
    pub fn is_active(&self, conversation_id: &str) -> bool {
        let map = self.inner.lock().unwrap();
        map.get(conversation_id)
            .is_some_and(|entry| !entry.tx.is_closed())
    }

    /// 持有锁的前提下写入新条目，调用方负责决定「顶替」还是「已存在即放弃」。
    fn insert_locked(
        &self,
        map: &mut HashMap<String, Entry>,
        conversation_id: &str,
    ) -> RunRegistration {
        let (tx, control_rx) = mpsc::channel(CONTROL_CHANNEL_CAPACITY);
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let feed = Arc::new(TurnFeed::new(conversation_id.to_string()));
        map.insert(
            conversation_id.to_string(),
            Entry {
                token,
                tx,
                feed: Arc::clone(&feed),
            },
        );
        RunRegistration {
            control_rx,
            guard: ChatRunGuard {
                inner: Arc::clone(&self.inner),
                conversation_id: conversation_id.to_string(),
                token,
            },
            feed,
        }
    }

    /// 请求暂停；`false` 表示该会话当前无在途运行（或控制通道已关闭）。
    pub fn pause(&self, conversation_id: &str) -> bool {
        self.send(conversation_id, ChatControl::Pause)
    }

    /// 请求恢复；语义同 [`Self::pause`]。
    pub fn resume(&self, conversation_id: &str) -> bool {
        self.send(conversation_id, ChatControl::Resume)
    }

    /// 请求取消这一轮；语义同 [`Self::pause`]（返回 `false` 表示无在途轮）。
    pub fn cancel(&self, conversation_id: &str) -> bool {
        self.send(conversation_id, ChatControl::Cancel)
    }

    /// 加入该会话在途轮的帧流；无在途轮、或这一轮已进入终态时返回 `None`
    /// （调用方据此回 204：历史已是权威）。
    ///
    /// 先克隆 `Arc` 再放注册表锁：`attach` 的原子性由 feed 内部那把锁保证，
    /// 没必要持着外层的锁去拿里层的。
    pub(crate) fn attach(&self, conversation_id: &str) -> Option<LiveFrames> {
        let feed = {
            let map = self.inner.lock().unwrap();
            Arc::clone(&map.get(conversation_id)?.feed)
        };
        feed.attach()
    }

    fn send(&self, conversation_id: &str, cmd: ChatControl) -> bool {
        let map = self.inner.lock().unwrap();
        map.get(conversation_id)
            .is_some_and(|entry| entry.tx.try_send(cmd).is_ok())
    }
}

/// 注册表条目的生命周期守卫 —— 随 run 任务 drop 时移除。
///
/// 只在条目**仍属于本次注册**（token 相同）时移除：同会话两代条目可能短暂并存
/// （顶替形态的注册），旧守卫不得删掉新条目，否则新的运行会失去控制端。
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
        assert!(registry.pause("c1"), "已注册条目可接收控制");

        drop(guard);
        assert!(!registry.pause("c1"), "守卫 drop 清理条目");
        assert!(!registry.pause("c2"), "其他会话不受影响");
    }

    #[tokio::test]
    async fn try_register_refuses_a_live_run() {
        let registry = ChatActiveRuns::new();
        let first = registry.try_register("c1").expect("空闲会话可注册");
        assert!(registry.is_active("c1"));

        assert!(registry.try_register("c1").is_none(), "在途运行不得被顶替");

        drop(first);
        assert!(!registry.is_active("c1"), "守卫 drop 后不再算在途");
        assert!(registry.try_register("c1").is_some(), "清理后可再注册");
    }

    #[tokio::test]
    async fn is_active_needs_a_live_control_channel() {
        let registry = ChatActiveRuns::new();
        let (rx, _guard) = registry.register("c1");
        assert!(registry.is_active("c1"));

        drop(rx);
        assert!(!registry.is_active("c1"), "接收端已 drop 的运行不算在途");
    }

    #[tokio::test]
    async fn user_register_supersedes_an_unowned_run() {
        let registry = ChatActiveRuns::new();
        let mut wake = registry.try_register("c1").expect("空闲会话可注册");
        assert!(registry.is_active("c1"), "唤醒侧持有条目期间算在途");

        let (_rx, _guard) = registry.register("c1");
        assert_eq!(
            wake.control_rx.recv().await,
            None,
            "用户侧顶替唤醒侧后旧通道关闭"
        );
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
        assert!(registry.resume("c1"), "旧守卫不得删除新条目");
        assert_eq!(new_rx.recv().await, Some(ChatControl::Resume));
    }
}
