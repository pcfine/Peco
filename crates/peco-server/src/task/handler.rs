// ============================================================================
// Task Handlers — CRUD + Toggle + Logs
// ============================================================================

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::db::tasks::{self, CreateTaskParams, UpdateTaskParams};
use crate::error::ApiError;
use crate::state::AppState;

// ── Request / Response 类型 ─────────────────────────────────────────────────

/// 创建 Task 的请求体。
#[derive(Debug, Deserialize)]
pub struct CreateTaskRequest {
    pub agent_id: String,
    pub name: String,
    pub cron_expr: String,
    pub prompt: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// 更新 Task 的请求体（全部字段可选）。
#[derive(Debug, Deserialize)]
pub struct UpdateTaskRequest {
    pub name: Option<String>,
    pub agent_id: Option<String>,
    pub cron_expr: Option<String>,
    pub prompt: Option<String>,
}

/// Task 响应体。
#[derive(Debug, Serialize)]
pub struct TaskResponse {
    pub id: String,
    pub name: String,
    pub agent_id: String,
    pub agent_name: Option<String>,
    pub cron_expr: String,
    pub prompt: String,
    pub enabled: bool,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Toggle 响应体。
#[derive(Debug, Serialize)]
pub struct TaskToggleResponse {
    pub id: String,
    pub enabled: bool,
}

/// 成功响应体。
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
}

/// Task 日志响应体。
#[derive(Debug, Serialize)]
pub struct TaskLogResponse {
    pub id: String,
    pub task_id: String,
    pub status: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// 日志列表查询参数。
#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    #[serde(default = "default_offset")]
    pub offset: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_offset() -> i64 {
    0
}
fn default_limit() -> i64 {
    50
}

// ── 辅助函数 ────────────────────────────────────────────────────────────────

/// 将 DB 行转为响应体（包含 agent_name）。
async fn row_to_response(pool: &sqlx::SqlitePool, row: &tasks::TaskRow) -> TaskResponse {
    let agent_name = crate::db::agents::find_name_by_id(pool, &row.agent_id)
        .await
        .ok()
        .flatten();

    TaskResponse {
        id: row.id.clone(),
        name: row.name.clone(),
        agent_id: row.agent_id.clone(),
        agent_name,
        cron_expr: row.cron_expr.clone(),
        prompt: row.prompt.clone(),
        enabled: row.enabled != 0,
        last_run_at: row.last_run_at.clone(),
        next_run_at: row.next_run_at.clone(),
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    }
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `GET /api/tasks` — 获取当前用户的 Task 列表。
pub async fn list_tasks(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<TaskResponse>>, ApiError> {
    let rows = tasks::list_by_user(&state.db, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("db error: {e}")))?;

    let mut result = Vec::with_capacity(rows.len());
    for row in &rows {
        result.push(row_to_response(&state.db, row).await);
    }

    Ok(Json(result))
}

/// `POST /api/tasks` — 创建新 Task。
pub async fn create_task(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateTaskRequest>,
) -> Result<(StatusCode, Json<TaskResponse>), ApiError> {
    // 校验 agent 存在且属于当前用户
    let agent = crate::db::agents::find_index_by_id_and_user(&state.db, &body.agent_id, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("db error: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("agent '{}' not found", body.agent_id)))?;

    // 校验 cron 表达式
    croner::Cron::new(&body.cron_expr)
        .parse()
        .map_err(|e| ApiError::BadRequest(format!("invalid cron expression: {e}")))?;

    let task_id = Uuid::new_v4().to_string();

    // 写入数据库
    tasks::insert(
        &state.db,
        &CreateTaskParams {
            id: task_id.clone(),
            user_id: user_id.clone(),
            agent_id: body.agent_id.clone(),
            name: body.name.clone(),
            cron_expr: body.cron_expr.clone(),
            prompt: body.prompt.clone(),
        },
    )
    .await
    .map_err(|e| ApiError::Internal(format!("db error: {e}")))?;

    // 如果 enabled，注册到调度器
    if body.enabled {
        // ★ 忽略调度器注册错误（不影响请求返回），仅记录日志
        if let Err(e) = state
            .task_scheduler
            .add_task(
                task_id.clone(),
                body.cron_expr.clone(),
                body.agent_id.clone(),
                user_id.clone(),
                body.prompt.clone(),
                state.db.clone(),
                Arc::clone(&state),
            )
            .await
        {
            tracing::error!(task_id = %task_id, error = %e, "Failed to register task with scheduler");
        } else {
            // 更新 next_run_at（cron 调度器会自动管理，此处不强制设置）
            tracing::info!(task_id = %task_id, "Task registered with scheduler");
        }
    }

    let task = tasks::find_by_id(&state.db, &task_id)
        .await
        .map_err(|e| ApiError::Internal(format!("db error: {e}")))?
        .ok_or_else(|| ApiError::Internal("task not found after insert".into()))?;

    let response = TaskResponse {
        id: task.id,
        name: task.name,
        agent_id: task.agent_id,
        agent_name: Some(agent.name),
        cron_expr: task.cron_expr,
        prompt: task.prompt,
        enabled: task.enabled != 0,
        last_run_at: task.last_run_at,
        next_run_at: task.next_run_at,
        created_at: task.created_at,
        updated_at: task.updated_at,
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// `PATCH /api/tasks/:id` — 更新 Task 配置。
pub async fn update_task(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
    Json(body): Json<UpdateTaskRequest>,
) -> Result<Json<TaskResponse>, ApiError> {
    // 校验 task 存在且属于当前用户
    let old_task = tasks::find_by_id_and_user(&state.db, &task_id, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("db error: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("task '{task_id}' not found")))?;

    // 校验新的 agent_id（如果提供）
    if let Some(ref agent_id) = body.agent_id {
        crate::db::agents::find_index_by_id_and_user(&state.db, agent_id, &user_id)
            .await
            .map_err(|e| ApiError::Internal(format!("db error: {e}")))?
            .ok_or_else(|| ApiError::NotFound(format!("agent '{agent_id}' not found")))?;
    }

    // 校验新的 cron 表达式（如果提供）
    if let Some(ref cron_expr) = body.cron_expr {
        croner::Cron::new(cron_expr)
            .parse()
            .map_err(|e| ApiError::BadRequest(format!("invalid cron expression: {e}")))?;
    }

    // 更新数据库
    let updated = tasks::update(
        &state.db,
        &task_id,
        &UpdateTaskParams {
            name: body.name.clone(),
            agent_id: body.agent_id.clone(),
            cron_expr: body.cron_expr.clone(),
            prompt: body.prompt.clone(),
        },
    )
    .await
    .map_err(|e| ApiError::Internal(format!("db error: {e}")))?;

    if !updated {
        return Err(ApiError::NotFound(format!("task '{task_id}' not found")));
    }

    // 如果当前 enabled 且 cron_expr 变更，重新调度
    let is_enabled = old_task.enabled != 0;
    let cron_changed = body
        .cron_expr
        .as_ref()
        .is_some_and(|c| *c != old_task.cron_expr);

    if is_enabled && cron_changed {
        let effective_agent_id = body.agent_id.as_ref().unwrap_or(&old_task.agent_id);
        let effective_prompt = body.prompt.as_ref().unwrap_or(&old_task.prompt);

        if let Err(e) = state
            .task_scheduler
            .reschedule(
                task_id.clone(),
                body.cron_expr.clone().unwrap(),
                effective_agent_id.clone(),
                user_id.clone(),
                effective_prompt.clone(),
                state.db.clone(),
                Arc::clone(&state),
            )
            .await
        {
            tracing::error!(task_id = %task_id, error = %e, "Failed to reschedule task");
        }
    }

    let task = tasks::find_by_id(&state.db, &task_id)
        .await
        .map_err(|e| ApiError::Internal(format!("db error: {e}")))?
        .ok_or_else(|| ApiError::Internal("task not found after update".into()))?;

    let response = row_to_response(&state.db, &task).await;
    Ok(Json(response))
}

/// `DELETE /api/tasks/:id` — 删除 Task。
pub async fn delete_task(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    // 校验 task 存在且属于当前用户
    let task = tasks::find_by_id_and_user(&state.db, &task_id, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("db error: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("task '{task_id}' not found")))?;

    // 从调度器移除
    if task.enabled != 0
        && let Err(e) = state.task_scheduler.remove_task(&task_id).await
    {
        tracing::error!(task_id = %task_id, error = %e, "Failed to unschedule task");
    }

    // 删除数据库记录（CASCADE 自动删除 task_logs）
    let deleted = tasks::delete(&state.db, &task_id)
        .await
        .map_err(|e| ApiError::Internal(format!("db error: {e}")))?;

    if !deleted {
        return Err(ApiError::NotFound(format!("task '{task_id}' not found")));
    }

    Ok(Json(SuccessResponse { success: true }))
}

/// `POST /api/tasks/:id/toggle` — 切换启用/禁用。
pub async fn toggle_task(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> Result<Json<TaskToggleResponse>, ApiError> {
    // 校验 task 存在且属于当前用户
    let task = tasks::find_by_id_and_user(&state.db, &task_id, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("db error: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("task '{task_id}' not found")))?;

    let currently_enabled = task.enabled != 0;
    let new_enabled = !currently_enabled;

    // 更新数据库
    tasks::update_enabled(&state.db, &task_id, new_enabled)
        .await
        .map_err(|e| ApiError::Internal(format!("db error: {e}")))?;

    if new_enabled {
        // 重新注册到调度器
        if let Err(e) = state
            .task_scheduler
            .add_task(
                task_id.clone(),
                task.cron_expr.clone(),
                task.agent_id.clone(),
                user_id.clone(),
                task.prompt.clone(),
                state.db.clone(),
                Arc::clone(&state),
            )
            .await
        {
            tracing::error!(task_id = %task_id, error = %e, "Failed to re-register task after toggle");
        }
    } else {
        // 从调度器移除
        if let Err(e) = state.task_scheduler.remove_task(&task_id).await {
            tracing::error!(task_id = %task_id, error = %e, "Failed to unschedule task after toggle");
        }
    }

    Ok(Json(TaskToggleResponse {
        id: task_id,
        enabled: new_enabled,
    }))
}

/// `GET /api/tasks/:id/logs` — 获取执行日志。
pub async fn get_task_logs(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<Vec<TaskLogResponse>>, ApiError> {
    // 校验 task 存在且属于当前用户
    tasks::find_by_id_and_user(&state.db, &task_id, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("db error: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("task '{task_id}' not found")))?;

    let rows = crate::db::task_logs::list_by_task(&state.db, &task_id, query.offset, query.limit)
        .await
        .map_err(|e| ApiError::Internal(format!("db error: {e}")))?;

    let logs: Vec<TaskLogResponse> = rows
        .into_iter()
        .map(|row| TaskLogResponse {
            id: row.id,
            task_id: row.task_id,
            status: row.status,
            output: row.output,
            error: row.error,
            started_at: row.started_at,
            finished_at: row.finished_at,
        })
        .collect();

    Ok(Json(logs))
}
