//! 一次触发的执行体：开空间 → 建/取空间会话 → 跑一轮 → 盯着它收尾。
//!
//! 与 `wake::deliver` 是同一条执行链（[`crate::chat::run::spawn_run`] 的
//! `RunAudience::Headless`），差别有两处：目标空间由调度行指定，并且**必须**有人
//! 盯住这一轮的结果 —— 唤醒只需要「投出去」，定时任务要记下跑成了什么样。

use std::sync::Arc;

use chrono::Utc;
use model_provider::Content;
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tracing::warn;

use crate::chat::active::ChatActiveRuns;
use crate::chat::feed::{LiveFrames, TurnFeed};
use crate::chat::run::{RunAudience, RunDeps, RunError, spawn_run};
use crate::chat::sse::ChatSseEvent;
use crate::db::conversations;
use crate::db::schedules::{self, ScheduledRunRow, ScheduledTaskRow};
use crate::space::SpaceEnv;
use crate::space::registry::SpaceRegistry;

use super::{
    MAX_RUN, RunStatus, SKIP_CONVERSATION_BUSY, busy_gives_up, notify, parse_utc_text, retry_at,
    utc_text,
};

/// 投递落到哪个 agent 的会话：空间页里用户看的那个对话面。
pub(crate) const TARGET_AGENT: &str = "@master";

/// 到 [`MAX_RUN`] 被取消时记下的成因。
///
/// 上限从常量现算：写成固定文案的那天起，改 [`MAX_RUN`] 就会让执行历史撒一次谎。
fn timeout_reason() -> String {
    format!(
        "超过单轮上限（{} 分钟），已取消本轮",
        MAX_RUN.as_secs() / 60
    )
}

/// 帧流先于终态帧结束：运行守卫已经 drop，但没人告诉我们结果。
pub(crate) const LOST_TURN_REASON: &str = "本轮的执行端提前结束，没有留下结果";

/// 一次触发需要的外部依赖。
pub(crate) struct FireDeps<'a> {
    pub pool: &'a SqlitePool,
    pub spaces: &'a SpaceRegistry,
    /// 取 `Arc` 而不是引用：watcher 是 spawn 出去的独立任务，要跟着活到这一轮结束。
    pub chat_runs: &'a Arc<ChatActiveRuns>,
    pub env: &'a SpaceEnv,
}

/// 投给目标空间的正文：前缀固定，用户与模型都能一眼看出这是定时触发。
pub(crate) fn delivery_text(task: &ScheduledTaskRow) -> String {
    format!(
        "[定时任务] {name}（schedule={id}）\n\n{prompt}",
        name = task.name,
        id = task.id,
        prompt = task.prompt,
    )
}

/// [`RunError`] → 库里的可读成因。
///
/// 空间类失败带 `DaemonError` 的稳定 code（`work_dir_missing`、`space_not_found`…）：
/// 只有文案的话，日志与执行历史都对不上错误分类表。
fn run_error_text(error: RunError) -> String {
    match error {
        RunError::Space(e) => format!("{}: {e}", e.code()),
        RunError::Agent(detail) => format!("agent_error: {detail}"),
        RunError::Provider(_, detail) => format!("provider_error: {detail}"),
        // 忙走单独分支；走到这里说明状态机漏了一条路径，不替它编成因。
        RunError::Busy => SKIP_CONVERSATION_BUSY.to_string(),
    }
}

