//! 调度循环：每 [`TICK`] 一轮，固定按「到期 → 重试 → 通知」三段处理。
//!
//! 三段顺序不可换：先推进到期槽位（新的 run 才有机会在这一轮里赶上重试的名额），
//! 再补试排队的 run（它们可能刚被释放），最后才发通知（前两段刚产生的终态也在
//! 这一轮就能通知出去）。
//!
//! 与 `wake::pump` 同一套监督手法：本模块只有一个长驻循环，它退出或 panic 之后
//! 没有别的部件会发现「再也没有人看排期」，所以外层套一层带退避的重启。

use std::sync::{Arc, Mutex, Weak};

use chrono::{Local, Utc};
use sqlx::SqlitePool;
use tokio::task::JoinHandle;
use tracing::{info, warn};
use uuid::Uuid;

use crate::chat::active::ChatActiveRuns;
use crate::db::schedules::{self, NewRun, ScheduledRunRow, ScheduledTaskRow};
use crate::shutdown::Shutdown;
use crate::space::SpaceEnv;
use crate::space::peco::PecoIdentity;
use crate::space::registry::SpaceRegistry;
use crate::wake::deliver::{Delivery, DeliverySink, WakeDelivery, WakeTarget};

use super::fire::{self, FireDeps};
use super::{
    MAX_BATCH, NOTIFY_MAX_ATTEMPTS, RESTART_BACKOFF, RESTART_BACKOFF_MAX, RunStatus, SKIP_MISSED,
    SKIP_PREVIOUS_RUNNING, SKIP_SCHEDULE_DISABLED, TICK, TriggerKind, cron, is_missed, notify,
    notify_retry_at, parse_utc_text, utc_text,
};

/// 手动触发一次的结果。
pub struct ManualRun {
    pub run_id: String,
    /// 台账里的真实状态：`running`（已经跑起来）或 `pending`（会话忙，排队重试）。
    /// 硬失败（空间打不开、agent 缺失）会是 `failed` —— 那是台账的结论，不在这里改写，
    /// 调用方从执行历史里读成因。
    pub status: String,
}

/// 手动触发的失败面。
#[derive(Debug)]
pub enum RunNowError {
    /// 调度不存在。
    NotFound,
    /// 这一条已有未完成的 run：再排一次只会互相踩着，不如让用户等它结束。
    Busy,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for RunNowError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

/// 定时任务的调度器：排期在库里，循环只做「现在该动谁」。
pub struct Scheduler {
    pool: SqlitePool,
    spaces: Arc<SpaceRegistry>,
    chat_runs: Arc<ChatActiveRuns>,
    /// 只用于日志口径与通知落点说明：通知经 `delivery` 投进 Peco 空间的 `@master`。
    peco: Arc<PecoIdentity>,
    /// 只持弱引用：与 `WakeDelivery` 同一手法，`scheduler → env → hub → 实例 → 工具`
    /// 若成强引用，关掉的空间会随调度器一起钉在内存里。
    env: Weak<SpaceEnv>,
    delivery: Arc<WakeDelivery>,
    shutdown: Shutdown,
    /// `Mutex` 是因为 `spawn` 拿 `&Arc<Self>`，而 `stop` 要移出句柄。
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Scheduler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: SqlitePool,
        spaces: Arc<SpaceRegistry>,
        chat_runs: Arc<ChatActiveRuns>,
        peco: Arc<PecoIdentity>,
        env: Weak<SpaceEnv>,
        delivery: Arc<WakeDelivery>,
        shutdown: Shutdown,
    ) -> Self {
        Self {
            pool,
            spaces,
            chat_runs,
            peco,
            env,
            delivery,
            shutdown,
            task: Mutex::new(None),
        }
    }

