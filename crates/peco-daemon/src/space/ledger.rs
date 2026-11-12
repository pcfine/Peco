//! 台账的读端：进程重启后仍能取回终态任务的正文。
//!
//! 写端是 [`crate::space::tasks::TaskObserver`]，两边对准同一张 `d_dispatches`。
//! `peco-core` 不感知 SQLite，故这里实现它声明的 `TaskLedger`。

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use peco_core::tools::{LedgerOutput, LedgerTask, TaskLedger};
use sqlx::SqlitePool;

/// 台账读端。`task_id` 在库外不可枚举，故查询键只能是工具入参的那两样：
/// 空间目录 + 任务 id。
pub struct SqliteTaskLedger {
    pool: SqlitePool,
}

impl SqliteTaskLedger {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

/// 只回工具能展示的那几列。`output` 最多 100 万字符，故不并进列表查询。
#[derive(sqlx::FromRow)]
struct LedgerRow {
    task_id: String,
    task_state: String,
    output: Option<String>,
    output_truncated: bool,
    result_digest: Option<String>,
    terminal_at: Option<String>,
    dispatch_id: Option<String>,
}

/// `action=output` 只需要正文与它的长度口径，其余列一并搬出来是白付代价。
#[derive(sqlx::FromRow)]
struct OutputRow {
    task_state: String,
    output: Option<String>,
    output_truncated: bool,
    result_digest: Option<String>,
}

impl TaskLedger for SqliteTaskLedger {
    fn find_terminal<'a>(
        &'a self,
        work_dir: &Path,
        task_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<LedgerTask>, String>> + Send + 'a>> {
        let work_dir = work_dir.to_string_lossy().into_owned();
        let task_id = task_id.to_string();
        Box::pin(async move {
            let row = sqlx::query_as::<_, LedgerRow>(
                "SELECT task_id, task_state, output, output_truncated, result_digest, \
                 terminal_at, dispatch_id FROM d_dispatches \
                 WHERE target_work_dir = ? AND task_id = ? AND terminal_at IS NOT NULL \
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(work_dir)
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| e.to_string())?;

            Ok(row.map(|row| LedgerTask {
                task_id: row.task_id,
                state: row.task_state,
                output: row.output,
                output_truncated: row.output_truncated,
                result_digest: row.result_digest,
                terminal_at: row.terminal_at,
                dispatch_id: row.dispatch_id,
            }))
        })
    }

    fn find_output<'a>(
        &'a self,
        work_dir: &Path,
        task_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<LedgerOutput>, String>> + Send + 'a>> {
        let work_dir = work_dir.to_string_lossy().into_owned();
        let task_id = task_id.to_string();
        Box::pin(async move {
            let row = sqlx::query_as::<_, OutputRow>(
                "SELECT task_state, output, output_truncated, result_digest FROM d_dispatches \
                 WHERE target_work_dir = ? AND task_id = ? AND terminal_at IS NOT NULL \
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(work_dir)
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| e.to_string())?;

            Ok(row.map(|row| LedgerOutput {
                state: row.task_state,
                output: row.output,
                output_truncated: row.output_truncated,
                result_digest: row.result_digest,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/ledger.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    async fn seed(pool: &SqlitePool, task_id: &str, dir: &str, state: &str, terminal: bool) {
        sqlx::query(
            "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, \
             target_team_id, origin_ws_id, agent_name, origin_kind, task_state, wake_state, \
             output, output_truncated, result_digest, terminal_at) \
             VALUES (?, ?, 'ws_a', ?, 'research', 'ws_peco', '@master', \
             'dispatch_to_workspace', ?, 'skipped', '正文', 1, '摘要', \
             CASE WHEN ? THEN datetime('now') END)",
        )
        .bind(format!("d_{task_id}"))
        .bind(task_id)
        .bind(dir)
        .bind(state)
        .bind(terminal)
        .execute(pool)
        .await
        .unwrap();
    }

    /// 正文取法同样只认终态行，且把截断标志一起带出来：读者要知道手上这份是不是原文。
    #[tokio::test]
    async fn the_body_query_returns_only_terminal_rows() {
        let (pool, _dir) = test_pool().await;
        seed(&pool, "t_done", "/tmp/a", "failed", true).await;
        seed(&pool, "t_running", "/tmp/a", "working", false).await;
        let ledger = SqliteTaskLedger::new(pool);

        let row = ledger
            .find_output(Path::new("/tmp/a"), "t_done")
            .await
            .unwrap()
            .expect("终态行必须能取正文");
        assert_eq!(row.state, "failed");
        assert_eq!(row.output.as_deref(), Some("正文"));
        assert!(row.output_truncated);
        assert_eq!(row.result_digest.as_deref(), Some("摘要"));

        assert!(
            ledger
                .find_output(Path::new("/tmp/a"), "t_running")
                .await
                .unwrap()
                .is_none(),
            "在飞的行不得回退"
        );
    }

    /// 只回终态行，且按 `(work_dir, task_id)` 定位 —— 这两条一起决定「会不会把
    /// 本进程没有句柄的任务谎报成有」。
    ///
    /// 变异点：去掉 `terminal_at IS NOT NULL` ⇒ 在飞那条从 `None` 变成有行；
    /// 去掉 `target_work_dir` 条件 ⇒ 别的目录那条被串到本空间。
    #[tokio::test]
    async fn only_terminal_rows_of_that_work_dir_are_returned() {
        let (pool, _dir) = test_pool().await;
        seed(&pool, "t_done", "/tmp/a", "completed", true).await;
        seed(&pool, "t_running", "/tmp/a", "working", false).await;
        seed(&pool, "t_elsewhere", "/tmp/b", "completed", true).await;
        let ledger = SqliteTaskLedger::new(pool);

        let row = ledger
            .find_terminal(Path::new("/tmp/a"), "t_done")
            .await
            .unwrap()
            .expect("终态行必须能查到");
        assert_eq!(row.task_id, "t_done");
        assert_eq!(row.state, "completed");
        assert_eq!(row.output.as_deref(), Some("正文"));
        assert!(row.output_truncated, "落库时的截断标志必须带出来");
        assert!(row.terminal_at.is_some());
        assert_eq!(row.dispatch_id, None, "夹具没写派发回执");

        assert!(
            ledger
                .find_terminal(Path::new("/tmp/a"), "t_running")
                .await
                .unwrap()
                .is_none(),
            "在飞的行不得回退"
        );
        assert!(
            ledger
                .find_terminal(Path::new("/tmp/a"), "t_elsewhere")
                .await
                .unwrap()
                .is_none(),
            "别的目录的行不属于这个空间"
        );
    }
}