/// 执行一次触发；会话忙则排队重试。
///
/// 返回的 `Err` 只表示「台账写不动」—— 领域失败（空间打不开、agent 缺失、会话忙）
/// 都落在 run 行上，由执行历史回答。调用方读回那一行即可。
pub(crate) async fn fire(
    deps: FireDeps<'_>,
    task: &ScheduledTaskRow,
    run: &ScheduledRunRow,
) -> Result<(), sqlx::Error> {
    // ── 1. 打开目标空间 ──────────────────────────────────────────────────
    // 定时任务没有「用户点进来」这个前置动作，因此这里必须自己把空间打开：
    // 调度说过「到点在这个空间里做一件事」，空间关着也要做。
    let Some(hub) = deps.env.hub() else {
        return fail_to_start(
            deps.pool,
            &task.id,
            &run.id,
            "internal: daemon 未注入已打开实例表".to_string(),
        )
        .await;
    };
    if let Err(e) =
        crate::space::open::ensure_open_indexed(deps.env, &hub, &task.target_ws_id).await
    {
        return fail_to_start(deps.pool, &task.id, &run.id, format!("{}: {e}", e.code())).await;
    }

    // ── 2. 目标会话（空间会话，幂等创建）────────────────────────────────
    let conversation_id = match conversations::ensure_space_session(
        deps.pool,
        &task.target_ws_id,
        TARGET_AGENT,
    )
    .await
    {
        Ok((id, _created)) => id,
        Err(e) => {
            return fail_to_start(deps.pool, &task.id, &run.id, format!("db_error: {e}")).await;
        }
    };
    let Some(conv) = conversations::find_by_id(deps.pool, &conversation_id).await? else {
        return fail_to_start(
            deps.pool,
            &task.id,
            &run.id,
            "db_error: 会话在创建之后立刻消失了".to_string(),
        )
        .await;
    };

    // ── 3. 跑一轮 ───────────────────────────────────────────────────────
    let text = delivery_text(task);
    let spawned = spawn_run(
        RunDeps {
            pool: deps.pool,
            env: deps.env,
            spaces: deps.spaces,
            chat_runs: deps.chat_runs.as_ref(),
        },
        &task.target_ws_id,
        &conv,
        text.clone(),
        Content::Text(text),
        RunAudience::Headless,
    )
    .await;

    match spawned {
        Ok(frames) => {
            let claimed = schedules::mark_running(deps.pool, &run.id, &conv.id).await?;
            if claimed == 0 {
                // 这一行已经不是 `pending` 了（被删除、被对账收尾、或被另一次触发
                // 拿走）。把刚起的一轮掐掉，否则它跑完也没有人认领。
                warn!(
                    run_id = %run.id,
                    conversation_id = %conv.id,
                    "scheduled run is no longer pending; cancelling the turn that just started"
                );
                deps.chat_runs.cancel(&conv.id);
                return Ok(());
            }
            let watcher = Watcher {
                pool: deps.pool.clone(),
                chat_runs: Arc::clone(deps.chat_runs),
                schedule_id: task.id.clone(),
                run_id: run.id.clone(),
                conversation_id: conv.id.clone(),
            };
            tokio::spawn(watcher.watch(frames));
            Ok(())
        }
        Err(RunError::Busy) => {
            // 会话里有人在跑：这一轮让路。不是失败，排到 60 秒后再试。
            let now = Utc::now();
            let scheduled_for = parse_utc_text(&run.scheduled_for).unwrap_or(now);
            let attempts_after = run.attempts + 1;
            if busy_gives_up(attempts_after, scheduled_for, now) {
                schedules::mark_skipped(deps.pool, &run.id, SKIP_CONVERSATION_BUSY).await?;
                schedules::record_task_last_run(deps.pool, &task.id, RunStatus::Skipped).await?;
            } else {
                let next = utc_text(retry_at(now));
                schedules::mark_retry(deps.pool, &run.id, &next).await?;
            }
            Ok(())
        }
        Err(error) => fail_to_start(deps.pool, &task.id, &run.id, run_error_text(error)).await,
    }
}

/// 一轮的结局。
pub(crate) enum TurnOutcome {
    /// 跑完了，带本轮正文。
    Completed(String),
    /// 装不上 / 跑挂了，带人话原因。
    Failed(String),
    /// 到 [`MAX_RUN`] 被取消。
    Timeout,
}

/// 一轮跑起来之后的收尾：run 行 + 调度行的「上次结果」。
///
/// 单独抽出来是因为它同时是「唯一写终态的地方」与「测试点」：watcher 的循环体
/// 要真实 DB + 已打开空间 + 带 provider 的 agent，跑不出两小时那条路径；收尾
/// 本身却是纯粹的落库，可以单独驱动。
///
/// 这条路径不改 `attempts` —— 尝试次数由「启动这一轮」（[`schedules::mark_running`]）
/// 记过账了，收尾再记一次就成了两倍。
pub(crate) async fn record_outcome(
    pool: &SqlitePool,
    schedule_id: &str,
    run_id: &str,
    outcome: TurnOutcome,
) -> Result<(), sqlx::Error> {
    let (status, digest, error) = outcome.parts();
    schedules::finish_run(
        pool,
        run_id,
        status,
        digest.as_deref(),
        error.as_deref(),
        notify::should_notify(status),
    )
    .await?;
    schedules::record_task_last_run(pool, schedule_id, status).await?;
    Ok(())
}

/// 根本没能跑起来：落 `failed` 并计一次尝试。
///
/// 与 [`record_outcome`] 分开：这里连「跑过」都不成立，`attempts` 因此要加。
pub(crate) async fn fail_to_start(
    pool: &SqlitePool,
    schedule_id: &str,
    run_id: &str,
    reason: String,
) -> Result<(), sqlx::Error> {
    schedules::mark_failed(pool, run_id, &reason).await?;
    schedules::record_task_last_run(pool, schedule_id, RunStatus::Failed).await?;
    Ok(())
}

