//! 唤醒泵：按固定顺序把台账里待唤醒的行投出去，并回收三条卡住的路径。
//!
//! 顺序不可换：回收先于取批，同一轮里被捞回来的行就能立刻参与投递。认领之后
//! 不许早退 —— 行一旦进了 `delivering`，提前返回只能等 60 秒超时或重启才有人
//! 把它捞回来。

use std::sync::{Arc, Mutex, OnceLock};

use sqlx::SqlitePool;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::db::dispatches;
use crate::shutdown::Shutdown;
use crate::space::peco::PecoIdentity;

use super::compose::compose;
use super::deliver::{Delivery, DeliverySink, WakeDelivery, WakeTarget};
use super::{
    DELIVERING_TIMEOUT, MAX_ATTEMPTS, MAX_BATCH, PUMP_RESTART_BACKOFF, PUMP_RESTART_BACKOFF_MAX,
    RETRY_BACKOFF, TICK,
};

/// 把「台账里等着投」的行变成「派发方会话里跑起来的一轮」。
///
/// 只在终态事件与定时器上醒来，不扫全表：台账里待唤醒的行数上界是「未通知的
/// 终态任务数」，与累计任务数无关。
pub struct WakePump {
    pool: SqlitePool,
    /// 只用于启动日志（说清这个泵由谁拉起）；投递落点由台账逐行决定。
    peco: Arc<PecoIdentity>,
    notify: Notify,
    /// 晚绑定：投递端要在 `AppState` 建好之后才拿得到 `SpaceRegistry`，而泵必须
    /// 在那之前就交给观察者。反过来持有会成 Arc 环。
    delivery: OnceLock<Arc<dyn DeliverySink>>,
    shutdown: Shutdown,
    /// `Mutex` 是因为 `spawn` 拿 `&Arc<Self>`，而 `stop` 要移出句柄。
    task: Mutex<Option<JoinHandle<()>>>,
}

impl WakePump {
    pub fn new(pool: SqlitePool, peco: Arc<PecoIdentity>, shutdown: Shutdown) -> Self {
        Self {
            pool,
            peco,
            notify: Notify::new(),
            delivery: OnceLock::new(),
            shutdown,
            task: Mutex::new(None),
        }
    }

    /// 注入投递端。重复注入只 warn：先到的那一份才是真正在用的。
    pub fn attach_delivery(&self, delivery: Arc<WakeDelivery>) {
        let sink: Arc<dyn DeliverySink> = delivery;
        if self.delivery.set(sink).is_err() {
            warn!("wake pump already has a delivery sink; keeping the first one");
        }
    }

    /// 叫醒泵。可以多叫：多份通知只换来一次 tick，待投递的行由台账决定。
    pub fn notify_one(&self) {
        self.notify.notify_one();
    }

    /// 启动回收：上个进程停在 `delivering` 的行没有人再会把它落地，全部退回待投递。
    ///
    /// 必须在 `reconcile_ledger` 之后调用 —— 对账会把重启前的在飞行 orphan 化并
    /// 保留唤醒态，更早被认领、随进程一起停下的行则要靠这一步回到队列；顺序反了，
    /// 首个 tick 就看不到它们。
    pub async fn startup_reclaim(&self) {
        match dispatches::requeue_all_delivering(&self.pool).await {
            Ok(0) => {}
            Ok(count) => info!(
                count,
                "reclaimed deliveries left over by the previous process"
            ),
            Err(e) => {
                warn!(error = %e, "startup reclaim failed; stale claims wait for the timeout")
            }
        }
    }

