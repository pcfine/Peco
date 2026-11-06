//! Agent 作用域对话管理 + SSE 流式聊天。
//!
//! 会话按空间隔离：`{agentId}` 只定位 Agent，历史归属由请求的空间选择器决定
//! （[`LocalUser`](crate::auth::LocalUser)）。单用户 daemon 没有 `/api/conversations`
//! 那层多用户兼容路由 —— 它的取参方式（按 `user_id` 查会话）在单用户语义下没有
//! 对应物，留着只会造出「同一会话在哪个空间」说不清的第二条路径。

pub(crate) mod active;
pub(crate) mod conversation;
pub(crate) mod handler;
pub(crate) mod images;
pub(crate) mod sse;

pub use active::ChatActiveRuns;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, patch, post};

use crate::state::AppState;

/// 对话管理路由；每个 handler 都要求 `LocalUser`，空间选择器因此是强制项。
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/{agent_id}/conversations",
            get(handler::list_conversations).post(handler::create_conversation),
        )
        .route(
            "/{agent_id}/conversations/{id}",
            patch(handler::update_conversation).delete(handler::delete_conversation),
        )
        .route(
            "/{agent_id}/conversations/{id}/messages",
            get(handler::get_messages),
        )
        .route(
            "/{agent_id}/conversations/{id}/session",
            get(handler::get_session_snapshot),
        )
        .route(
            "/{agent_id}/conversations/{id}/images",
            // layer 加在 MethodRouter 上，只作用于本路由（Router 级 layer 会
            // 波及之前已注册的所有路由）
            post(images::upload_conversation_image)
                .layer(DefaultBodyLimit::max(images::MAX_UPLOAD_BODY_BYTES)),
        )
        .route(
            "/{agent_id}/conversations/{id}/artifacts",
            get(crate::artifact::list),
        )
        .route(
            "/{agent_id}/conversations/{id}/stream",
            get(handler::stream_chat),
        )
        .route(
            "/{agent_id}/conversations/{id}/pause",
            post(handler::pause_chat),
        )
        .route(
            "/{agent_id}/conversations/{id}/resume",
            post(handler::resume_chat),
        )
        .route(
            "/{agent_id}/conversations/{id}/export",
            get(handler::export_conversation),
        )
}