/// 盯住一轮：等终态帧，或到点取消。
///
/// **持有帧接收端**是这里唯一的设计取舍：它让 `TurnFeed::has_subscribers` 恒为真，
/// `chat::run` 的空闲看门狗（只在没人看时生效）对这一轮永不触发。要的就是这个
/// 行为（定时任务的一轮不该因为「没人看」被掐断），代价是必须自带 [`MAX_RUN`] ——
/// 没有它，一个卡死的请求会让该会话的运行条目永久占住，之后每次触发都只能记「忙」。
struct Watcher {
    pool: SqlitePool,
    chat_runs: Arc<ChatActiveRuns>,
    schedule_id: String,
    run_id: String,
    conversation_id: String,
}

impl Watcher {
    async fn watch(self, frames: LiveFrames) {
        let mut rx = frames.rx;
        let feed = Arc::clone(&frames.feed);
        let deadline = tokio::time::sleep(MAX_RUN);
        tokio::pin!(deadline);

        let outcome = loop {
            tokio::select! {
                received = rx.recv() => match received {
                    Ok(frame) => match &frame.event {
                        ChatSseEvent::TurnComplete { text, .. } => {
                            break TurnOutcome::Completed(text.clone());
                        }
                        ChatSseEvent::Error {
                            message, cancelled, ..
                        } => break TurnOutcome::Failed(failure_reason(message, *cancelled)),
                        _ => {}
                    },
                    // 帧流有界：慢订阅者会丢掉中间的帧，但终态帧丢了就再也等不到。
                    // 落后时先看这一轮是否已经收尾，收尾了就用快照正文收场。
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        warn!(
                            run_id = %self.run_id,
                            missed,
                            "schedule watcher fell behind the frame stream"
                        );
                        if turn_finished(&feed) {
                            let text = feed.snapshot().text;
                            break if text.trim().is_empty() {
                                TurnOutcome::Failed(LOST_TURN_REASON.to_string())
                            } else {
                                TurnOutcome::Completed(text)
                            };
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break TurnOutcome::Failed(LOST_TURN_REASON.to_string());
                    }
                },
                _ = &mut deadline => {
                    // 先取消再收尾：顺序反了，这一行已经是 `timeout` 而 looper
                    // 还在跑，下一次触发会被自己判成「会话忙」。
                    self.chat_runs.cancel(&self.conversation_id);
                    break TurnOutcome::Timeout;
                }
            }
        };

        self.settle(outcome).await;
    }

    /// 收尾。写不动只 warn：这一轮已经结束，日志是唯一还能留下的证据。
    ///
    /// 终态不可回退由 `finish_run` 的谓词保证：删除前被 cancel 的那一轮也会走到
    /// 这里，而它的行已经随调度行级联消失了。
    async fn settle(&self, outcome: TurnOutcome) {
        if let Err(e) = record_outcome(&self.pool, &self.schedule_id, &self.run_id, outcome).await {
            warn!(run_id = %self.run_id, error = %e, "cannot record the run outcome");
        }
    }
}

impl TurnOutcome {
    fn parts(self) -> (RunStatus, Option<String>, Option<String>) {
        match self {
            Self::Completed(text) => (RunStatus::Completed, Some(notify::digest_of(&text)), None),
            Self::Failed(reason) => (RunStatus::Failed, None, Some(reason)),
            Self::Timeout => (RunStatus::Timeout, None, Some(timeout_reason())),
        }
    }
}

/// 这一轮是否已经发过终态帧（`attach` 在终态之后返回 `None`）。
fn turn_finished(feed: &Arc<TurnFeed>) -> bool {
    feed.attach().is_none()
}