    /// 起后台循环；调用即返回。首个 tick 由一次通知或 [`TICK`] 触发。
    ///
    /// 外面再套一层监督：`run` 是本模块唯一的长驻循环，它一旦返回或 panic，唤醒
    /// 就静默停摆 —— 没有别的部件会发现「台账里待投的行再也没人取」。故退出就
    /// warn 并重启，带退避，免得一个每次启动就 panic 的循环被无间隔地拉起。
    pub fn spawn(self: &Arc<Self>) {
        let pump = Arc::clone(self);
        let task = tokio::spawn(async move {
            let mut backoff = PUMP_RESTART_BACKOFF;
            loop {
                let outcome = tokio::select! {
                    _ = pump.shutdown.cancelled() => return,
                    outcome = tokio::spawn(Arc::clone(&pump).run()) => outcome,
                };
                // 停机时 `run` 正常返回不是故障：不判一下 journal 里会留一句「正在重启它」。
                if pump.shutdown.is_cancelled() {
                    return;
                }
                match outcome {
                    Ok(()) => warn!("wake pump loop returned; restarting it"),
                    Err(e) => warn!(error = %e, "wake pump loop died; restarting it"),
                }
                tokio::select! {
                    _ = pump.shutdown.cancelled() => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(PUMP_RESTART_BACKOFF_MAX);
            }
        });
        *self.task.lock().unwrap() = Some(task);
    }

    /// 停机：等监督任务退出，泵因此确定不会再投递（关库要在它之后）。
    pub async fn stop(&self) {
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    async fn run(self: Arc<Self>) {
        info!(
            ws_id = %self.peco.ws_id,
            tick_secs = TICK.as_secs(),
            "wake pump started"
        );
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = self.notify.notified() => {}
                _ = tokio::time::sleep(TICK) => {}
            }
            self.tick().await;
        }
    }

    /// 一轮：三条回收 → 取批 → 忙则让路 → 认领 → 合成 → 投递 → 落台账。
    pub async fn tick(&self) {
        self.reclaim().await;

        let rows = match dispatches::list_pending(&self.pool, MAX_BATCH, RETRY_BACKOFF).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "wake pump cannot list pending rows");
                return;
            }
        };
        if rows.is_empty() {
            return;
        }

        let Some(delivery) = self.delivery.get() else {
            // 装配顺序保证 `attach_delivery` 先于 `spawn`；没接上时一行都不认领，
            // 行留在 `pending` 等下一条通知。
            return;
        };

        // 一批里的行可能来自不同会话：按目标分组后逐组认领与投递。忙的组整组
        // 让路（没认领就不算一次尝试），不挡住同批里其它会话。
        for (target, rows) in group_by_target(rows) {
            if delivery.is_busy(&target).await {
                // 用户正在看这个会话：本轮让路，下一轮再来。
                continue;
            }

            let mut claimed = Vec::with_capacity(rows.len());
            for row in rows {
                match dispatches::claim(&self.pool, &row.id, RETRY_BACKOFF).await {
                    // 1 = 这一行由本次投递拿下；0 = 别的投递者刚拿走它，或它还在退避里。
                    Ok(1) => claimed.push(row),
                    Ok(_) => {}
                    Err(e) => {
                        warn!(task_id = %row.task_id, error = %e, "wake pump cannot claim the row")
                    }
                }
            }
            if claimed.is_empty() {
                continue;
            }

            self.deliver_group(delivery.as_ref(), &target, &claimed)
                .await;
        }
    }

    /// 把一组已认领的行投出去，并落台账结论。
    ///
    /// 从这里到 `mark_*` 之间不得早退：行已经进了 `delivering`，提前返回会把它
    /// 钉在那里等 60 秒超时。
    async fn deliver_group(
        &self,
        delivery: &dyn DeliverySink,
        target: &WakeTarget,
        claimed: &[dispatches::DispatchRow],
    ) {
        match delivery.deliver(target, compose(claimed)).await {
            Delivery::Delivered => {
                for row in claimed {
                    if let Err(e) = dispatches::mark_delivered(&self.pool, &row.id).await {
                        warn!(task_id = %row.task_id, error = %e, "cannot mark the row delivered");
                    }
                }
            }
            // 忙与短暂不可用都是「这次不算数」：回滚认领，尝试次数一并退回。
            Delivery::Busy | Delivery::Deferred => {
                for row in claimed {
                    if let Err(e) = dispatches::release_claim(&self.pool, &row.id).await {
                        warn!(task_id = %row.task_id, error = %e, "cannot roll the claim back");
                    }
                }
            }
            Delivery::Skipped => {
                warn!(
                    count = claimed.len(),
                    ws_id = %target.ws_id,
                    "wake target space is gone; rows skipped for good"
                );
                for row in claimed {
                    if let Err(e) = dispatches::mark_skipped(
                        &self.pool,
                        &row.id,
                        dispatches::WakeSkipReason::SpaceClosed,
                    )
                    .await
                    {
                        warn!(task_id = %row.task_id, error = %e, "cannot skip the row");
                    }
                }
            }
            Delivery::Failed(reason) => {
                warn!(
                    count = claimed.len(),
                    ws_id = %target.ws_id,
                    reason,
                    "wake delivery failed"
                );
                for row in claimed {
                    if let Err(e) = dispatches::mark_failed(&self.pool, &row.id).await {
                        warn!(task_id = %row.task_id, error = %e, "cannot mark the row failed");
                    }
                }
            }
        }
    }

    /// 三条回收：60s 超时、尝试用尽、失败重排。
    ///
    /// 各自的失败只 warn：一条回收写不动不该拦住本轮投递，下一轮还会再来。
    async fn reclaim(&self) {
        if let Err(e) = dispatches::requeue_stale_delivering(&self.pool, DELIVERING_TIMEOUT).await {
            warn!(error = %e, "wake pump cannot reclaim stale claims");
        }
        match dispatches::skip_exhausted(&self.pool, MAX_ATTEMPTS).await {
            Ok(0) => {}
            // 一条唤醒被彻底放弃是「Peco 再也不会知道这条任务」：没有日志就只能
            // 靠用户自己发现少了一轮复核。
            Ok(count) => warn!(
                count,
                max_attempts = MAX_ATTEMPTS,
                "wake attempts exhausted; rows are skipped for good"
            ),
            Err(e) => warn!(error = %e, "wake pump cannot skip exhausted rows"),
        }
        if let Err(e) = dispatches::reschedule_failed(&self.pool, MAX_ATTEMPTS).await {
            warn!(error = %e, "wake pump cannot reschedule failed rows");
        }
    }
}

