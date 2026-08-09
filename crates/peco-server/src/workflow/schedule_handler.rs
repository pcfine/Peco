// ============================================================================
// Schedule API Handlers — 调度配置管理
// ============================================================================

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

use super::schedule::*;
use super::types::SuccessResponse;

/// `GET /api/schedules` — 列出所有调度配置。
pub async fn list_schedules(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ScheduleResponse>>, ApiError> {
    let rows = crate::db::workflow_schedules::list_by_user(&state.db, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("list schedules: {e}")))?;

    let resp: Vec<ScheduleResponse> = rows
        .into_iter()
        .map(|r| ScheduleResponse {
            workflow_name: r.workflow_name,
            cron: r.cron_expr,
            enabled: r.enabled != 0,
            timezone: r.timezone,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect();

    Ok(Json(resp))
}

/// `POST /api/schedules` — 创建调度配置。
pub async fn create_schedule(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateScheduleRequest>,
) -> Result<(StatusCode, Json<ScheduleResponse>), ApiError> {
    // 验证 Workflow 存在
    let ws = state.workspace_manager.get(&user_id)?;
    ws.workflow_manager()
        .load(&body.workflow_name)
        .map_err(|_| ApiError::NotFound(format!("workflow '{}' not found", body.workflow_name)))?;

    // 验证 cron 表达式
    croner::Cron::new(&body.cron)
        .parse()
        .map_err(|e| ApiError::BadRequest(format!("invalid cron expression: {e}")))?;

    // 检查是否已存在调度
    let existing = crate::db::workflow_schedules::find_by_user_and_workflow(
        &state.db,
        &user_id,
        &body.workflow_name,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("check existing schedule: {e}")))?;

    if existing.is_some() {
        return Err(ApiError::Conflict(format!(
            "schedule already exists for workflow '{}'",
            body.workflow_name
        )));
    }

    let schedule_id = Uuid::new_v4().to_string();

    crate::db::workflow_schedules::insert(
        &state.db,
        &crate::db::workflow_schedules::CreateScheduleParams {
            id: schedule_id,
            user_id: user_id.clone(),
            workflow_name: body.workflow_name.clone(),
            cron_expr: body.cron.clone(),
            enabled: body.enabled,
            timezone: body.timezone.clone(),
        },
    )
    .await
    .map_err(|e| ApiError::Internal(format!("insert schedule: {e}")))?;

    // 如果启用，注册到调度器
    if body.enabled {
        let _ = state
            .cron_scheduler
            .add_workflow(
                body.workflow_name.clone(),
                body.cron.clone(),
                body.timezone.clone(),
                user_id.clone(),
                state.db.clone(),
                Arc::clone(&state),
            )
            .await;
    }

    let now = chrono::Utc::now().to_rfc3339();
    Ok((
        StatusCode::CREATED,
        Json(ScheduleResponse {
            workflow_name: body.workflow_name,
            cron: body.cron,
            enabled: body.enabled,
            timezone: body.timezone,
            created_at: now.clone(),
            updated_at: now,
        }),
    ))
}

/// `PUT /api/schedules/:workflow_name` — 完整替换调度。
pub async fn replace_schedule(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(workflow_name): Path<String>,
    Json(body): Json<ReplaceScheduleRequest>,
) -> Result<Json<ScheduleResponse>, ApiError> {
    // 验证 cron 表达式
    croner::Cron::new(&body.cron)
        .parse()
        .map_err(|e| ApiError::BadRequest(format!("invalid cron expression: {e}")))?;

    let updated = crate::db::workflow_schedules::replace(
        &state.db,
        &user_id,
        &workflow_name,
        &body.cron,
        body.enabled,
        body.timezone.as_deref(),
    )
    .await
    .map_err(|e| ApiError::Internal(format!("replace schedule: {e}")))?;

    if !updated {
        return Err(ApiError::NotFound(format!(
            "schedule not found for workflow '{workflow_name}'"
        )));
    }

    // 同步调度器
    let _ = state
        .cron_scheduler
        .remove_workflow(&workflow_name, &user_id)
        .await;
    if body.enabled {
        let _ = state
            .cron_scheduler
            .add_workflow(
                workflow_name.clone(),
                body.cron.clone(),
                body.timezone.clone(),
                user_id.clone(),
                state.db.clone(),
                Arc::clone(&state),
            )
            .await;
    }

    // 重新读取
    let row = crate::db::workflow_schedules::find_by_user_and_workflow(
        &state.db,
        &user_id,
        &workflow_name,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("find schedule: {e}")))?
    .ok_or_else(|| ApiError::NotFound("schedule not found after update".into()))?;

    Ok(Json(ScheduleResponse {
        workflow_name: row.workflow_name,
        cron: row.cron_expr,
        enabled: row.enabled != 0,
        timezone: row.timezone,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

/// `PATCH /api/schedules/:workflow_name` — 部分更新调度。
pub async fn update_schedule(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(workflow_name): Path<String>,
    Json(body): Json<UpdateScheduleRequest>,
) -> Result<Json<ScheduleResponse>, ApiError> {
    let updated = crate::db::workflow_schedules::update(
        &state.db,
        &user_id,
        &workflow_name,
        &crate::db::workflow_schedules::UpdateScheduleParams {
            cron_expr: body.cron,
            enabled: body.enabled,
            timezone: body.timezone,
        },
    )
    .await
    .map_err(|e| ApiError::Internal(format!("update schedule: {e}")))?;

    if !updated {
        return Err(ApiError::NotFound(format!(
            "schedule not found for workflow '{workflow_name}'"
        )));
    }

    // 重新读取并同步调度器
    let row = crate::db::workflow_schedules::find_by_user_and_workflow(
        &state.db,
        &user_id,
        &workflow_name,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("find schedule: {e}")))?
    .ok_or_else(|| ApiError::NotFound("schedule not found after update".into()))?;

    // 同步调度器
    let _ = state
        .cron_scheduler
        .remove_workflow(&workflow_name, &user_id)
        .await;
    if row.enabled != 0 {
        let _ = state
            .cron_scheduler
            .add_workflow(
                workflow_name.clone(),
                row.cron_expr.clone(),
                row.timezone.clone(),
                user_id.clone(),
                state.db.clone(),
                Arc::clone(&state),
            )
            .await;
    }

    Ok(Json(ScheduleResponse {
        workflow_name: row.workflow_name,
        cron: row.cron_expr,
        enabled: row.enabled != 0,
        timezone: row.timezone,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

/// `DELETE /api/schedules/:workflow_name` — 删除调度。
pub async fn delete_schedule(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(workflow_name): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let _ = state
        .cron_scheduler
        .remove_workflow(&workflow_name, &user_id)
        .await;

    crate::db::workflow_schedules::delete(&state.db, &user_id, &workflow_name)
        .await
        .map_err(|e| ApiError::Internal(format!("delete schedule: {e}")))?;

    Ok(Json(SuccessResponse { success: true }))
}
