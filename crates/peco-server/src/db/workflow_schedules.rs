// ============================================================================
// workflow_schedules 表 DAO
// ============================================================================

use sqlx::SqlitePool;

/// workflow_schedules 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct WorkflowScheduleRow {
    pub id: String,
    pub user_id: String,
    pub workflow_name: String,
    pub cron_expr: String,
    pub enabled: i64,
    pub timezone: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 创建调度配置的参数。
pub struct CreateScheduleParams {
    pub id: String,
    pub user_id: String,
    pub workflow_name: String,
    pub cron_expr: String,
    pub enabled: bool,
    pub timezone: Option<String>,
}

/// 更新调度配置的参数（全部可选，用于 PATCH）。
pub struct UpdateScheduleParams {
    pub cron_expr: Option<String>,
    pub enabled: Option<bool>,
    pub timezone: Option<String>,
}

/// 插入新调度记录。
pub async fn insert(pool: &SqlitePool, params: &CreateScheduleParams) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO workflow_schedules (id, user_id, workflow_name, cron_expr, enabled, timezone) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&params.id)
    .bind(&params.user_id)
    .bind(&params.workflow_name)
    .bind(&params.cron_expr)
    .bind(params.enabled as i64)
    .bind(&params.timezone)
    .execute(pool)
    .await?;
    Ok(())
}

/// 查询用户的所有调度配置。
pub async fn list_by_user(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<WorkflowScheduleRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkflowScheduleRow>(
        "SELECT id, user_id, workflow_name, cron_expr, enabled, timezone, created_at, updated_at \
         FROM workflow_schedules WHERE user_id = ? ORDER BY workflow_name ASC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// 查询所有已启用的调度配置（服务器启动时加载）。
pub async fn list_all_enabled(pool: &SqlitePool) -> Result<Vec<WorkflowScheduleRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkflowScheduleRow>(
        "SELECT id, user_id, workflow_name, cron_expr, enabled, timezone, created_at, updated_at \
         FROM workflow_schedules WHERE enabled = 1 ORDER BY workflow_name ASC",
    )
    .fetch_all(pool)
    .await
}

/// 按用户+workflow 名称查询单条调度。
pub async fn find_by_user_and_workflow(
    pool: &SqlitePool,
    user_id: &str,
    workflow_name: &str,
) -> Result<Option<WorkflowScheduleRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkflowScheduleRow>(
        "SELECT id, user_id, workflow_name, cron_expr, enabled, timezone, created_at, updated_at \
         FROM workflow_schedules WHERE user_id = ? AND workflow_name = ?",
    )
    .bind(user_id)
    .bind(workflow_name)
    .fetch_optional(pool)
    .await
}

/// 部分更新调度配置。
pub async fn update(
    pool: &SqlitePool,
    user_id: &str,
    workflow_name: &str,
    params: &UpdateScheduleParams,
) -> Result<bool, sqlx::Error> {
    let mut sets: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();

    if let Some(ref cron) = params.cron_expr {
        sets.push(format!("cron_expr = ?{}", binds.len() + 1));
        binds.push(cron.clone());
    }
    if let Some(enabled) = params.enabled {
        sets.push(format!("enabled = ?{}", binds.len() + 1));
        binds.push(if enabled { "1".into() } else { "0".into() });
    }
    if let Some(ref tz) = params.timezone {
        sets.push(format!("timezone = ?{}", binds.len() + 1));
        binds.push(tz.clone());
    }

    if sets.is_empty() {
        return Ok(false);
    }

    sets.push("updated_at = datetime('now')".to_string());

    let sql = format!(
        "UPDATE workflow_schedules SET {} WHERE user_id = ?{} AND workflow_name = ?{}",
        sets.join(", "),
        binds.len() + 1,
        binds.len() + 2
    );

    let mut query = sqlx::query(&sql);
    for value in &binds {
        query = query.bind(value);
    }
    query = query.bind(user_id).bind(workflow_name);

    let result = query.execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

/// 完整替换调度配置（PUT）。
pub async fn replace(
    pool: &SqlitePool,
    user_id: &str,
    workflow_name: &str,
    cron_expr: &str,
    enabled: bool,
    timezone: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE workflow_schedules \
         SET cron_expr = ?, enabled = ?, timezone = ?, updated_at = datetime('now') \
         WHERE user_id = ? AND workflow_name = ?",
    )
    .bind(cron_expr)
    .bind(enabled as i64)
    .bind(timezone)
    .bind(user_id)
    .bind(workflow_name)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// 删除调度配置。
pub async fn delete(
    pool: &SqlitePool,
    user_id: &str,
    workflow_name: &str,
) -> Result<bool, sqlx::Error> {
    let result =
        sqlx::query("DELETE FROM workflow_schedules WHERE user_id = ? AND workflow_name = ?")
            .bind(user_id)
            .bind(workflow_name)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() > 0)
}
