// ============================================================================
// workflow_executions 表 DAO
// ============================================================================

use sqlx::SqlitePool;

/// workflow_executions 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct WorkflowExecutionRow {
    pub id: String,
    pub user_id: String,
    pub workflow_name: String,
    pub trigger_type: String,
    pub status: String,
    pub inputs_json: Option<String>,
    pub total_steps: i64,
    pub steps_completed: i64,
    pub steps_failed: i64,
    pub steps_skipped: i64,
    pub total_duration_ms: Option<i64>,
    pub error: Option<String>,
    pub snapshot_json: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub created_at: String,
}

/// 创建执行记录的参数。
pub struct CreateExecutionParams {
    pub id: String,
    pub user_id: String,
    pub workflow_name: String,
    pub trigger_type: String,
    pub inputs_json: Option<String>,
    pub total_steps: usize,
    pub started_at: String,
}

/// 插入新执行记录。
pub async fn insert(pool: &SqlitePool, params: &CreateExecutionParams) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO workflow_executions (id, user_id, workflow_name, trigger_type, \
         inputs_json, total_steps, started_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&params.id)
    .bind(&params.user_id)
    .bind(&params.workflow_name)
    .bind(&params.trigger_type)
    .bind(&params.inputs_json)
    .bind(params.total_steps as i64)
    .bind(&params.started_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// 查询用户的执行记录列表（分页）。
pub async fn list_by_user(
    pool: &SqlitePool,
    user_id: &str,
    offset: i64,
    limit: i64,
) -> Result<Vec<WorkflowExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkflowExecutionRow>(
        "SELECT id, user_id, workflow_name, trigger_type, status, inputs_json, \
         total_steps, steps_completed, steps_failed, steps_skipped, \
         total_duration_ms, error, snapshot_json, started_at, finished_at, created_at \
         FROM workflow_executions WHERE user_id = ? \
         ORDER BY started_at DESC LIMIT ? OFFSET ?",
    )
    .bind(user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// 按用户+workflow 名称筛选的执行记录列表。
pub async fn list_by_user_and_workflow(
    pool: &SqlitePool,
    user_id: &str,
    workflow_name: &str,
    offset: i64,
    limit: i64,
) -> Result<Vec<WorkflowExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkflowExecutionRow>(
        "SELECT id, user_id, workflow_name, trigger_type, status, inputs_json, \
         total_steps, steps_completed, steps_failed, steps_skipped, \
         total_duration_ms, error, snapshot_json, started_at, finished_at, created_at \
         FROM workflow_executions WHERE user_id = ? AND workflow_name = ? \
         ORDER BY started_at DESC LIMIT ? OFFSET ?",
    )
    .bind(user_id)
    .bind(workflow_name)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// 带筛选条件的执行记录列表。
pub async fn list_by_user_with_filters(
    pool: &SqlitePool,
    user_id: &str,
    workflow_name: Option<&str>,
    status: Option<&str>,
    trigger_type: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<WorkflowExecutionRow>, sqlx::Error> {
    let mut query_str = String::from(
        "SELECT id, user_id, workflow_name, trigger_type, status, inputs_json, \
         total_steps, steps_completed, steps_failed, steps_skipped, \
         total_duration_ms, error, snapshot_json, started_at, finished_at, created_at \
         FROM workflow_executions WHERE user_id = ?",
    );
    let mut params: Vec<String> = vec![user_id.to_string()];

    if let Some(wn) = workflow_name {
        params.push(wn.to_string());
        query_str.push_str(&format!(" AND workflow_name = ?{}", params.len()));
    }
    if let Some(s) = status {
        // 支持逗号分隔的多状态筛选
        let statuses: Vec<&str> = s.split(',').map(|s| s.trim()).collect();
        if statuses.len() == 1 {
            params.push(statuses[0].to_string());
            query_str.push_str(&format!(" AND status = ?{}", params.len()));
        } else {
            let placeholders: Vec<String> = statuses
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", params.len() + i + 1))
                .collect();
            for st in &statuses {
                params.push(st.to_string());
            }
            query_str.push_str(&format!(" AND status IN ({})", placeholders.join(", ")));
        }
    }
    if let Some(tt) = trigger_type {
        params.push(tt.to_string());
        query_str.push_str(&format!(" AND trigger_type = ?{}", params.len()));
    }

    query_str.push_str(&format!(
        " ORDER BY started_at DESC LIMIT ?{} OFFSET ?{}",
        params.len() + 1,
        params.len() + 2
    ));

    let mut query = sqlx::query_as::<_, WorkflowExecutionRow>(&query_str);
    for value in &params {
        query = query.bind(value);
    }
    query = query.bind(limit).bind(offset);

    query.fetch_all(pool).await
}

/// 按 run_id 查询单条记录。
pub async fn find_by_id(
    pool: &SqlitePool,
    run_id: &str,
) -> Result<Option<WorkflowExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkflowExecutionRow>(
        "SELECT id, user_id, workflow_name, trigger_type, status, inputs_json, \
         total_steps, steps_completed, steps_failed, steps_skipped, \
         total_duration_ms, error, snapshot_json, started_at, finished_at, created_at \
         FROM workflow_executions WHERE id = ?",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
}

/// 按 run_id 和 user_id 查询（归属校验）。
pub async fn find_by_id_and_user(
    pool: &SqlitePool,
    run_id: &str,
    user_id: &str,
) -> Result<Option<WorkflowExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkflowExecutionRow>(
        "SELECT id, user_id, workflow_name, trigger_type, status, inputs_json, \
         total_steps, steps_completed, steps_failed, steps_skipped, \
         total_duration_ms, error, snapshot_json, started_at, finished_at, created_at \
         FROM workflow_executions WHERE id = ? AND user_id = ?",
    )
    .bind(run_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// 更新执行记录的状态（完成/失败时调用）。
pub async fn update_status(
    pool: &SqlitePool,
    run_id: &str,
    status: &str,
    error: Option<&str>,
    total_duration_ms: i64,
    snapshot_json: &str,
    finished_at: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE workflow_executions \
         SET status = ?, error = ?, total_duration_ms = ?, snapshot_json = ?, finished_at = ? \
         WHERE id = ?",
    )
    .bind(status)
    .bind(error)
    .bind(total_duration_ms)
    .bind(snapshot_json)
    .bind(finished_at)
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// 更新执行进度（每层完成后调用）。
pub async fn update_progress(
    pool: &SqlitePool,
    run_id: &str,
    steps_completed: i64,
    steps_failed: i64,
    steps_skipped: i64,
    snapshot_json: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE workflow_executions \
         SET steps_completed = ?, steps_failed = ?, steps_skipped = ?, snapshot_json = ? \
         WHERE id = ?",
    )
    .bind(steps_completed)
    .bind(steps_failed)
    .bind(steps_skipped)
    .bind(snapshot_json)
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// 清理僵尸执行记录（服务器重启时调用）。
/// 将所有 running/paused 状态的记录标记为 failed。
pub async fn mark_zombies_failed(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE workflow_executions \
         SET status = 'failed', \
             error = 'Server restarted during execution', \
             finished_at = datetime('now') \
         WHERE status IN ('running', 'paused')",
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 统计用户的执行记录总数。
pub async fn count_by_user(pool: &SqlitePool, user_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM workflow_executions WHERE user_id = ?")
        .bind(user_id)
        .fetch_one(pool)
        .await
}

/// 带筛选条件的执行记录计数（与 list_by_user_with_filters 筛选逻辑一致）。
pub async fn count_by_user_with_filters(
    pool: &SqlitePool,
    user_id: &str,
    workflow_name: Option<&str>,
    status: Option<&str>,
    trigger_type: Option<&str>,
) -> Result<i64, sqlx::Error> {
    let mut query_str = String::from("SELECT COUNT(*) FROM workflow_executions WHERE user_id = ?");
    let mut params: Vec<String> = vec![user_id.to_string()];

    if let Some(wn) = workflow_name {
        params.push(wn.to_string());
        query_str.push_str(&format!(" AND workflow_name = ?{}", params.len()));
    }
    if let Some(s) = status {
        let statuses: Vec<&str> = s.split(',').map(|s| s.trim()).collect();
        if statuses.len() == 1 {
            params.push(statuses[0].to_string());
            query_str.push_str(&format!(" AND status = ?{}", params.len()));
        } else {
            let placeholders: Vec<String> = statuses
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", params.len() + i + 1))
                .collect();
            for st in &statuses {
                params.push(st.to_string());
            }
            query_str.push_str(&format!(" AND status IN ({})", placeholders.join(", ")));
        }
    }
    if let Some(tt) = trigger_type {
        params.push(tt.to_string());
        query_str.push_str(&format!(" AND trigger_type = ?{}", params.len()));
    }

    let mut query = sqlx::query_scalar::<_, i64>(&query_str);
    for value in &params {
        query = query.bind(value);
    }

    query.fetch_one(pool).await
}

/// 查询用户每个 workflow 的最近一次执行记录（每个 workflow 一行）。
///
/// 使用 GROUP BY + MAX(started_at) 子查询，相比拉取全量再分组更高效。
pub async fn latest_per_workflow(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<WorkflowExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkflowExecutionRow>(
        "SELECT w.id, w.user_id, w.workflow_name, w.trigger_type, w.status, w.inputs_json, \
         w.total_steps, w.steps_completed, w.steps_failed, w.steps_skipped, \
         w.total_duration_ms, w.error, w.snapshot_json, w.started_at, w.finished_at, w.created_at \
         FROM workflow_executions w \
         INNER JOIN ( \
           SELECT workflow_name, MAX(started_at) AS max_started \
           FROM workflow_executions WHERE user_id = ? GROUP BY workflow_name \
         ) latest ON w.workflow_name = latest.workflow_name AND w.started_at = latest.max_started \
         WHERE w.user_id = ?",
    )
    .bind(user_id)
    .bind(user_id)
    .fetch_all(pool)
    .await
}
