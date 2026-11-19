//! `scheduled_tasks` / `scheduled_runs` 的 DAO —— 定时任务的排期与每轮执行记录。
//!
//! 两张表的分工与台账一致：调度行只记「下一次什么时候、上次结果如何」，每次触发
//! 各占一行 run。终态不可回退由 SQL 谓词保证（`WHERE status IN ('pending','running')`），
//! 不在应用层「先读再判」—— 读与写之间别的 tick 就能把这一行推过终态。
//!
//! 时间列一律 UTC 文本（与 `datetime('now')` 同格式），cron 计算的时区换算在
//! [`crate::schedule::cron`] 里做。

use sqlx::SqlitePool;

use crate::schedule::{NotifyState, RunStatus, TriggerKind};

/// 任务行的列清单：目标空间的人读标签与任务同一行取出。
///
/// 空 label 回退 `team_id`，与 `/api/spaces` 的展示口径一致；`JOIN` 而非二次查询
/// 是因为 `target_ws_id` 有外键（`ON DELETE CASCADE`），调度行不可能指向不存在的空间。
const TASK_SELECT: &str = "SELECT t.id, t.name, t.target_ws_id, t.prompt, t.cron_expr, \
     t.enabled, t.next_run_at, t.last_run_at, t.last_status, t.created_at, t.updated_at, \
     COALESCE(NULLIF(w.label, ''), w.team_id) AS target_label \
     FROM scheduled_tasks t JOIN workspaces w ON w.id = t.target_ws_id";

const RUN_COLUMNS: &str = "id, schedule_id, trigger_kind, scheduled_for, status, attempts, \
     next_attempt_at, conversation_id, started_at, finished_at, result_digest, error, \
     notify_state, notify_attempts, notify_attempt_at, notified_at";

/// 一条调度。`target_label` 不落库 —— 它是 `workspaces` 的投影。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ScheduledTaskRow {
    pub id: String,
    pub name: String,
    pub target_ws_id: String,
    pub prompt: String,
    pub cron_expr: String,
    pub enabled: bool,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_status: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub target_label: String,
}

/// 一次触发。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ScheduledRunRow {
    pub id: String,
    pub schedule_id: String,
    pub trigger_kind: String,
    pub scheduled_for: String,
    pub status: String,
    pub attempts: i64,
    pub next_attempt_at: Option<String>,
    pub conversation_id: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub result_digest: Option<String>,
    pub error: Option<String>,
    pub notify_state: String,
    pub notify_attempts: i64,
    pub notify_attempt_at: Option<String>,
    pub notified_at: Option<String>,
}

/// 写入调度行的入参；`replace_task` 与 `insert_task` 共用同一份字段集。
pub struct NewTask<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub target_ws_id: &'a str,
    pub prompt: &'a str,
    pub cron_expr: &'a str,
    pub enabled: bool,
    pub next_run_at: Option<&'a str>,
}

/// 写入 run 行的入参。`finished_at` 只有终态行才有值。
pub struct NewRun<'a> {
    pub id: &'a str,
    pub schedule_id: &'a str,
    pub trigger_kind: TriggerKind,
    pub scheduled_for: &'a str,
    pub status: RunStatus,
    pub error: Option<&'a str>,
    pub finished_at: Option<&'a str>,
}

/// 全部调度，`created_at DESC`（列表接口的顺序）。
pub async fn list_tasks(pool: &SqlitePool) -> Result<Vec<ScheduledTaskRow>, sqlx::Error> {
    // `created_at` 只到秒：同一秒建出的两条会退化到随机主键顺序，故用 rowid 兜底。
    sqlx::query_as::<_, ScheduledTaskRow>(&format!(
        "{TASK_SELECT} ORDER BY t.created_at DESC, t.rowid DESC"
    ))
    .fetch_all(pool)
    .await
}

