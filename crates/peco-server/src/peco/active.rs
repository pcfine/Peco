// ============================================================================
// PecoActiveRuns — 活跃 Peco 运行注册表
// ============================================================================
//
// 事件转发架构（对齐 workflow/active.rs 的 broadcast + 控制通道范式）：
//
//   LooperHandle ──→ runner 任务 ──→ broadcast::Sender<LooperEvent>
//                                        │
//                   mpsc (Query/Cancel)  ├── 桥接任务 ×N（每 SSE 连接一个，随时附着）
//                   (控制命令)
//
// runner 任务独占 LooperHandle，SSE 连接断开只结束对应的桥接任务，
// 运行中的轮次不受影响；重开页面通过 subscribe() 重新附着。
//
// 轮边界回收：looper 进入 Idle（当前轮结束且无排队输入）且无订阅者时，
// runner 主动退出并 drop handle，停靠的 looper 随之优雅终止 — 防止
// 无人观看的 parked looper 长期占用内存。桥接任务退出时通过
// request_reclaim 唤醒 runner 复查（覆盖「Idle 停靠之后订阅者才消失」）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use peco_core::agent::LooperEvent;
use tokio::sync::{Notify, broadcast, mpsc};

/// 发给 runner 的控制命令。
#[derive(Debug)]
pub enum ControlCommand {
    /// 排队一条用户消息（looper Active 时自动入 pending 队列，当前轮结束后续接）。
    Query(String),
    /// 取消在途轮次（looper 在下个检查点收尾并退出）。
    Cancel,
}

/// 注册表条目：runner 之外的所有交互都经由这份句柄。
struct ActiveEntry {
    control_tx: mpsc::Sender<ControlCommand>,
    event_tx: broadcast::Sender<LooperEvent>,
    /// 桥接任务退出时 notify，runner 醒来复查是否可回收。
    reclaim_notify: Arc<Notify>,
}

/// runner 退出 / 构建失败 / panic unwind 时自清理注册表的守卫。
pub struct RunGuard {
    inner: Arc<Mutex<HashMap<String, ActiveEntry>>>,
    user_id: String,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.inner.lock().unwrap().remove(&self.user_id);
    }
}

/// `try_register` 的产物：runner 任务的输入 + 生命周期守卫。
///
/// 持有期间该用户的 run 在注册表中可见（is_running = true）；
/// drop 时从注册表移除（幂等）。所有字段随 runner 任务移入持有。
pub struct RunRegistration {
    pub control_rx: mpsc::Receiver<ControlCommand>,
    pub event_tx: broadcast::Sender<LooperEvent>,
    pub reclaim_notify: Arc<Notify>,
    pub _guard: RunGuard,
}

/// 取消并等待收尾的结果。
#[derive(Debug, PartialEq, Eq)]
pub enum CancelWaitResult {
    /// 无活跃 run。
    NoRun,
    /// run 已退出（注册表条目消失）。
    Exited,
    /// 超时未退出（如模型调用在途，取消要等下个检查点）。
    TimedOut,
}

/// 活跃 Peco 运行注册表，按 user_id 隔离（每用户至多一个 run）。
///
/// 所有同步方法的临界区内无 await；跨任务的等待通过轮询
/// `is_running` 完成（回收只发生在 looper 停靠态，退出是毫秒级）。
pub struct PecoActiveRuns {
    inner: Arc<Mutex<HashMap<String, ActiveEntry>>>,
}

impl Default for PecoActiveRuns {
    fn default() -> Self {
        Self::new()
    }
}

