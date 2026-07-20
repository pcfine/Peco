// ============================================================================
// Chat 模块 — 对话管理 + SSE 流式聊天
// ============================================================================

mod handler;
pub(crate) mod sse;

use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, get};

use crate::state::AppState;

/// 返回对话管理路由（需 JWT 认证）。
///
/// # 路由
/// - `GET /` — 对话列表
/// - `POST /` — 创建对话
/// - `DELETE /:id` — 删除对话
/// - `GET /:id/messages` — 消息历史
/// - `GET /:id/session` — Session 完整快照（含 tool calls/reasoning）
/// - `GET /:id/stream` — SSE 流式对话
pub fn conversation_router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/",
            get(handler::list_conversations).post(handler::create_conversation),
        )
        .route("/{id}", delete(handler::delete_conversation))
        .route("/{id}/messages", get(handler::get_messages))
        .route("/{id}/session", get(handler::get_session_snapshot))
        .route("/{id}/stream", get(handler::stream_chat))
}
