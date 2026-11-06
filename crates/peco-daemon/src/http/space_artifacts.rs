//! `/api/spaces/{ws_id}/artifacts` —— 空间维度的产物列表。
//!
//! 与 `/api/artifacts/{id}/...` 的分工：那两条按产物 id 取文件内容，本接口按
//! 空间列清单，供空间详情页与看板的「成果」区使用。
//!
//! 归属只看 `conversation_id → conversations.workspace_id`：同一台机器上的
//! 单用户，`artifacts.user_id` 恒为固定值，用它过滤等于不过滤。

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use serde::{Deserialize, Serialize};

use crate::db::artifacts::{self, WorkspaceArtifactRow};
use crate::db::workspaces;
use crate::error::DaemonError;
use crate::state::AppState;

/// 一屏够用的条数；超出部分由前端翻页（本期不做游标）。
const DEFAULT_LIMIT: i64 = 50;
/// 上限：产物行可能上万，不设上限等于让一次请求把整库读出来。
const MAX_LIMIT: i64 = 200;

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ArtifactBrief {
    pub id: String,
    pub conversation_id: String,
    /// 产出该产物的对话所属 agent。
    pub agent_name: String,
    /// `image` / `video` / `audio` / `document`（登记时按 mime 推得）。
    pub kind: String,
    /// 相对该空间 work 目录的路径。
    pub rel_path: String,
    /// 登记时的字节数；与 `/api/artifacts` 的 DTO 同名，前端不必记两套。
    pub size_bytes: i64,
    pub created_at: String,
}

impl From<WorkspaceArtifactRow> for ArtifactBrief {
    fn from(row: WorkspaceArtifactRow) -> Self {
        Self {
            id: row.id,
            conversation_id: row.conversation_id,
            agent_name: row.agent_name,
            kind: row.kind,
            rel_path: row.rel_path,
            size_bytes: row.size_bytes,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ArtifactList {
    pub items: Vec<ArtifactBrief>,
    /// 空间内产物总数，不随 `limit` 变化。
    pub total: i64,
}

pub async fn list(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ArtifactList>, DaemonError> {
    if workspaces::by_id(&state.pool, &ws_id).await?.is_none() {
        return Err(DaemonError::SpaceNotFound { ws_id });
    }
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let rows = artifacts::list_by_workspace(&state.pool, &ws_id, limit).await?;
    let total = artifacts::count_by_workspace(&state.pool, &ws_id).await?;
    Ok(Json(ArtifactList {
        items: rows.into_iter().map(ArtifactBrief::from).collect(),
        total,
    }))
}