    /// 起后台循环；调用即返回。
    pub fn spawn(self: &Arc<Self>) {
        let scheduler = Arc::clone(self);
        let task = tokio::spawn(async move {
            let mut backoff = RESTART_BACKOFF;
            loop {
                let outcome = tokio::select! {
                    _ = scheduler.shutdown.cancelled() => return,
                    outcome = tokio::spawn(Arc::clone(&scheduler).run()) => outcome,
                };
                // 停机时 `run` 正常返回不是故障：不判一下 journal 里会留一句「正在重启它」。
                if scheduler.shutdown.is_cancelled() {
                    return;
                }
                match outcome {
                    Ok(()) => warn!("schedule loop returned; restarting it"),
                    Err(e) => warn!(error = %e, "schedule loop died; restarting it"),
                }
                tokio::select! {
                    _ = scheduler.shutdown.cancelled() => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(RESTART_BACKOFF_MAX);
            }
        });
        *self.task.lock().unwrap() = Some(task);
    }

    /// 停机：等监督任务退出，排期因此确定不会再触发（关库要在它之后）。
    pub async fn stop(&self) {
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    /// 循环体：**先 tick 再睡**。
    ///
    /// 与唤醒泵（先睡再 tick）相反：重启后第一个 tick 就该处理停机期间到期的槽位，
    /// 让它们落在 [`super::GRACE`] 里被补跑，而不是白等一个 tick。
    async fn run(self: Arc<Self>) {
        info!(
            ws_id = %self.peco.ws_id,
            tick_secs = TICK.as_secs(),
            "schedule scheduler started"
        );
        loop {
            self.tick().await;
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = tokio::time::sleep(TICK) => {}
            }
        }
    }

    /// 一轮：到期 → 重试 → 通知。三段各自的失败只 warn，不拦住后面两段。
    pub async fn tick(&self) {
        if let Err(e) = self.fire_due().await {
            warn!(error = %e, "cannot process the due schedules");
        }
        if let Err(e) = self.retry_pending().await {
            warn!(error = %e, "cannot retry the pending schedule runs");
        }
        if let Err(e) = self.deliver_notifications().await {
            warn!(error = %e, "cannot deliver the schedule notifications");
        }
    }

    /// 手动触发一次：排一行 `manual` run 并立刻跑。
    pub async fn run_now(&self, schedule_id: &str) -> Result<ManualRun, RunNowError> {
        let task = schedules::task_by_id(&self.pool, schedule_id)
            .await?
            .ok_or(RunNowError::NotFound)?;
        if schedules::unfinished_run(&self.pool, &task.id)
            .await?
            .is_some()
        {
            return Err(RunNowError::Busy);
        }
        let slot = utc_text(Utc::now());
        let run = self
            .insert_run(&task, TriggerKind::Manual, &slot, RunStatus::Pending)
            .await?
            .ok_or(RunNowError::Busy)?;
        self.fire(&task, &run).await?;
        let status = schedules::run_by_id(&self.pool, &run.id)
            .await?
            .map(|row| row.status)
            .unwrap_or_default();
        Ok(ManualRun {
            run_id: run.id,
            status,
        })
    }

    /// 删除调度之前先掐掉在途轮。
    ///
    /// run 行随调度行的外键级联消失，那一轮就跑在一个没人认领的会话里了 ——
    /// 用户看到的是「我删了任务，它还在那个空间里说话」。
    pub async fn cancel_inflight(&self, schedule_id: &str) -> Result<(), sqlx::Error> {
        let Some(run) = schedules::unfinished_run(&self.pool, schedule_id).await? else {
            return Ok(());
        };
        if let Some(conversation_id) = run.conversation_id.as_deref()
            && self.chat_runs.cancel(conversation_id)
        {
            info!(
                schedule_id,
                conversation_id, "cancelled the in-flight run before deleting the schedule"
            );
        }
        Ok(())
    }

    /// 到期任务：CAS 推进槽位 → 判错过 → 判前一轮未完成 → 触发。
    async fn fire_due(&self) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        for task in schedules::due_tasks(&self.pool, &utc_text(now), MAX_BATCH).await? {
            let Some(slot) = task.next_run_at.clone() else {
                continue;
            };
            // 排期算不出来只可能是库里的表达式坏了（写入口已校验）。不推进、也不
            // 触发：这条路每 tick 都会重来一次，日志是唯一的信号。
            let Ok(next) = cron::next_occurrence(&task.cron_expr, Local::now()) else {
                warn!(
                    schedule_id = %task.id,
                    cron_expr = %task.cron_expr,
                    "the stored cron expression no longer parses; the slot is left untouched"
                );
                continue;
            };
            let next_text = utc_text(next.with_timezone(&Utc));
            // 1 = 这个槽位由本次 tick 拿下；0 = 别人已经推进过（重启交叠、
            // 手改过 next_run_at），这一行不该再被触发一次。
            if schedules::advance_next_run(&self.pool, &task.id, &slot, &next_text).await? == 0 {
                continue;
            }

            let scheduled_for = parse_utc_text(&slot).unwrap_or(now);
            if is_missed(scheduled_for, now) {
                self.record_skipped(&task, &slot, SKIP_MISSED).await?;
                continue;
            }
            if schedules::unfinished_run(&self.pool, &task.id)
                .await?
                .is_some()
            {
                self.record_skipped(&task, &slot, SKIP_PREVIOUS_RUNNING)
                    .await?;
                continue;
            }
            let Some(run) = self
                .insert_run(&task, TriggerKind::Cron, &slot, RunStatus::Pending)
                .await?
            else {
                continue;
            };
            self.fire(&task, &run).await?;
        }
        Ok(())
    }

