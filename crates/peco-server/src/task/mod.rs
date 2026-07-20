// ============================================================================
// Task 模块 — Router 组装 + 公共导出
// ============================================================================

mod executor;
mod handler;
mod scheduler;

pub use scheduler::CronScheduler;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, patch, post};

use crate::state::AppState;

/// 组装 `/api/tasks` 路由组。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(handler::list_tasks).post(handler::create_task))
        .route(
            "/{id}",
            patch(handler::update_task).delete(handler::delete_task),
        )
        .route("/{id}/toggle", post(handler::toggle_task))
        .route("/{id}/logs", get(handler::get_task_logs))
}
