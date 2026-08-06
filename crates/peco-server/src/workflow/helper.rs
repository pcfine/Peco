// ============================================================================
// Handler 共享辅助函数
// ============================================================================

use std::collections::HashMap;

use sqlx::SqlitePool;

/// 获取用户的所有调度配置，返回 workflow_name → ScheduleInfo 映射。
pub async fn get_user_schedules(
    pool: &SqlitePool,
    user_id: &str,
) -> HashMap<String, crate::workflow::schedule::ScheduleConfig> {
    let rows = match crate::db::workflow_schedules::list_by_user(pool, user_id).await {
        Ok(rows) => rows,
        Err(_) => return HashMap::new(),
    };

    rows.into_iter()
        .map(|r| {
            (
                r.workflow_name.clone(),
                crate::workflow::schedule::ScheduleConfig {
                    workflow_name: r.workflow_name,
                    cron: r.cron_expr,
                    enabled: r.enabled != 0,
                    timezone: r.timezone,
                    user_id: r.user_id,
                    created_at: r.created_at,
                    updated_at: r.updated_at,
                },
            )
        })
        .collect()
}

/// 获取用户每个 workflow 的最近一次执行，返回 workflow_name → ExecutionSummary.
pub async fn get_recent_executions(
    pool: &SqlitePool,
    user_id: &str,
) -> HashMap<String, super::types::ExecutionSummary> {
    use crate::db::workflow_executions;

    // 使用 MAX(started_at) 子查询：每个 workflow 只返回最近一条，无上限截断
    let rows = match workflow_executions::latest_per_workflow(pool, user_id).await {
        Ok(rows) => rows,
        Err(_) => return HashMap::new(),
    };

    rows.into_iter()
        .map(|r| {
            (
                r.workflow_name.clone(),
                super::types::ExecutionSummary {
                    run_id: r.id,
                    workflow_name: r.workflow_name,
                    trigger_type: r.trigger_type,
                    status: r.status,
                    total_steps: r.total_steps as usize,
                    steps_completed: r.steps_completed as usize,
                    steps_failed: r.steps_failed as usize,
                    steps_skipped: r.steps_skipped as usize,
                    total_duration_ms: r.total_duration_ms,
                    started_at: r.started_at,
                    finished_at: r.finished_at,
                },
            )
        })
        .collect()
}