    /// 会话忙时排下的队：到点再试一次，不新建行（唯一槽位索引保证不会重复触发）。
    async fn retry_pending(&self) -> Result<(), sqlx::Error> {
        let rows = schedules::due_retries(&self.pool, &utc_text(Utc::now()), MAX_BATCH).await?;
        for run in rows {
            // 调度行没了就是被删了，run 行会随外键一起消失；这里只兜一个删与读的竞态。
            let Some(task) = schedules::task_by_id(&self.pool, &run.schedule_id).await? else {
                continue;
            };
            // 排队期间用户可能已经把任务停用：重试队列不看 `enabled`，这里不拦就
            // 会照原计划打开空间、往会话里投一条、再跑一轮。
            if !task.enabled {
                schedules::mark_skipped(&self.pool, &run.id, SKIP_SCHEDULE_DISABLED).await?;
                schedules::record_task_last_run(&self.pool, &task.id, RunStatus::Skipped).await?;
                continue;
            }
            self.fire(&task, &run).await?;
        }
        Ok(())
    }

    /// 待发通知：投进 Peco 空间的 `@master` 会话。
    async fn deliver_notifications(&self) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        let rows = schedules::due_notifications(
            &self.pool,
            &utc_text(now),
            NOTIFY_MAX_ATTEMPTS,
            MAX_BATCH,
        )
        .await?;
        for run in rows {
            let Some(task) = schedules::task_by_id(&self.pool, &run.schedule_id).await? else {
                continue;
            };
            let text = notify::compose_notification(&task, &run);
            // 定时任务的通知固定落在 Peco 空间的空间会话：它的语义是平台通知，
            // 与派发台账的来源路由无关。
            let target = WakeTarget::space(self.peco.ws_id.clone());
            match self.delivery.deliver(&target, text).await {
                Delivery::Delivered => {
                    schedules::mark_notified(&self.pool, &run.id).await?;
                }
                // 用户正在 Peco 那边对话，或空间刚好在开关之间：不是投不出去，等一会儿。
                Delivery::Busy | Delivery::Deferred => {
                    if run.notify_attempts + 1 >= NOTIFY_MAX_ATTEMPTS {
                        schedules::mark_notify_gave_up(
                            &self.pool,
                            &run.id,
                            super::NotifyState::Skipped,
                        )
                        .await?;
                    } else {
                        schedules::mark_notify_retry(
                            &self.pool,
                            &run.id,
                            &utc_text(notify_retry_at(now)),
                        )
                        .await?;
                    }
                }
                Delivery::Skipped => {
                    warn!(run_id = %run.id, "schedule notification target is gone");
                    schedules::mark_notify_gave_up(&self.pool, &run.id, super::NotifyState::Failed)
                        .await?;
                }
                Delivery::Failed(reason) => {
                    warn!(run_id = %run.id, reason, "schedule notification failed");
                    schedules::mark_notify_gave_up(&self.pool, &run.id, super::NotifyState::Failed)
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// 触发一次；`Err` 只表示台账写不动。
    async fn fire(
        &self,
        task: &ScheduledTaskRow,
        run: &ScheduledRunRow,
    ) -> Result<(), sqlx::Error> {
        let Some(env) = self.env.upgrade() else {
            // 进程正在收摊：这一轮跑不成，但要让执行历史说清原因。
            return fire::fail_to_start(
                &self.pool,
                &task.id,
                &run.id,
                "internal: space env is gone".to_string(),
            )
            .await;
        };
        fire::fire(
            FireDeps {
                pool: &self.pool,
                spaces: &self.spaces,
                chat_runs: &self.chat_runs,
                env: &env,
            },
            task,
            run,
        )
        .await
    }

    /// 写一行「这一轮没跑」以及成因。
    async fn record_skipped(
        &self,
        task: &ScheduledTaskRow,
        slot: &str,
        reason: &str,
    ) -> Result<(), sqlx::Error> {
        let finished_at = utc_text(Utc::now());
        schedules::insert_run(
            &self.pool,
            &NewRun {
                id: &Uuid::new_v4().to_string(),
                schedule_id: &task.id,
                trigger_kind: TriggerKind::Cron,
                scheduled_for: slot,
                status: RunStatus::Skipped,
                error: Some(reason),
                finished_at: Some(&finished_at),
            },
        )
        .await?;
        schedules::record_task_last_run(&self.pool, &task.id, RunStatus::Skipped).await?;
        Ok(())
    }

    async fn insert_run(
        &self,
        task: &ScheduledTaskRow,
        trigger_kind: TriggerKind,
        slot: &str,
        status: RunStatus,
    ) -> Result<Option<ScheduledRunRow>, sqlx::Error> {
        schedules::insert_run(
            &self.pool,
            &NewRun {
                id: &Uuid::new_v4().to_string(),
                schedule_id: &task.id,
                trigger_kind,
                scheduled_for: slot,
                status,
                error: None,
                finished_at: None,
            },
        )
        .await
    }
}
