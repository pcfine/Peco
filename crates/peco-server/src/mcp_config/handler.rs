// MCP Config Handler

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub async fn get_config(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let path = ws.root().join("mcpconfig.json");
    if path.exists() {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| ApiError::Internal(format!("failed to read mcp_config.json: {e}")))?;
        let json: serde_json::Value =
            serde_json::from_str(&content).unwrap_or(serde_json::json!({ "mcpServers": {} }));
        Ok(Json(json))
    } else {
        Ok(Json(serde_json::json!({ "mcpServers": {} })))
    }
}

pub async fn update_config(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let path = ws.root().join("mcpconfig.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApiError::Internal(format!("failed to create directory: {e}")))?;
    }
    let content = serde_json::to_string_pretty(&body)
        .map_err(|e| ApiError::BadRequest(format!("invalid JSON: {e}")))?;
    std::fs::write(&path, &content)
        .map_err(|e| ApiError::Internal(format!("failed to write mcpconfig.json: {e}")))?;

    // 通知 WorkSpace 热重载 MCP 配置
    let system_mcp = state.workspace_manager.system_config().mcp.clone();
    let count = ws.reload_mcp_config(&system_mcp);
    tracing::info!(count, "MCP config hot-reloaded after API update");

    // 更新 mcp 模块哈希
    let mcp_hash = peco_core::workspace::hash::compute_mcp_hash(ws.root());
    let _ = crate::db::workspace_hashes::upsert_hash(&state.db, &user_id, "mcp", &mcp_hash).await;

    Ok(Json(SuccessResponse {
        success: true,
        message: Some("MCP config updated".into()),
    }))
}

pub async fn test_connection(
    AuthUser { user_id: _ }: AuthUser,
    State(_state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    Ok(Json(SuccessResponse {
        success: true,
        message: Some(format!("MCP server '{name}' test not yet implemented")),
    }))
}
