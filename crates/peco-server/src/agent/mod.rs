// ============================================================================
// Agent 模块 — 路由、处理函数、实例注册表
// ============================================================================

mod handler;
mod orchestration;
mod registry;

pub use orchestration::{WebDelegateSubAgentTool, WebRunParallelSubAgentsTool};
pub use registry::AgentRegistry;

use std::sync::Arc;

use axum::routing::get;
use axum::Router;

use crate::state::AppState;

/// 返回 Agent 管理路由（需 JWT 认证）。
///
/// # 路由
/// - `GET /` — 当前用户的 Agent 列表
/// - `POST /` — 创建 Agent
/// - `GET /:id` — Agent 详情
/// - `PATCH /:id` — 更新 Agent
/// - `DELETE /:id` — 删除 Agent
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(handler::list).post(handler::create))
        .route("/{id}", get(handler::get).patch(handler::update).delete(handler::delete))
}
