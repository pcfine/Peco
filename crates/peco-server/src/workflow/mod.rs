// ============================================================================
// Workflow 管理模块 — peco-server 侧
// ============================================================================

pub mod active;
mod handler;
mod helper;
pub mod persister;
pub mod schedule;
mod schedule_handler;
pub mod scheduler;
mod types;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post, put};

use crate::state::AppState;

/// 组装 `/api/workflows` 路由组。
pub fn workflow_router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/",
            get(handler::list_workflows).post(handler::create_workflow),
        )
        .route(
            "/{name}",
            get(handler::get_workflow)
                .put(handler::update_workflow)
                .delete(handler::delete_workflow),
        )
        .route("/{name}/execute", post(handler::execute_workflow))
        .route("/{name}/statistics", get(handler::get_statistics))
        .route("/executions", get(handler::list_executions))
        .route("/executions/{run_id}", get(handler::get_execution))
        .route(
            "/executions/{run_id}/cancel",
            post(handler::cancel_execution),
        )
        .route(
            "/executions/{run_id}/approve",
            post(handler::approve_execution),
        )
}

/// 组装 `/api/schedules` 路由组。
pub fn schedule_router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/",
            get(schedule_handler::list_schedules).post(schedule_handler::create_schedule),
        )
        .route(
            "/{workflow_name}",
            put(schedule_handler::replace_schedule)
                .patch(schedule_handler::update_schedule)
                .delete(schedule_handler::delete_schedule),
        )
}
