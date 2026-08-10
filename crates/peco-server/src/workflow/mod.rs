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
mod sse;
mod stream_handler;
mod types;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post, put};
use peco_core::workflow::WorkflowEvent;
use tokio::sync::broadcast;

use crate::state::AppState;

// ============================================================================
// WorkflowEventSource — 窄 trait，遵循 deps.rs 的 DI 模式
// ============================================================================

/// 工作流事件订阅接口。
///
/// 遵循 `deps.rs` 窄 trait 模式。注入 SSE handler 以替代
/// 直接调用 `active::subscribe_events()`（全局静态函数）。
///
/// 由 `AppState` 实现，内部委托给 `active` 模块的全局注册表。
/// SSE handler 通过 `state.subscribe_events(&run_id)` 调用，
/// 而非直接访问全局静态变量，从而实现可测试性和后端可替换性。
pub trait WorkflowEventSource: Send + Sync {
    /// 订阅指定执行的 WorkflowEvent 流。
    ///
    /// 返回 broadcast Receiver，调用方通过 `recv().await` 消费事件。
    /// 若执行不存在或已结束，返回 None。
    fn subscribe_events(&self, run_id: &str) -> Option<broadcast::Receiver<WorkflowEvent>>;
}

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
            "/executions/{run_id}/stream",
            get(stream_handler::stream_execution),
        )
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
