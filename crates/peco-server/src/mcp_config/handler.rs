// MCP Config Handler

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;

use peco_core::tools::McpAccess;

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

    // 1. 解析前端发来的 JSON 是否符合 McpConfig 格式
    let new_config: peco_core::mcp::McpConfig = serde_json::from_value(body)
        .map_err(|e| ApiError::BadRequest(format!("Invalid MCP config format: {e}")))?;

    // 2. 校验配置（如 stdio 缺 command、http 缺 url）— 客户端错误 → 400
    new_config
        .validate()
        .map_err(|e| ApiError::BadRequest(format!("Invalid MCP config: {e}")))?;

    let server_count = new_config.mcp_servers.len();

    // 3. 原子更新：写盘 → 更新内存，在一次锁内完成
    //    此时配置已通过校验，剩余失败均为服务器端 IO 错误 → 500
    ws.replace_mcp_config(new_config)
        .map_err(|e| ApiError::Internal(format!("Failed to save MCP config: {e}")))?;

    tracing::info!(
        servers = server_count,
        "MCP config updated via replace_mcp_config"
    );

    // 3. 更新 mcp 模块哈希
    let mcp_hash = peco_core::workspace::hash::compute_mcp_hash(ws.root());
    let _ = crate::db::workspace_hashes::upsert_hash(&state.db, &user_id, "mcp", &mcp_hash).await;

    Ok(Json(SuccessResponse {
        success: true,
        message: Some("MCP config updated".into()),
    }))
}

pub async fn test_connection(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<peco_core::mcp::McpTestResult>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;

    let config = ws.get_mcp_server_config(&name).ok_or_else(|| {
        ApiError::NotFound(format!("MCP server '{name}' not found in configuration"))
    })?;

    let result = peco_core::mcp::test_mcp_connection(&name, &config).await;

    Ok(Json(result))
}
