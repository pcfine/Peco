// ============================================================================
// TaskLog 数据库查询函数
// ============================================================================

use sqlx::SqlitePool;

/// task_logs 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct TaskLogRow {
    pub id: String,
    pub task_id: String,
    pub status: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// 创建日志的参数。
pub struct CreateLogParams<'a> {
    pub id: &'a str,
    pub task_id: &'a str,
    pub status: &'a str,
    pub output: &'a str,
    pub error: &'a str,
    pub started_at: &'a str,
    pub finished_at: Option<&'a str>,
}

/// 插入新日志记录。
pub async fn insert(pool: &SqlitePool, params: &CreateLogParams<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO task_logs (id, task_id, status, output, error, started_at, finished_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(params.id)
    .bind(params.task_id)
    .bind(params.status)
    .bind(params.output)
    .bind(params.error)
    .bind(params.started_at)
    .bind(params.finished_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// 更新日志状态（任务执行完成后调用）。
pub async fn update_status(
    pool: &SqlitePool,
    log_id: &str,
    status: &str,
    output: &str,
    error: &str,
    finished_at: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE task_logs SET status = ?, output = ?, error = ?, finished_at = ? WHERE id = ?",
    )
    .bind(status)
    .bind(output)
    .bind(error)
    .bind(finished_at)
    .bind(log_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// 查询指定 Task 的执行日志列表（按时间倒序）。
pub async fn list_by_task(
    pool: &SqlitePool,
    task_id: &str,
    offset: i64,
    limit: i64,
) -> Result<Vec<TaskLogRow>, sqlx::Error> {
    sqlx::query_as::<_, TaskLogRow>(
        "SELECT id, task_id, status, output, error, started_at, finished_at \
         FROM task_logs WHERE task_id = ? \
         ORDER BY started_at DESC LIMIT ? OFFSET ?",
    )
    .bind(task_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}