impl PecoActiveRuns {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 原子抢注：该用户尚无 run 时创建条目并返回注册产物，否则返回 None。
    ///
    /// 在耗时的 PecoManager 构建**之前**调用，构建期间其他连接可附着
    /// （条目已存在）；构建失败直接 drop 返回值即完成清理。
    pub fn try_register(&self, user_id: &str) -> Option<RunRegistration> {
        let mut map = self.inner.lock().unwrap();
        if map.contains_key(user_id) {
            return None;
        }
        let (control_tx, control_rx) = mpsc::channel::<ControlCommand>(32);
        let (event_tx, _rx) = broadcast::channel::<LooperEvent>(256);
        let reclaim_notify = Arc::new(Notify::new());
        map.insert(
            user_id.to_string(),
            ActiveEntry {
                control_tx: control_tx.clone(),
                event_tx: event_tx.clone(),
                reclaim_notify: Arc::clone(&reclaim_notify),
            },
        );
        Some(RunRegistration {
            control_rx,
            event_tx,
            reclaim_notify,
            _guard: RunGuard {
                inner: Arc::clone(&self.inner),
                user_id: user_id.to_string(),
            },
        })
    }

    /// 订阅指定用户的 LooperEvent 广播；无 run 时返回 None。
    pub fn subscribe(&self, user_id: &str) -> Option<broadcast::Receiver<LooperEvent>> {
        let map = self.inner.lock().unwrap();
        map.get(user_id).map(|entry| entry.event_tx.subscribe())
    }

    /// 向活跃 run 排队一条用户消息。
    ///
    /// 返回 false 表示无 run 或控制通道已满 — 调用方应改走新建路径。
    pub fn enqueue_query(&self, user_id: &str, text: String) -> bool {
        let map = self.inner.lock().unwrap();
        match map.get(user_id) {
            Some(entry) => entry
                .control_tx
                .try_send(ControlCommand::Query(text))
                .is_ok(),
            None => false,
        }
    }

    /// 请求取消活跃 run 的在途轮次；无 run 返回 false。
    pub fn cancel(&self, user_id: &str) -> bool {
        let map = self.inner.lock().unwrap();
        match map.get(user_id) {
            Some(entry) => entry.control_tx.try_send(ControlCommand::Cancel).is_ok(),
            None => false,
        }
    }

    /// 取消并轮询等待 run 退出。
    pub async fn cancel_and_wait(&self, user_id: &str, max_wait: Duration) -> CancelWaitResult {
        if !self.cancel(user_id) {
            return CancelWaitResult::NoRun;
        }
        if self.wait_until_absent(user_id, max_wait).await {
            CancelWaitResult::Exited
        } else {
            CancelWaitResult::TimedOut
        }
    }

