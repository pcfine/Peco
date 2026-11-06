//! `/api/tasks` —— 跨空间派发台账的只读查询。
//!
//! 台账由 `space::tasks::TaskObserver` 写入；HTTP 只做筛选、投影与 404。
//! ws_id 与目录的映射始终来自 `workspaces` 行，不维护第二份缓存。

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use serde::{Deserialize, Serialize};

use crate::db::dispatches::{self, DispatchFilter, DispatchRow};
use crate::db::workspaces;
use crate::error::DaemonError;
use crate::state::AppState;

/// UI 需要的台账视图；`wake_state` 属于 B 类唤醒内部字段，不对外暴露。
#[derive(Debug, Serialize)]
pub struct TaskView {
    pub id: String,
    pub task_id: String,
    pub target_ws_id: String,
    pub target_work_dir: String,
    pub target_team_id: String,
    pub origin_ws_id: String,
    pub origin_kind: String,
    pub agent_name: String,
    pub state: String,
    pub created_at: String,
    pub terminal_at: Option<String>,
    pub result_digest: Option<String>,
}

impl From<DispatchRow> for TaskView {
    fn from(row: DispatchRow) -> Self {
        Self {
            id: row.id,
            task_id: row.task_id,
            target_ws_id: row.target_ws_id,
            target_work_dir: row.target_work_dir,
            target_team_id: row.target_team_id,
            origin_ws_id: row.origin_ws_id,
            origin_kind: row.origin_kind,
            agent_name: row.agent_name,
            state: row.state,
            created_at: row.created_at,
            terminal_at: row.terminal_at,
            result_digest: row.result_digest,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub target_ws_id: Option<String>,
    pub origin_ws_id: Option<String>,
    pub state: Option<String>,
    pub limit: Option<i64>,
}

/// `GET /api/tasks?target_ws_id=&origin_ws_id=&state=&limit=`
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<TaskView>>, DaemonError> {
    let rows = dispatches::list(
        &state.pool,
        DispatchFilter {
            target_ws_id: query.target_ws_id.as_deref(),
            origin_ws_id: query.origin_ws_id.as_deref(),
            state: query.state.as_deref(),
            limit: query.limit,
        },
    )
    .await?;
    Ok(Json(rows.into_iter().map(TaskView::from).collect()))
}

/// `GET /api/spaces/{ws_id}/tasks` —— 该空间作为 target 的台账。
pub async fn list_for_space(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
) -> Result<Json<Vec<TaskView>>, DaemonError> {
    if workspaces::by_id(&state.pool, &ws_id).await?.is_none() {
        return Err(DaemonError::SpaceNotFound { ws_id });
    }
    let rows = dispatches::list(
        &state.pool,
        DispatchFilter {
            target_ws_id: Some(ws_id.as_str()),
            limit: Some(200),
            ..DispatchFilter::default()
        },
    )
    .await?;
    Ok(Json(rows.into_iter().map(TaskView::from).collect()))
}

/// `GET /api/tasks/{task_id}` —— task_id 是 Hub 发放的 UUID，全局唯一。
pub async fn detail(
    State(state): State<AppState>,
    UrlPath(task_id): UrlPath<String>,
) -> Result<Json<TaskView>, DaemonError> {
    dispatches::get_by_task_id(&state.pool, &task_id)
        .await?
        .map(TaskView::from)
        .map(Json)
        .ok_or(DaemonError::TaskNotFound { task_id })
}