pub async fn task_by_id(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<ScheduledTaskRow>, sqlx::Error> {
    sqlx::query_as::<_, ScheduledTaskRow>(&format!("{TASK_SELECT} WHERE t.id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// 到期的已启用调度，按到期先后（同一 tick 先处理欠得久的）。
pub async fn due_tasks(
    pool: &SqlitePool,
    now: &str,
    limit: i64,
) -> Result<Vec<ScheduledTaskRow>, sqlx::Error> {
    sqlx::query_as::<_, ScheduledTaskRow>(&format!(
        "{TASK_SELECT} WHERE t.enabled = 1 AND t.next_run_at IS NOT NULL AND t.next_run_at <= ? \
         ORDER BY t.next_run_at LIMIT ?"
    ))
    .bind(now)
    .bind(limit)
    .fetch_all(pool)
    .await
}

pub async fn insert_task(pool: &SqlitePool, task: &NewTask<'_>) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO scheduled_tasks (id, name, target_ws_id, prompt, cron_expr, enabled, next_run_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(task.id)
    .bind(task.name)
    .bind(task.target_ws_id)
    .bind(task.prompt)
    .bind(task.cron_expr)
    .bind(task.enabled)
    .bind(task.next_run_at)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 全字段覆盖（`PATCH` 的落地点）。
///
/// 不做「只写变化的那几列」：改动一律先读、改、整行写回，少一条按字段拼的 UPDATE
/// 就少一处「哪个字段漏了」的可能。单用户宿主上两个 PATCH 撞在一起的概率不值得
/// 为它引入乐观锁。
pub async fn replace_task(pool: &SqlitePool, task: &NewTask<'_>) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE scheduled_tasks SET name = ?, target_ws_id = ?, prompt = ?, cron_expr = ?, \
         enabled = ?, next_run_at = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(task.name)
    .bind(task.target_ws_id)
    .bind(task.prompt)
    .bind(task.cron_expr)
    .bind(task.enabled)
    .bind(task.next_run_at)
    .bind(task.id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn delete_task(pool: &SqlitePool, id: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM scheduled_tasks WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 推进下一槽位（CAS）：`1` 表示本次推进生效，`0` 表示别人已经推进过。
///
/// 没有这道 CAS，两个 tick（重启交叠、或多实例误开）会把同一个到期槽位各触发一次。
pub async fn advance_next_run(
    pool: &SqlitePool,
    id: &str,
    old: &str,
    new: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE scheduled_tasks SET next_run_at = ?, updated_at = datetime('now') \
         WHERE id = ? AND next_run_at = ?",
    )
    .bind(new)
    .bind(id)
    .bind(old)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 该调度最近的未完成 run（`pending` / `running`）。
pub async fn unfinished_run(
    pool: &SqlitePool,
    schedule_id: &str,
) -> Result<Option<ScheduledRunRow>, sqlx::Error> {
    let sql = format!(
        "SELECT {RUN_COLUMNS} FROM scheduled_runs WHERE schedule_id = ? \
         AND status IN ('{}','{}') ORDER BY started_at DESC LIMIT 1",
        RunStatus::Pending.as_str(),
        RunStatus::Running.as_str(),
    );
    sqlx::query_as::<_, ScheduledRunRow>(&sql)
        .bind(schedule_id)
        .fetch_optional(pool)
        .await
}

/// 写入一行 run；返回 `None` 表示唯一槽位索引挡下了重复触发。
pub async fn insert_run(
    pool: &SqlitePool,
    run: &NewRun<'_>,
) -> Result<Option<ScheduledRunRow>, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO scheduled_runs \
         (id, schedule_id, trigger_kind, scheduled_for, status, error, finished_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
    )
    .bind(run.id)
    .bind(run.schedule_id)
    .bind(run.trigger_kind.as_str())
    .bind(run.scheduled_for)
    .bind(run.status.as_str())
    .bind(run.error)
    .bind(run.finished_at)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    run_by_id(pool, run.id).await
}

pub async fn run_by_id(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<ScheduledRunRow>, sqlx::Error> {
    let sql = format!("SELECT {RUN_COLUMNS} FROM scheduled_runs WHERE id = ?");
    sqlx::query_as::<_, ScheduledRunRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// 某调度的执行历史，`started_at DESC`。
pub async fn runs_by_schedule(
    pool: &SqlitePool,
    schedule_id: &str,
    limit: i64,
) -> Result<Vec<ScheduledRunRow>, sqlx::Error> {
    // 与列表同理：`started_at` 同秒的行按插入序（rowid）排列，不让随机主键决定先后。
    let sql = format!(
        "SELECT {RUN_COLUMNS} FROM scheduled_runs WHERE schedule_id = ? \
         ORDER BY started_at DESC, rowid DESC LIMIT ?"
    );
    sqlx::query_as::<_, ScheduledRunRow>(&sql)
        .bind(schedule_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// 到了重试时刻的 run（会话忙时排的队）。
pub async fn due_retries(
    pool: &SqlitePool,
    now: &str,
    limit: i64,
) -> Result<Vec<ScheduledRunRow>, sqlx::Error> {
    let sql = format!(
        "SELECT {RUN_COLUMNS} FROM scheduled_runs \
         WHERE status = '{}' AND next_attempt_at IS NOT NULL AND next_attempt_at <= ? \
         ORDER BY next_attempt_at LIMIT ?",
        RunStatus::Pending.as_str(),
    );
    sqlx::query_as::<_, ScheduledRunRow>(&sql)
        .bind(now)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// 会话已经跑起来了：记下会话 id，尝试次数 +1，清掉重试时刻。
pub async fn mark_running(
    pool: &SqlitePool,
    run_id: &str,
    conversation_id: &str,
) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE scheduled_runs SET status = '{}', conversation_id = ?, attempts = attempts + 1, \
         next_attempt_at = NULL WHERE id = ? AND status = '{}'",
        RunStatus::Running.as_str(),
        RunStatus::Pending.as_str(),
    );
    let result = sqlx::query(&sql)
        .bind(conversation_id)
        .bind(run_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 会话忙：这一轮让路，等 [`crate::schedule::RETRY_INTERVAL`] 之后再试。
pub async fn mark_retry(
    pool: &SqlitePool,
    run_id: &str,
    next_attempt_at: &str,
) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE scheduled_runs SET status = '{}', attempts = attempts + 1, next_attempt_at = ? \
         WHERE id = ? AND status = '{}'",
        RunStatus::Pending.as_str(),
        RunStatus::Pending.as_str(),
    );
    let result = sqlx::query(&sql)
        .bind(next_attempt_at)
        .bind(run_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 根本没跑起来（空间打不开、agent 缺失、provider 不可用）：落 `failed` 并计一次尝试。
///
/// 与 [`mark_skipped`] 分开：`skipped` 是「这一轮本来就没跑」，`failed` 是「试着跑
/// 但装不上」。两者的重试语义不同，执行历史里也必须分得开。
pub async fn mark_failed(pool: &SqlitePool, run_id: &str, error: &str) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE scheduled_runs SET status = '{}', attempts = attempts + 1, error = ?, \
         finished_at = datetime('now') WHERE id = ? AND status IN ('{}','{}')",
        RunStatus::Failed.as_str(),
        RunStatus::Pending.as_str(),
        RunStatus::Running.as_str(),
    );
    let result = sqlx::query(&sql)
        .bind(error)
        .bind(run_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 放弃这一轮（忙到阈值、错过槽位、前一轮未完成）：落 `skipped` 并记成因。
pub async fn mark_skipped(
    pool: &SqlitePool,
    run_id: &str,
    reason: &str,
) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE scheduled_runs SET status = '{}', attempts = attempts + 1, error = ?, \
         finished_at = datetime('now') WHERE id = ? AND status IN ('{}','{}')",
        RunStatus::Skipped.as_str(),
        RunStatus::Pending.as_str(),
        RunStatus::Running.as_str(),
    );
    let result = sqlx::query(&sql)
        .bind(reason)
        .bind(run_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 写终态：`status` / `finished_at` / 摘要与成因，一次 UPDATE 落地。
///
/// `notify` 为真时同时把通知排进队列 —— 状态与「该不该通知」是同一件事的两面，
/// 分两条 UPDATE 写会留下「completed 但通知永远发不出去」的窗口。
///
/// 谓词只放 `pending` / `running`：终态不可回退，晚到的第二次收尾（watcher 与
/// 删除前的 cancel 各写一次）不得把 `timeout` 改回 `completed`。
pub async fn finish_run(
    pool: &SqlitePool,
    run_id: &str,
    status: RunStatus,
    result_digest: Option<&str>,
    error: Option<&str>,
    notify: bool,
) -> Result<u64, sqlx::Error> {
    let notify_sql = if notify {
        format!(
            ", notify_state = '{}', notify_attempt_at = datetime('now')",
            NotifyState::Pending.as_str()
        )
    } else {
        String::new()
    };
    let sql = format!(
        "UPDATE scheduled_runs SET status = ?, finished_at = datetime('now'), result_digest = ?, \
         error = ?{notify_sql} WHERE id = ? AND status IN ('{}','{}')",
        RunStatus::Pending.as_str(),
        RunStatus::Running.as_str(),
    );
    let result = sqlx::query(&sql)
        .bind(status.as_str())
        .bind(result_digest)
        .bind(error)
        .bind(run_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 调度行记下最近一次结果 —— 列表页的「上次跑成什么样」。
pub async fn record_task_last_run(
    pool: &SqlitePool,
    task_id: &str,
    status: RunStatus,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE scheduled_tasks SET last_run_at = datetime('now'), last_status = ?, \
         updated_at = datetime('now') WHERE id = ?",
    )
    .bind(status.as_str())
    .bind(task_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 启动对账：上个进程停在 `running` 的行没有观察者了。
///
/// `pending` 保留 —— 它本来就等着重试，退回去等于丢失「会话忙」这条已有结论。
pub async fn interrupt_running(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE scheduled_runs SET status = '{}', finished_at = datetime('now'), \
         error = '进程重启时仍在运行' WHERE status = '{}'",
        RunStatus::Interrupted.as_str(),
        RunStatus::Running.as_str(),
    );
    let result = sqlx::query(&sql).execute(pool).await?;
    Ok(result.rows_affected())
}

/// 到点的待发通知。`notify_attempts` 已达上限的行不再入选。
pub async fn due_notifications(
    pool: &SqlitePool,
    now: &str,
    max_attempts: i64,
    limit: i64,
) -> Result<Vec<ScheduledRunRow>, sqlx::Error> {
    let sql = format!(
        "SELECT {RUN_COLUMNS} FROM scheduled_runs \
         WHERE notify_state = '{}' AND notify_attempts < ? \
           AND (notify_attempt_at IS NULL OR notify_attempt_at <= ?) \
         ORDER BY notify_attempt_at LIMIT ?",
        NotifyState::Pending.as_str(),
    );
    sqlx::query_as::<_, ScheduledRunRow>(&sql)
        .bind(max_attempts)
        .bind(now)
        .bind(limit)
        .fetch_all(pool)
        .await
}

pub async fn mark_notified(pool: &SqlitePool, run_id: &str) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE scheduled_runs SET notify_state = '{}', notified_at = datetime('now') WHERE id = ?",
        NotifyState::Sent.as_str(),
    );
    let result = sqlx::query(&sql).bind(run_id).execute(pool).await?;
    Ok(result.rows_affected())
}

pub async fn mark_notify_retry(
    pool: &SqlitePool,
    run_id: &str,
    next_attempt_at: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE scheduled_runs SET notify_attempts = notify_attempts + 1, notify_attempt_at = ? \
         WHERE id = ?",
    )
    .bind(next_attempt_at)
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 通知彻底放弃 / 投不出去：两种成因分开记，前者是预算用尽，后者是链路坏了。
pub async fn mark_notify_gave_up(
    pool: &SqlitePool,
    run_id: &str,
    state: NotifyState,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE scheduled_runs SET notify_state = ?, notify_attempts = notify_attempts + 1 \
         WHERE id = ?",
    )
    .bind(state.as_str())
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        sqlx::raw_sql(
            "INSERT INTO teams (id, display_name) VALUES ('team', 'team'); \
             INSERT INTO workspaces (id, work_dir, team_id, label) \
             VALUES ('ws_a', '/tmp/a', 'team', '甲'), ('ws_b', '/tmp/b', 'team', NULL);",
        )
        .execute(&pool)
        .await
        .unwrap();
        (pool, dir)
    }

    async fn seed_task(pool: &SqlitePool, id: &str, next_run_at: Option<&str>) {
        insert_task(
            pool,
            &NewTask {
                id,
                name: "每日汇报",
                target_ws_id: "ws_a",
                prompt: "汇报昨天的进展",
                cron_expr: "0 9 * * *",
                enabled: true,
                next_run_at,
            },
        )
        .await
        .unwrap();
    }

    async fn seed_run(pool: &SqlitePool, id: &str, task_id: &str, slot: &str) -> ScheduledRunRow {
        insert_run(
            pool,
            &NewRun {
                id,
                schedule_id: task_id,
                trigger_kind: TriggerKind::Cron,
                scheduled_for: slot,
                status: RunStatus::Pending,
                error: None,
                finished_at: None,
            },
        )
        .await
        .unwrap()
        .expect("首次写入同一个槽位必须成功")
    }

    /// CAS 是「同一槽位只触发一次」的唯一保证：第二次推进必须落空，否则两个
    /// tick 会把同一行各 fire 一遍。
    #[tokio::test]
    async fn advancing_the_slot_only_succeeds_once() {
        let (pool, _dir) = test_pool().await;
        seed_task(&pool, "s1", Some("2026-01-01 00:00:00")).await;

        let first = advance_next_run(&pool, "s1", "2026-01-01 00:00:00", "2026-01-02 00:00:00")
            .await
            .unwrap();
        let second = advance_next_run(&pool, "s1", "2026-01-01 00:00:00", "2026-01-03 00:00:00")
            .await
            .unwrap();

        assert_eq!(first, 1, "第一个推进者拿到槽位");
        assert_eq!(second, 0, "旧槽位已被推进，后来的写入必须落空");
        let now = task_by_id(&pool, "s1").await.unwrap().unwrap().next_run_at;
        assert_eq!(
            now.as_deref(),
            Some("2026-01-02 00:00:00"),
            "后者不得覆盖前者"
        );
    }

    /// 唯一索引兜住「CAS 之后、插入之前」的重启窗口：同一 cron 槽位只会有一行。
    #[tokio::test]
    async fn the_slot_index_rejects_a_second_cron_run() {
        let (pool, _dir) = test_pool().await;
        seed_task(&pool, "s1", Some("2026-01-01 00:00:00")).await;
        seed_run(&pool, "r1", "s1", "2026-01-01 00:00:00").await;

        let duplicate = insert_run(
            &pool,
            &NewRun {
                id: "r2",
                schedule_id: "s1",
                trigger_kind: TriggerKind::Cron,
                scheduled_for: "2026-01-01 00:00:00",
                status: RunStatus::Pending,
                error: None,
                finished_at: None,
            },
        )
        .await
        .unwrap();
        assert!(duplicate.is_none(), "同一 cron 槽位不得出现第二行");
    }

    /// 手动触发不受槽位索引约束：同一秒点两次是两个独立请求，各有自己的 run 行。
    #[tokio::test]
    async fn manual_runs_are_not_folded_into_one_slot() {
        let (pool, _dir) = test_pool().await;
        seed_task(&pool, "s1", None).await;

        for id in ["m1", "m2"] {
            let run = insert_run(
                &pool,
                &NewRun {
                    id,
                    schedule_id: "s1",
                    trigger_kind: TriggerKind::Manual,
                    scheduled_for: "2026-01-01 00:00:00",
                    status: RunStatus::Pending,
                    error: None,
                    finished_at: None,
                },
            )
            .await
            .unwrap();
            assert!(run.is_some(), "手动触发不该被唯一索引挡下");
        }
    }

    /// 重启对账：`running` 收成 `interrupted`，`pending` 原样留着继续重试。
    #[tokio::test]
    async fn restarting_interrupts_running_rows_and_keeps_pending_ones() {
        let (pool, _dir) = test_pool().await;
        seed_task(&pool, "s1", None).await;
        seed_run(&pool, "r1", "s1", "2026-01-01 00:00:00").await;
        seed_run(&pool, "r2", "s1", "2026-01-01 00:01:00").await;
        mark_running(&pool, "r1", "conv-1").await.unwrap();

        let interrupted = interrupt_running(&pool).await.unwrap();

        assert_eq!(interrupted, 1, "只有 running 的那一行被收尾");
        let r1 = run_by_id(&pool, "r1").await.unwrap().unwrap();
        assert_eq!(r1.status, "interrupted");
        assert!(r1.finished_at.is_some(), "收尾必须留时间戳");
        assert_eq!(r1.error.as_deref(), Some("进程重启时仍在运行"));
        let r2 = run_by_id(&pool, "r2").await.unwrap().unwrap();
        assert_eq!(r2.status, "pending", "重试队列不得被对账清空");
    }

    /// 终态不可回退：晚到的第二次收尾既不改状态，也不再把通知排进队列。
    #[tokio::test]
    async fn a_terminal_run_cannot_be_finished_twice() {
        let (pool, _dir) = test_pool().await;
        seed_task(&pool, "s1", None).await;
        seed_run(&pool, "r1", "s1", "2026-01-01 00:00:00").await;
        mark_running(&pool, "r1", "conv-1").await.unwrap();

        let first = finish_run(
            &pool,
            "r1",
            RunStatus::Timeout,
            None,
            Some("超过单轮上限"),
            true,
        )
        .await
        .unwrap();
        let second = finish_run(
            &pool,
            "r1",
            RunStatus::Completed,
            Some("迟到的正文"),
            None,
            true,
        )
        .await
        .unwrap();

        assert_eq!(first, 1);
        assert_eq!(second, 0, "终态行不得再被写一次");
        let row = run_by_id(&pool, "r1").await.unwrap().unwrap();
        assert_eq!(row.status, "timeout");
        assert!(row.result_digest.is_none(), "迟到的正文不得覆盖超时的成因");
    }

    /// `ON DELETE CASCADE`：删空间连带删调度与 run，删调度连带删 run。
    /// 少了任一条，库里就会留下指向已消失资源的孤儿行。
    #[tokio::test]
    async fn deleting_a_space_takes_its_runs_with_it() {
        let (pool, _dir) = test_pool().await;
        seed_task(&pool, "s1", None).await;
        seed_run(&pool, "r1", "s1", "2026-01-01 00:00:00").await;
        insert_task(
            &pool,
            &NewTask {
                id: "s2",
                name: "第二",
                target_ws_id: "ws_b",
                prompt: "p",
                cron_expr: "* * * * *",
                enabled: true,
                next_run_at: None,
            },
        )
        .await
        .unwrap();
        seed_run(&pool, "r2", "s2", "2026-01-01 00:00:00").await;

        sqlx::query("DELETE FROM workspaces WHERE id = 'ws_a'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            task_by_id(&pool, "s1").await.unwrap().is_none(),
            "空间没了，指向它的调度行必须一起消失"
        );
        assert!(run_by_id(&pool, "r1").await.unwrap().is_none());
        assert!(
            task_by_id(&pool, "s2").await.unwrap().is_some(),
            "别的空间的调度不受影响"
        );

        delete_task(&pool, "s2").await.unwrap();
        assert!(
            run_by_id(&pool, "r2").await.unwrap().is_none(),
            "删调度要连带删它的执行记录"
        );
    }

    /// CHECK 与枚举必须同集合：枚举多一个值而迁移漏改，写入只会在运行期报
    /// `constraint failed`，而那时已经跑了一轮。
    #[tokio::test]
    async fn the_status_check_lists_every_run_status() {
        let (pool, _dir) = test_pool().await;
        seed_task(&pool, "s1", None).await;
        let run = seed_run(&pool, "r1", "s1", "2026-01-01 00:00:00").await;

        for status in RunStatus::ALL {
            sqlx::query("UPDATE scheduled_runs SET status = ? WHERE id = ?")
                .bind(status.as_str())
                .bind(&run.id)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("CHECK 不认识状态 {}：{e}", status.as_str()));
        }
        assert!(
            sqlx::query("UPDATE scheduled_runs SET status = 'made_up' WHERE id = ?")
                .bind(&run.id)
                .execute(&pool)
                .await
                .is_err(),
            "CHECK 必须拒绝清单外的状态"
        );
        for state in NotifyState::ALL {
            sqlx::query("UPDATE scheduled_runs SET notify_state = ? WHERE id = ?")
                .bind(state.as_str())
                .bind(&run.id)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("CHECK 不认识通知态 {}：{e}", state.as_str()));
        }
    }

    /// 通知队列只取到点的、还没烧完预算的 `pending` 行。
    #[tokio::test]
    async fn only_due_notifications_within_the_attempt_budget_are_listed() {
        let (pool, _dir) = test_pool().await;
        seed_task(&pool, "s1", None).await;
        seed_run(&pool, "r1", "s1", "2026-01-01 00:00:00").await;
        mark_running(&pool, "r1", "conv-1").await.unwrap();
        finish_run(
            &pool,
            "r1",
            RunStatus::Completed,
            Some("做完了"),
            None,
            true,
        )
        .await
        .unwrap();

        let due = due_notifications(&pool, "2999-01-01 00:00:00", 20, 20)
            .await
            .unwrap();
        assert_eq!(due.len(), 1, "刚跑完的通知就在队列里");

        sqlx::query("UPDATE scheduled_runs SET notify_attempts = 20 WHERE id = 'r1'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            due_notifications(&pool, "2999-01-01 00:00:00", 20, 20)
                .await
                .unwrap()
                .is_empty(),
            "预算用完的行不得再入选"
        );

        sqlx::query(
            "UPDATE scheduled_runs SET notify_attempts = 0, \
             notify_attempt_at = '2999-01-01 00:00:00' WHERE id = 'r1'",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            due_notifications(&pool, "2998-01-01 00:00:00", 20, 20)
                .await
                .unwrap()
                .is_empty(),
            "还没到重试时刻的行不得入选"
        );
    }

    /// 未完成 run 的判据只认 `pending` / `running`：`skipped` 与终态都不是阻碍。
    #[tokio::test]
    async fn only_pending_and_running_rows_block_a_new_run() {
        let (pool, _dir) = test_pool().await;
        seed_task(&pool, "s1", None).await;
        assert!(unfinished_run(&pool, "s1").await.unwrap().is_none());

        seed_run(&pool, "r1", "s1", "2026-01-01 00:00:00").await;
        let blocking = unfinished_run(&pool, "s1").await.unwrap();
        assert_eq!(blocking.map(|row| row.id), Some("r1".to_string()));

        mark_skipped(&pool, "r1", "previous_running").await.unwrap();
        assert!(
            unfinished_run(&pool, "s1").await.unwrap().is_none(),
            "skipped 不再是阻碍"
        );
        let row = run_by_id(&pool, "r1").await.unwrap().unwrap();
        assert_eq!(row.attempts, 1, "放弃也算一次尝试");
        assert!(row.finished_at.is_some());
    }

    /// 标签回退：空 label 用 `team_id`，两个空间各自取到自己的。
    #[tokio::test]
    async fn the_task_projection_falls_back_to_the_team_id() {
        let (pool, _dir) = test_pool().await;
        seed_task(&pool, "s1", None).await;
        insert_task(
            &pool,
            &NewTask {
                id: "s2",
                name: "第二",
                target_ws_id: "ws_b",
                prompt: "p",
                cron_expr: "* * * * *",
                enabled: false,
                next_run_at: None,
            },
        )
        .await
        .unwrap();

        let rows = list_tasks(&pool).await.unwrap();
        let labels: std::collections::HashMap<String, String> = rows
            .iter()
            .map(|row| (row.id.clone(), row.target_label.clone()))
            .collect();
        assert_eq!(labels.get("s1").map(String::as_str), Some("甲"));
        assert_eq!(
            labels.get("s2").map(String::as_str),
            Some("team"),
            "空 label 回退 team_id"
        );
        let enabled: std::collections::HashMap<&str, bool> = rows
            .iter()
            .map(|row| (row.id.as_str(), row.enabled))
            .collect();
        assert_eq!(enabled.get("s1"), Some(&true));
        assert_eq!(enabled.get("s2"), Some(&false), "INTEGER 0 要读回 false");
    }
}