/// 按投递目标分组，保持首次出现顺序：同一 tick 取到的同目标行才合成一条消息。
fn group_by_target(
    rows: Vec<dispatches::DispatchRow>,
) -> Vec<(WakeTarget, Vec<dispatches::DispatchRow>)> {
    let mut groups: Vec<(WakeTarget, Vec<dispatches::DispatchRow>)> = Vec::new();
    for row in rows {
        let target = WakeTarget {
            ws_id: row.origin_ws_id.clone(),
            conversation_id: row.origin_conversation_id.clone(),
        };
        match groups.iter_mut().find(|(existing, _)| *existing == target) {
            Some((_, items)) => items.push(row),
            None => groups.push((target, vec![row])),
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::db::dispatch_state::WakeState;
    use crate::db::dispatches::NewDispatch;

    /// 只回答「忙不忙」与「投出去会怎样」的假投递端：把泵的分支决策从真实运行里
    /// 单独摘出来，认领与回滚的落库结果才可穷举。
    struct FakeSink {
        busy: bool,
        busy_conversation: Option<String>,
        outcome: FakeOutcome,
        delivered: AtomicUsize,
        texts: Mutex<Vec<String>>,
    }

    #[derive(Clone, Copy)]
    enum FakeOutcome {
        Delivered,
        Busy,
        Failed,
    }

    impl FakeSink {
        fn new(busy: bool, outcome: FakeOutcome) -> Arc<Self> {
            Arc::new(Self {
                busy,
                busy_conversation: None,
                outcome,
                delivered: AtomicUsize::new(0),
                texts: Mutex::new(Vec::new()),
            })
        }

        fn busy_conversation(conversation_id: &str) -> Arc<Self> {
            Arc::new(Self {
                busy: false,
                busy_conversation: Some(conversation_id.to_string()),
                outcome: FakeOutcome::Delivered,
                delivered: AtomicUsize::new(0),
                texts: Mutex::new(Vec::new()),
            })
        }

        fn delivered_count(&self) -> usize {
            self.delivered.load(Ordering::SeqCst)
        }

        fn texts(&self) -> Vec<String> {
            self.texts.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl DeliverySink for FakeSink {
        async fn is_busy(&self, target: &WakeTarget) -> bool {
            self.busy
                || self
                    .busy_conversation
                    .as_deref()
                    .is_some_and(|id| target.conversation_id.as_deref() == Some(id))
        }

        async fn deliver(&self, _target: &WakeTarget, text: String) -> Delivery {
            self.delivered.fetch_add(1, Ordering::SeqCst);
            self.texts.lock().unwrap().push(text);
            match self.outcome {
                FakeOutcome::Delivered => Delivery::Delivered,
                FakeOutcome::Busy => Delivery::Busy,
                FakeOutcome::Failed => Delivery::Failed("scripted failure".to_string()),
            }
        }
    }

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    fn pump_for(pool: &SqlitePool) -> Arc<WakePump> {
        Arc::new(WakePump::new(
            pool.clone(),
            Arc::new(PecoIdentity {
                ws_id: "ws_peco".to_string(),
                work_dir: PathBuf::from("/tmp/peco/work"),
                team_id: "peco".to_string(),
                from_marker: true,
            }),
            Shutdown::new(),
        ))
    }

    async fn seed_pending(pool: &SqlitePool, task_id: &str) {
        seed_pending_for(pool, task_id, "ws_peco", None).await;
    }

    async fn seed_pending_for(
        pool: &SqlitePool,
        task_id: &str,
        origin_ws_id: &str,
        origin_conversation_id: Option<&str>,
    ) {
        dispatches::insert(
            pool,
            &NewDispatch {
                id: &format!("d_{task_id}"),
                task_id,
                target_ws_id: "ws_peco",
                target_work_dir: "/tmp/work",
                target_team_id: "peco",
                origin_ws_id,
                origin_conversation_id,
                agent_name: "@master",
                origin_kind: "run_machine_task",
                task_state: "completed",
                wake_state: WakeState::Pending,
                wake_skip_reason: None,
                prompt_digest: None,
                dispatch_id: None,
            },
        )
        .await
        .unwrap();
    }

    async fn row_of(pool: &SqlitePool, task_id: &str) -> (String, i64, Option<String>) {
        sqlx::query_as::<_, (String, i64, Option<String>)>(
            "SELECT wake_state, wake_attempts, wake_claimed_at FROM d_dispatches WHERE task_id = ?",
        )
        .bind(task_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// 忙时连认领都不该发生。真实链路里「没认领」与「认领后回滚」在台账上长得
    /// 一模一样（都是 `pending` + `attempts` 不变），故判据落在「投递有没有被调用」。
    #[tokio::test]
    async fn a_busy_session_is_never_claimed() {
        let (pool, _dir) = test_pool().await;
        seed_pending(&pool, "t-busy").await;
        let pump = pump_for(&pool);
        let sink = FakeSink::new(true, FakeOutcome::Delivered);
        assert!(pump.delivery.set(sink.clone()).is_ok());

        pump.tick().await;

        assert_eq!(sink.delivered_count(), 0, "忙时不得投递");
        assert_eq!(
            row_of(&pool, "t-busy").await,
            ("pending".to_string(), 0, None)
        );
    }

    /// 认领之后竞态输给用户：行回滚成待投递，且这次尝试退回去 —— 否则「用户正好
    /// 在打开页面」会被算成一次失败尝试。
    #[tokio::test]
    async fn losing_the_registration_race_rolls_the_claim_back() {
        let (pool, _dir) = test_pool().await;
        seed_pending(&pool, "t-race").await;
        let pump = pump_for(&pool);
        let sink = FakeSink::new(false, FakeOutcome::Busy);
        assert!(pump.delivery.set(sink.clone()).is_ok());

        pump.tick().await;

        assert_eq!(sink.delivered_count(), 1, "认领之后确实尝试过投递");
        assert_eq!(
            row_of(&pool, "t-race").await,
            ("pending".to_string(), 0, None),
            "回滚到待投递、尝试退回 0、认领时间清空"
        );
        assert_eq!(
            dispatches::claim(&pool, "d_t-race", RETRY_BACKOFF)
                .await
                .unwrap(),
            1,
            "回滚后的行必须能被再次认领"
        );
    }

    #[tokio::test]
    async fn a_delivered_row_records_when_it_was_sent() {
        let (pool, _dir) = test_pool().await;
        seed_pending(&pool, "t-ok").await;
        let pump = pump_for(&pool);
        let sink = FakeSink::new(false, FakeOutcome::Delivered);
        assert!(pump.delivery.set(sink.clone()).is_ok());

        pump.tick().await;

        let (state, attempts, _) = row_of(&pool, "t-ok").await;
        assert_eq!(state, "delivered");
        assert_eq!(attempts, 1);
        let delivered_at: Option<String> =
            sqlx::query_scalar("SELECT delivered_at FROM d_dispatches WHERE task_id = ?")
                .bind("t-ok")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(delivered_at.is_some(), "送达必须留时间戳");
        let texts = sink.texts();
        assert_eq!(texts.len(), 1);
        assert!(texts[0].starts_with("[peco-wake]"), "{}", texts[0]);
    }

    #[tokio::test]
    async fn a_failed_delivery_stays_counted() {
        let (pool, _dir) = test_pool().await;
        seed_pending(&pool, "t-bad").await;
        let pump = pump_for(&pool);
        let sink = FakeSink::new(false, FakeOutcome::Failed);
        assert!(pump.delivery.set(sink).is_ok());

        pump.tick().await;

        let (state, attempts, _) = row_of(&pool, "t-bad").await;
        assert_eq!(state, "failed");
        assert_eq!(attempts, 1, "失败要计数，下一轮才有重排的依据");
    }

    /// 退避是「预算别被快速重试烧光」的唯一依据：去掉谓词后，`MAX_ATTEMPTS + 1`
    /// 个 tick 就能把一条行推到 `skipped`。
    ///
    /// 判据落在尝试次数上而不是时长上 —— 失败后的 tick 全部落在退避窗口里，真人
    /// 时钟的 5 秒退避足够挡住测试里毫秒级的连打。
    #[tokio::test]
    async fn the_backoff_keeps_a_fast_tick_loop_from_burning_the_budget() {
        let (pool, _dir) = test_pool().await;
        seed_pending(&pool, "t-backoff").await;
        let pump = pump_for(&pool);
        assert!(
            pump.delivery
                .set(FakeSink::new(false, FakeOutcome::Failed))
                .is_ok()
        );

        for _ in 0..=(MAX_ATTEMPTS + 1) {
            pump.tick().await;
        }

        let (state, attempts, claimed_at) = row_of(&pool, "t-backoff").await;
        assert_eq!(attempts, 1, "退避窗口里的 tick 不得再认领");
        assert_eq!(state, "pending", "还有预算，行必须留在队列里");
        assert!(claimed_at.is_some(), "认领时间要留着算退避");
    }

    #[tokio::test]
    async fn a_busy_target_does_not_block_another_target() {
        let (pool, _dir) = test_pool().await;
        seed_pending_for(&pool, "t-busy", "ws_peco", Some("c_busy")).await;
        seed_pending_for(&pool, "t-idle", "ws_peco", Some("c_idle")).await;
        let pump = pump_for(&pool);
        let sink = FakeSink::busy_conversation("c_busy");
        assert!(pump.delivery.set(sink.clone()).is_ok());

        pump.tick().await;

        assert_eq!(sink.delivered_count(), 1, "只有闲目标该被投递");
        assert!(
            sink.texts()[0].contains("t-idle"),
            "投出去的必须是闲目标的行：{}",
            sink.texts()[0]
        );
        assert_eq!(
            row_of(&pool, "t-busy").await,
            ("pending".to_string(), 0, None),
            "忙目标保持待投递、不计尝试"
        );
        assert_eq!(row_of(&pool, "t-idle").await.0, "delivered");
    }

    /// 一轮只取一批：`MAX_BATCH` 之外的行留在队列里等下一轮。
    #[tokio::test]
    async fn one_tick_takes_a_single_batch() {
        let (pool, _dir) = test_pool().await;
        for seq in 0..(MAX_BATCH + 2) {
            seed_pending(&pool, &format!("t-{seq}")).await;
        }
        let pump = pump_for(&pool);
        assert!(
            pump.delivery
                .set(FakeSink::new(false, FakeOutcome::Delivered))
                .is_ok()
        );

        pump.tick().await;

        let pending: i64 =
            sqlx::query_scalar("SELECT count(*) FROM d_dispatches WHERE wake_state = 'pending'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(pending, 2, "超出一批的行不得被顺手捎走");
    }
}
