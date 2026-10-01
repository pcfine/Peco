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
use std::sync::atomic::{AtomicBool, Ordering};
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

/// `enqueue_query` 的结果。
///
/// 区分「无 run」与「控制通道满」是必要的：前者要求调用方改走新建路径，
/// 后者只是瞬时背压，误判会让调用方白建一个 run。
#[derive(Debug, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// 已投递到 runner 的控制通道。
    Enqueued,
    /// 该用户无 run，或 runner 正在退出（通道已关闭）—— 消息原样退回，未半投递。
    NoRun,
    /// 控制通道已满（待处理消息积压）。
    Backpressure,
}

/// 注册表条目：runner 之外的所有交互都经由这份句柄。
struct ActiveEntry {
    control_tx: mpsc::Sender<ControlCommand>,
    event_tx: broadcast::Sender<LooperEvent>,
    /// 桥接任务退出时 notify，runner 醒来复查是否可回收。
    reclaim_notify: Arc<Notify>,
    /// 是否有轮次在途（区别于「run 已注册」）。
    ///
    /// 由 runner 在 `OuterStateChange` 时更新，读侧无需取锁。
    turn_in_flight: Arc<AtomicBool>,
    /// 在途轮的用户输入文本。
    ///
    /// 由 runner 在 `TurnStart` 时上报、`OuterStateChange → Idle` 时清空 ——
    /// 与 `turn_in_flight` 同寿命。快照端点据此让「整页刷新」后仍能显示刚发出、
    /// 尚未落盘的那句 query（快照只含 committed_turns，不含在途轮）。
    inflight_input: Option<String>,
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
    ///
    /// `turn_in_flight` 初始置 true：抢注只发生在携带消息的新建路径上，
    /// 「已注册」即意味着即将开跑 —— 覆盖构建窗口内附着方的判定。
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
                turn_in_flight: Arc::new(AtomicBool::new(true)),
                inflight_input: None,
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
    /// 退出码见 [`EnqueueOutcome`]：`NoRun` 与否决式失败都要求调用方改走
    /// 新建路径，`Backpressure` 则是瞬时积压。
    pub fn enqueue_query(&self, user_id: &str, text: String) -> EnqueueOutcome {
        let map = self.inner.lock().unwrap();
        let Some(entry) = map.get(user_id) else {
            return EnqueueOutcome::NoRun;
        };
        match entry.control_tx.try_send(ControlCommand::Query(text)) {
            Ok(()) => EnqueueOutcome::Enqueued,
            // Full 把消息原样退回，Closed 说明 runner 正在退出 —— 都不算投递成功。
            Err(mpsc::error::TrySendError::Full(_)) => EnqueueOutcome::Backpressure,
            Err(mpsc::error::TrySendError::Closed(_)) => EnqueueOutcome::NoRun,
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
    ///
    /// 注意这是「已注册」而非「有轮次在跑」：停靠在 Idle 等下一句输入的
    /// run 同样为 true。判断是否需要附着/补占位请用 [`Self::turn_in_flight`]。
    pub fn is_running(&self, user_id: &str) -> bool {
        self.inner.lock().unwrap().contains_key(user_id)
    }

    /// 该用户是否有轮次在途（runner 上报，无 run 时为 false）。
    pub fn turn_in_flight(&self, user_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(user_id)
            .is_some_and(|entry| entry.turn_in_flight.load(Ordering::Relaxed))
    }

    /// 由 runner 在 `OuterStateChange` 时上报轮次在途状态；无 run 时为 no-op。
    pub fn set_turn_in_flight(&self, user_id: &str, in_flight: bool) {
        let map = self.inner.lock().unwrap();
        if let Some(entry) = map.get(user_id) {
            entry.turn_in_flight.store(in_flight, Ordering::Relaxed);
        }
    }

    /// 在途轮的用户输入文本（无 run / 无在途轮时为 `None`）。
    ///
    /// 供快照端点在整页刷新（前端内存全清）后恢复「刚发出、尚未落盘」的 query。
    pub fn inflight_input(&self, user_id: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .get(user_id)
            .and_then(|entry| entry.inflight_input.clone())
    }

    /// 由 runner 在 `TurnStart` 上报在途轮的用户输入；轮次收尾（Idle）时传
    /// `None` 清空。无 run 时为 no-op。
    pub fn set_inflight_input(&self, user_id: &str, input: Option<String>) {
        let mut map = self.inner.lock().unwrap();
        if let Some(entry) = map.get_mut(user_id) {
            entry.inflight_input = input;
        }
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

        assert_eq!(
            registry.enqueue_query("u2", "hi".into()),
            EnqueueOutcome::NoRun,
            "无 run 入队失败"
        );
        assert_eq!(
            registry.enqueue_query("u1", "hi".into()),
            EnqueueOutcome::Enqueued
        );
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
    async fn enqueue_reports_backpressure_not_missing_run() {
        let registry = PecoActiveRuns::new();
        let _reg = registry.try_register("u1").expect("register");

        // 不排空 control_rx：灌满容量为 32 的通道，第 33 条必须是背压而非「无 run」
        for _ in 0..32 {
            assert_eq!(
                registry.enqueue_query("u1", "x".into()),
                EnqueueOutcome::Enqueued
            );
        }
        assert_eq!(
            registry.enqueue_query("u1", "x".into()),
            EnqueueOutcome::Backpressure,
            "满队列不能误报成无 run"
        );
    }

    #[test]
    fn turn_in_flight_tracks_registration_and_runner_reports() {
        let registry = PecoActiveRuns::new();
        assert!(!registry.turn_in_flight("u1"), "无 run 时无在途轮次");

        let reg = registry.try_register("u1").expect("register");
        assert!(registry.turn_in_flight("u1"), "抢注即为即将开跑");

        registry.set_turn_in_flight("u1", false);
        assert!(!registry.turn_in_flight("u1"), "runner 上报轮次结束");

        registry.set_turn_in_flight("ghost", true); // 无 run：no-op 不 panic
        assert!(!registry.turn_in_flight("ghost"));

        drop(reg);
        assert!(!registry.turn_in_flight("u1"), "条目移除后回落为 false");
    }

    #[test]
    fn inflight_input_tracks_runner_reports() {
        let registry = PecoActiveRuns::new();
        assert_eq!(registry.inflight_input("u1"), None, "无 run 时为 None");

        let reg = registry.try_register("u1").expect("register");
        assert_eq!(registry.inflight_input("u1"), None, "抢注时尚未上报输入");

        registry.set_inflight_input("u1", Some("我刚发的问题".into()));
        assert_eq!(
            registry.inflight_input("u1").as_deref(),
            Some("我刚发的问题")
        );

        // 轮次收尾（Idle）清空 —— 与 turn_in_flight 同寿命
        registry.set_inflight_input("u1", None);
        assert_eq!(registry.inflight_input("u1"), None);

        registry.set_inflight_input("ghost", Some("x".into())); // 无 run：no-op 不 panic
        assert_eq!(registry.inflight_input("ghost"), None);

        drop(reg);
        assert_eq!(registry.inflight_input("u1"), None, "条目移除后回落 None");
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