/// 错误帧 → 人话。取消是用户主动停的，与真故障分开说。
fn failure_reason(message: &str, cancelled: bool) -> String {
    if cancelled {
        format!("运行被取消：{message}")
    } else {
        message.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schedules::NewTask;
    use crate::schedule::TriggerKind;

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        sqlx::raw_sql(
            "INSERT INTO teams (id, display_name) VALUES ('team', 'team'); \
             INSERT INTO workspaces (id, work_dir, team_id, label) \
             VALUES ('ws_a', '/tmp/a', 'team', '甲');",
        )
        .execute(&pool)
        .await
        .unwrap();
        (pool, dir)
    }

    async fn seed(pool: &SqlitePool) -> (ScheduledTaskRow, ScheduledRunRow) {
        schedules::insert_task(
            pool,
            &NewTask {
                id: "s1",
                name: "每日汇报",
                target_ws_id: "ws_a",
                prompt: "汇报昨天的进展",
                cron_expr: "0 9 * * *",
                enabled: true,
                next_run_at: Some("2026-03-05 01:00:00"),
            },
        )
        .await
        .unwrap();
        let run = schedules::insert_run(
            pool,
            &schedules::NewRun {
                id: "r1",
                schedule_id: "s1",
                trigger_kind: TriggerKind::Cron,
                scheduled_for: "2026-03-05 01:00:00",
                status: RunStatus::Pending,
                error: None,
                finished_at: None,
            },
        )
        .await
        .unwrap()
        .expect("首次写入槽位必须成功");
        schedules::mark_running(pool, "r1", "conv-1").await.unwrap();
        let task = schedules::task_by_id(pool, "s1").await.unwrap().unwrap();
        let run = schedules::run_by_id(pool, &run.id).await.unwrap().unwrap();
        (task, run)
    }

    #[test]
    fn the_delivered_text_names_the_schedule() {
        let task = ScheduledTaskRow {
            id: "s-1".into(),
            name: "每日站会汇报".into(),
            target_ws_id: "ws_a".into(),
            prompt: "汇报昨天的进展".into(),
            cron_expr: "0 9 * * *".into(),
            enabled: true,
            next_run_at: None,
            last_run_at: None,
            last_status: None,
            created_at: "2026-03-01 00:00:00".into(),
            updated_at: "2026-03-01 00:00:00".into(),
            target_label: "研发".into(),
        };
        assert_eq!(
            delivery_text(&task),
            "[定时任务] 每日站会汇报（schedule=s-1）\n\n汇报昨天的进展"
        );
    }

    /// 空间类失败要带稳定 code：只有文案的话，执行历史与错误分类表对不上。
    #[test]
    fn space_failures_carry_their_code() {
        let error = run_error_text(RunError::Space(crate::error::DaemonError::WorkDirMissing {
            work_dir: "/w".into(),
        }));
        assert!(error.starts_with("work_dir_missing: "), "{error}");
    }

    /// 取消与真故障分开说：用户自己停的那一轮不该看起来像系统炸了。
    #[test]
    fn a_cancelled_turn_says_so() {
        assert_eq!(
            failure_reason("对话已被用户取消", true),
            "运行被取消：对话已被用户取消"
        );
        assert_eq!(failure_reason("上游 500", false), "上游 500");
    }

    /// 超时的收尾：状态、成因、通知排队三件事一起落地，并同步调度行的「上次结果」。
    /// 这条路径真实跑不出来（要等两小时），只能直接驱动收尾函数。
    #[tokio::test]
    async fn a_timeout_finishes_the_run_and_queues_a_notification() {
        let (pool, _dir) = test_pool().await;
        let (task, run) = seed(&pool).await;

        record_outcome(&pool, &task.id, &run.id, TurnOutcome::Timeout)
            .await
            .unwrap();

        let row = schedules::run_by_id(&pool, "r1").await.unwrap().unwrap();
        assert_eq!(row.status, "timeout");
        assert!(row.finished_at.is_some());
        assert_eq!(row.error.as_deref(), Some(timeout_reason().as_str()));
        assert_eq!(row.notify_state, "pending", "超时要通知用户");
        let task_row = schedules::task_by_id(&pool, "s1").await.unwrap().unwrap();
        assert_eq!(task_row.last_status.as_deref(), Some("timeout"));
        assert!(task_row.last_run_at.is_some());
    }

    /// 完成：摘要按字符截断后落库，并排队通知。
    #[tokio::test]
    async fn a_completed_run_records_the_digest() {
        let (pool, _dir) = test_pool().await;
        let (task, run) = seed(&pool).await;

        record_outcome(
            &pool,
            &task.id,
            &run.id,
            TurnOutcome::Completed("汉".repeat(600)),
        )
        .await
        .unwrap();

        let row = schedules::run_by_id(&pool, "r1").await.unwrap().unwrap();
        assert_eq!(row.status, "completed");
        let digest = row.result_digest.expect("完成必须留摘要");
        assert_eq!(digest.chars().count(), notify::RESULT_DIGEST_CHARS);
        assert_eq!(row.notify_state, "pending");
    }

    /// 放弃的轮次不通知：系统自己的账不该进用户的对话。
    #[tokio::test]
    async fn a_skipped_run_does_not_queue_a_notification() {
        let (pool, _dir) = test_pool().await;
        let (task, run) = seed(&pool).await;

        schedules::mark_skipped(&pool, "r1", SKIP_CONVERSATION_BUSY)
            .await
            .unwrap();
        let skipped = schedules::run_by_id(&pool, "r1").await.unwrap().unwrap();
        assert!(
            !notify::should_notify(RunStatus::from_db(&skipped.status).unwrap()),
            "skipped 不在通知集合里"
        );
        assert_eq!(
            skipped.notify_state, "none",
            "跳过时通知态保持 none，调度循环不会去发它"
        );
        let _ = (task, run);
    }
}
