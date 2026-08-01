// ============================================================================
// Chat 模块 — Agent 作用域对话管理 + SSE 流式聊天
// ============================================================================

pub(crate) mod conversation;
pub(crate) mod handler;
pub(crate) mod sse;

use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, get, patch};

use peco_core::persistence::SessionPersister;

use crate::state::AppState;

/// 返回 Agent 作用域下的对话管理路由（需 JWT 认证）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/{agentId}/conversations",
            get(handler::list_conversations).post(handler::create_conversation),
        )
        .route(
            "/{agentId}/conversations/{id}",
            patch(handler::update_conversation).delete(handler::delete_conversation),
        )
        .route(
            "/{agentId}/conversations/{id}/messages",
            get(handler::get_messages),
        )
        .route(
            "/{agentId}/conversations/{id}/session",
            get(handler::get_session_snapshot),
        )
        .route(
            "/{agentId}/conversations/{id}/stream",
            get(handler::stream_chat),
        )
        .route(
            "/{agentId}/conversations/{id}/export",
            get(handler::export_conversation),
        )
}

/// DEPRECATED: 旧路由兼容层（/api/conversations）。
/// 将在 Phase 4 完全移除。新代码应使用 /api/chat/{agentId}/conversations。
pub fn conversation_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_legacy).post(create_legacy))
        .route("/{id}", delete(delete_legacy))
        .route("/{id}/messages", get(messages_legacy))
        .route("/{id}/session", get(session_legacy))
        .route("/{id}/stream", get(stream_legacy))
}

// ── Legacy wrappers (DEPRECATED) ────────────────────────────────────────────

async fn list_legacy(
    user: crate::auth::AuthUser,
    state: axum::extract::State<Arc<AppState>>,
) -> Result<axum::Json<Vec<handler::ConversationResponse>>, crate::error::ApiError> {
    use crate::db::conversations;
    tracing::warn!(
        user_id = %user.user_id,
        "DEPRECATED /api/conversations GET; migrate to /api/chat/:agentId/conversations"
    );
    let rows = conversations::list_by_user(&state.db, &user.user_id, None).await?;
    Ok(axum::Json(
        rows.iter().map(handler::row_to_response).collect(),
    ))
}

async fn create_legacy(
    user: crate::auth::AuthUser,
    state: axum::extract::State<Arc<AppState>>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<
    (
        axum::http::StatusCode,
        axum::Json<handler::ConversationResponse>,
    ),
    crate::error::ApiError,
> {
    use crate::db::conversations;
    let title = body["title"].as_str().unwrap_or("新对话").to_string();
    let agent_name = body["agent_name"].as_str().unwrap_or("unknown").to_string();
    let conv_id = uuid::Uuid::new_v4().to_string();

    tracing::warn!(
        user_id = %user.user_id,
        "DEPRECATED /api/conversations POST; migrate to /api/chat/:agentId/conversations"
    );

    conversations::insert(
        &state.db,
        &conversations::CreateConversationParams {
            id: conv_id.clone(),
            user_id: user.user_id.clone(),
            agent_id: None,
            agent_name,
            title,
        },
    )
    .await?;

    let row = conversations::find_by_id(&state.db, &conv_id)
        .await?
        .ok_or_else(|| crate::error::ApiError::Internal("not found".into()))?;
    Ok((
        axum::http::StatusCode::CREATED,
        axum::Json(handler::row_to_response(&row)),
    ))
}

async fn delete_legacy(
    user: crate::auth::AuthUser,
    state: axum::extract::State<Arc<AppState>>,
    axum::extract::Path(conv_id): axum::extract::Path<String>,
) -> Result<axum::Json<handler::SuccessResponse>, crate::error::ApiError> {
    use crate::db::{conversations, messages};
    tracing::warn!(
        user_id = %user.user_id,
        "DEPRECATED /api/conversations DELETE; migrate to /api/chat/:agentId/conversations"
    );
    let _ = conversations::find_by_id_and_user(&state.db, &conv_id, &user.user_id)
        .await?
        .ok_or_else(|| crate::error::ApiError::NotFound(format!("not found: {conv_id}")))?;
    messages::delete_by_conversation(&state.db, &conv_id).await?;
    let persister = crate::session_store::SqliteSessionPersister::new(state.db.clone());
    let _ = persister.delete(&conv_id).await;
    conversations::delete(&state.db, &conv_id).await?;
    Ok(axum::Json(handler::SuccessResponse { success: true }))
}

async fn messages_legacy(
    user: crate::auth::AuthUser,
    state: axum::extract::State<Arc<AppState>>,
    axum::extract::Path(conv_id): axum::extract::Path<String>,
) -> Result<axum::Json<Vec<handler::MessageResponse>>, crate::error::ApiError> {
    use crate::db::{conversations, messages};
    let _ = conversations::find_by_id_and_user(&state.db, &conv_id, &user.user_id)
        .await?
        .ok_or_else(|| crate::error::ApiError::NotFound(format!("not found: {conv_id}")))?;
    let rows = messages::list_by_conversation(&state.db, &conv_id, 0, 50).await?;
    let resp: Vec<handler::MessageResponse> = rows
        .iter()
        .map(|r| handler::MessageResponse {
            id: r.id.clone(),
            role: r.role.clone(),
            content: r.content.clone(),
            agent_id: r.agent_id.clone(),
            agent_name: r.agent_name.clone(),
            created_at: r.created_at.clone(),
        })
        .collect();
    Ok(axum::Json(resp))
}

async fn session_legacy(
    user: crate::auth::AuthUser,
    state: axum::extract::State<Arc<AppState>>,
    axum::extract::Path(conv_id): axum::extract::Path<String>,
) -> Result<axum::Json<handler::SessionSnapshotResponse>, crate::error::ApiError> {
    use crate::db::conversations;
    let agent_name = conversations::find_by_id_and_user(&state.db, &conv_id, &user.user_id)
        .await?
        .map(|c| c.agent_name)
        .unwrap_or_else(|| "unknown".to_string());
    handler::get_session_snapshot(
        user,
        state,
        axum::extract::Path((agent_name, conv_id)),
    )
    .await
}

async fn stream_legacy(
    user: crate::auth::AuthUser,
    state: axum::extract::State<Arc<AppState>>,
    axum::extract::Path(conv_id): axum::extract::Path<String>,
    axum::extract::Query(params): axum::extract::Query<handler::StreamQuery>,
) -> Result<
    axum::response::sse::Sse<
        impl futures::stream::Stream<
            Item = Result<axum::response::sse::Event, std::convert::Infallible>,
        >,
    >,
    crate::error::ApiError,
> {
    use crate::db::conversations;
    let agent_name = conversations::find_by_id_and_user(&state.db, &conv_id, &user.user_id)
        .await?
        .map(|c| c.agent_name)
        .unwrap_or_else(|| "unknown".to_string());
    handler::stream_chat(
        user,
        state,
        axum::extract::Path((agent_name, conv_id)),
        axum::extract::Query(params),
    )
    .await
}
