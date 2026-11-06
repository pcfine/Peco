//! 请求身份：单用户 daemon 的「这次请求落在哪个空间」。
//!
//! 单用户语义：DB 各表的 `user_id` 恒写 [`crate::LOCAL_USER_ID`]，`ws_id` 只
//! 用来定位工作空间实例，**不得**写进 `user_id` 列 —— 把空间塞进用户维度会让
//! 「谁的资源」在两套语义上撞同一个列名，跨空间查询也就再没有可信的隔离边界。

use axum::extract::{FromRequestParts, Query};
use axum::http::StatusCode;
use axum::http::request::Parts;
use serde::Deserialize;

use crate::api_error::ApiError;
use crate::db::workspaces;
use crate::error::DaemonError;
use crate::state::AppState;

/// 空间选择器请求头。EventSource 无法自定义请求头，故另留 `?ws=` 兜底。
pub const SPACE_HEADER: &str = "X-Peco-Space";

/// 请求所落的已索引空间。
#[derive(Debug, Clone)]
pub struct LocalUser {
    pub ws_id: String,
}

#[derive(Debug, Deserialize)]
struct SpaceQuery {
    ws: Option<String>,
}

impl FromRequestParts<AppState> for LocalUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let ws_id = header_space(parts)
            .or_else(|| query_space(parts))
            .ok_or_else(|| {
                ApiError::coded(
                    StatusCode::BAD_REQUEST,
                    "space_required",
                    "missing space selector: send the X-Peco-Space header or ?ws=",
                )
            })?;

        // 这里只判存在性：未打开的空间仍要能走列表类接口（它们只读 DB），
        // 需要实例的 handler 自己经 `SpaceRegistry` 拿，拿到 409 才算失败。
        let exists = workspaces::by_id(&state.pool, &ws_id).await?.is_some();
        if !exists {
            return Err(DaemonError::SpaceNotFound { ws_id }.into());
        }
        Ok(LocalUser { ws_id })
    }
}

fn header_space(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get(SPACE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn query_space(parts: &Parts) -> Option<String> {
    Query::<SpaceQuery>::try_from_uri(&parts.uri)
        .ok()
        .and_then(|Query(query)| query.ws)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