    /// 轮询等待注册表条目消失（runner 退出 → guard 清理）。
    pub async fn wait_until_absent(&self, user_id: &str, max_wait: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + max_wait;
        loop {
            if !self.is_running(user_id) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// 若该用户有 run 且 broadcast 已无订阅者，移除条目并返回 true。
    ///
    /// 与 `subscribe` 共用同一把锁：subscribe 先拿到锁则 receiver_count ≥ 1
    /// 不回收（热备 looper 继续服务新消息）；本方法先拿到锁则条目被移除，
    /// 后续 subscribe 返回 None，调用方改走新建路径 — 消息不丢、无双 run。
    pub fn try_reclaim_if_unsubscribed(&self, user_id: &str) -> bool {
        let mut map = self.inner.lock().unwrap();
        match map.get(user_id) {
            Some(entry) if entry.event_tx.receiver_count() == 0 => {
                map.remove(user_id);
                true
            }
            _ => false,
        }
    }

    /// 唤醒 runner 复查回收条件（桥接任务退出时调用）。
    ///
    /// runner 是否真正回收由它自己判断（looper 是否 Idle）；无 run 时为 no-op。
    pub fn request_reclaim(&self, user_id: &str) {
        let map = self.inner.lock().unwrap();
        if let Some(entry) = map.get(user_id) {
            entry.reclaim_notify.notify_waiters();
        }
    }

    /// 该用户是否有活跃 run。
    pub fn is_running(&self, user_id: &str) -> bool {
        self.inner.lock().unwrap().contains_key(user_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn try_register_is_exclusive_and_guard_cleans_up() {
        let registry = PecoActiveRuns::new();
        let reg = registry.try_register("u1").expect("first register");
        assert!(registry.is_running("u1"));
        assert!(registry.try_register("u1").is_none(), "double register");
        assert!(registry.try_register("u2").is_some(), "其他用户不受影响");

        drop(reg);
        assert!(!registry.is_running("u1"), "guard drop 清理条目");
    }

    #[test]
    fn reclaim_requires_zero_subscribers() {
        let registry = PecoActiveRuns::new();
        let reg = registry.try_register("u1").expect("register");

        let rx = registry.subscribe("u1").expect("subscribe");
        assert!(
            !registry.try_reclaim_if_unsubscribed("u1"),
            "有订阅者不回收"
        );
        assert!(registry.is_running("u1"));

        drop(rx);
        assert!(
            registry.try_reclaim_if_unsubscribed("u1"),
            "订阅者消失后可回收"
        );
        assert!(!registry.is_running("u1"));
        drop(reg); // 幂等：条目已被 reclaim 移除
    }

    #[test]
    fn subscribe_after_reclaim_returns_none() {
        let registry = PecoActiveRuns::new();
        let reg = registry.try_register("u1").expect("register");
        assert!(registry.try_reclaim_if_unsubscribed("u1"));
        assert!(registry.subscribe("u1").is_none(), "回收后不可附着");
        drop(reg);
    }

    #[tokio::test]
    async fn enqueue_and_cancel_deliver_commands() {
        let registry = PecoActiveRuns::new();
        let mut reg = registry.try_register("u1").expect("register");

        assert!(
            !registry.enqueue_query("u2", "hi".into()),
            "无 run 入队失败"
        );
        assert!(registry.enqueue_query("u1", "hi".into()));
        assert!(registry.cancel("u1"));
        assert!(!registry.cancel("u2"), "无 run 取消失败");

        assert!(matches!(
            reg.control_rx.recv().await,
            Some(ControlCommand::Query(text)) if text == "hi"
        ));
        assert!(matches!(
            reg.control_rx.recv().await,
            Some(ControlCommand::Cancel)
        ));
    }

    #[tokio::test]
    async fn cancel_and_wait_reports_no_run() {
        let registry = PecoActiveRuns::new();
        assert_eq!(
            registry
                .cancel_and_wait("nobody", Duration::from_millis(50))
                .await,
            CancelWaitResult::NoRun
        );
    }

    #[tokio::test]
    async fn cancel_and_wait_exits_when_registration_dropped() {
        let registry = PecoActiveRuns::new();
        let reg = registry.try_register("u1").expect("register");
        assert!(registry.cancel("u1"));

        // 模拟 runner 收到取消后延迟退出
        let drop_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(reg);
        });
        let result = registry.cancel_and_wait("u1", Duration::from_secs(2)).await;
        drop_task.await.unwrap();
        assert_eq!(result, CancelWaitResult::Exited);
    }

    #[tokio::test]
    async fn cancel_and_wait_times_out_while_alive() {
        let registry = PecoActiveRuns::new();
        let _reg = registry.try_register("u1").expect("register");
        assert!(registry.cancel("u1"));
        assert_eq!(
            registry
                .cancel_and_wait("u1", Duration::from_millis(60))
                .await,
            CancelWaitResult::TimedOut
        );
    }

    #[tokio::test]
    async fn request_reclaim_wakes_runner_side_waiter() {
        let registry = PecoActiveRuns::new();
        let reg = registry.try_register("u1").expect("register");
        let notify = Arc::clone(&reg.reclaim_notify);

        let waiter = tokio::spawn(async move {
            notify.notified().await;
        });
        // 让 waiter 先进入等待，再触发唤醒
        tokio::time::sleep(Duration::from_millis(20)).await;
        registry.request_reclaim("u1");
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter 被 request_reclaim 唤醒")
            .unwrap();

        // 无 run 时 request_reclaim 是 no-op
        registry.request_reclaim("ghost");
        drop(reg);
    }
}
