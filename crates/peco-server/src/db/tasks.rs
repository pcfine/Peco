// ============================================================================
// Task 数据库查询函数
// ============================================================================

use sqlx::SqlitePool;

/// tasks 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct TaskRow {
    pub id: String,
    pub user_id: String,
    pub agent_id: String,
    pub name: String,
    pub cron_expr: String,
    pub prompt: String,
    pub enabled: i64,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 创建 Task 的参数（由 handler 层传入）。
pub struct CreateTaskParams {
    pub id: String,
    pub user_id: String,
    pub agent_id: String,
    pub name: String,
    pub cron_expr: String,
    pub prompt: String,
}

/// 更新 Task 的参数（所有字段可选）。
pub struct UpdateTaskParams {
    pub name: Option<String>,
    pub agent_id: Option<String>,
    pub cron_expr: Option<String>,
    pub prompt: Option<String>,
}

/// 插入新 Task 记录。
pub async fn insert(pool: &SqlitePool, params: &CreateTaskParams) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO tasks (id, user_id, agent_id, name, cron_expr, prompt) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&params.id)
    .bind(&params.user_id)
    .bind(&params.agent_id)
    .bind(&params.name)
    .bind(&params.cron_expr)
    .bind(&params.prompt)
    .execute(pool)
    .await?;
    Ok(())
}

/// 查询用户的 Task 列表。
pub async fn list_by_user(pool: &SqlitePool, user_id: &str) -> Result<Vec<TaskRow>, sqlx::Error> {
    sqlx::query_as::<_, TaskRow>(
        "SELECT id, user_id, agent_id, name, cron_expr, prompt, enabled, last_run_at, next_run_at, created_at, updated_at \
         FROM tasks WHERE user_id = ? ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// 按 ID 查询单个 Task（不校验 user_id，由调用方自行校验）。
pub async fn find_by_id(pool: &SqlitePool, task_id: &str) -> Result<Option<TaskRow>, sqlx::Error> {
    sqlx::query_as::<_, TaskRow>(
        "SELECT id, user_id, agent_id, name, cron_expr, prompt, enabled, last_run_at, next_run_at, created_at, updated_at \
         FROM tasks WHERE id = ?",
    )
    .bind(task_id)
    .fetch_optional(pool)
    .await
}

/// 按 ID 和 user_id 查询单个 Task。
pub async fn find_by_id_and_user(
    pool: &SqlitePool,
    task_id: &str,
    user_id: &str,
) -> Result<Option<TaskRow>, sqlx::Error> {
    sqlx::query_as::<_, TaskRow>(
        "SELECT id, user_id, agent_id, name, cron_expr, prompt, enabled, last_run_at, next_run_at, created_at, updated_at \
         FROM tasks WHERE id = ? AND user_id = ?",
    )
    .bind(task_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// 查询所有已启用的 Task（启动时加载使用）。
pub async fn list_all_enabled(pool: &SqlitePool) -> Result<Vec<TaskRow>, sqlx::Error> {
    sqlx::query_as::<_, TaskRow>(
        "SELECT id, user_id, agent_id, name, cron_expr, prompt, enabled, last_run_at, next_run_at, created_at, updated_at \
         FROM tasks WHERE enabled = 1 ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await
}

/// 更新 Task 的可选字段。
pub async fn update(
    pool: &SqlitePool,
    task_id: &str,
    params: &UpdateTaskParams,
) -> Result<bool, sqlx::Error> {
    let mut sets: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();

    if params.name.is_some() {
        sets.push(format!("name = ?{}", binds.len() + 1));
        binds.push(params.name.clone().unwrap());
    }
    if params.agent_id.is_some() {
        sets.push(format!("agent_id = ?{}", binds.len() + 1));
        binds.push(params.agent_id.clone().unwrap());
    }
    if params.cron_expr.is_some() {
        sets.push(format!("cron_expr = ?{}", binds.len() + 1));
        binds.push(params.cron_expr.clone().unwrap());
    }
    if params.prompt.is_some() {
        sets.push(format!("prompt = ?{}", binds.len() + 1));
        binds.push(params.prompt.clone().unwrap());
    }

    if sets.is_empty() {
        return Ok(false);
    }

    sets.push("updated_at = datetime('now')".to_string());

    let sql = format!(
        "UPDATE tasks SET {} WHERE id = ?{}",
        sets.join(", "),
        binds.len() + 1
    );

    let mut query = sqlx::query(&sql);
    for value in &binds {
        query = query.bind(value);
    }
    query = query.bind(task_id);

    let result = query.execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

/// 更新 Task 的启用/禁用状态。
pub async fn update_enabled(
    pool: &SqlitePool,
    task_id: &str,
    enabled: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE tasks SET enabled = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(enabled as i64)
        .bind(task_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 更新 Task 的运行时间戳。
pub async fn update_run_timestamps(
    pool: &SqlitePool,
    task_id: &str,
    last_run_at: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE tasks SET last_run_at = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(last_run_at)
        .bind(task_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 删除 Task。
pub async fn delete(pool: &SqlitePool, task_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM tasks WHERE id = ?")
        .bind(task_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
