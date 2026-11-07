//! `/api/spaces/{ws_id}/conversation/ensure` —— 空间单会话的惰性创建。
//!
//! 前端点击空间时才 ensure：dispatch 与唤醒会顺带打开空间，但那些路径不该
//! 在用户没进聊天页时留下会话行。

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use serde::{Deserialize, Serialize};

use crate::db::{conversations, workspaces};
use crate::error::DaemonError;
use crate::state::AppState;

/// 空间页唯一对话的默认 agent；请求体缺省时用它。
const DEFAULT_AGENT_NAME: &str = "@master";

/// 请求体整体可选：前端在「已经知道要聊谁」之前不必构造 JSON。
#[derive(Debug, Default, Deserialize)]
pub struct EnsureRequest {
    pub agent_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EnsureResponse {
    pub conversation_id: String,
    /// 本次是否真的插了新行：前端据此决定要不要去拉历史消息。
    pub created: bool,
}

pub async fn ensure(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
    body: Option<Json<EnsureRequest>>,
) -> Result<Json<EnsureResponse>, DaemonError> {
    if workspaces::by_id(&state.pool, &ws_id).await?.is_none() {
        return Err(DaemonError::SpaceNotFound { ws_id });
    }
    let agent_name = body
        .and_then(|Json(request)| request.agent_name)
        .unwrap_or_else(|| DEFAULT_AGENT_NAME.to_string());
    let (conversation_id, created) =
        conversations::ensure_space_session(&state.pool, &ws_id, &agent_name).await?;
    tracing::debug!(
        ws_id = %ws_id,
        agent_name = %agent_name,
        conversation_id = %conversation_id,
        created,
        "space session ensured"
    );
    Ok(Json(EnsureResponse {
        conversation_id,
        created,
    }))
}
