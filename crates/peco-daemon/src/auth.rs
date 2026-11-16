//! 请求身份：单用户 daemon 的「这次请求落在哪个空间 / 哪个团队」。
//!
//! 单用户语义：DB 各表的 `user_id` 恒写 [`crate::LOCAL_USER_ID`]，`ws_id` 只
//! 用来定位工作空间实例，**不得**写进 `user_id` 列 —— 把空间塞进用户维度会让
//! 「谁的资源」在两套语义上撞同一个列名，跨空间查询也就再没有可信的隔离边界。
//!
//! 两个选择器对应两种落点：空间是执行上下文（会话、work_dir、派发），团队的
//! 能力根则与空间无关 —— 没有空间的团队照样要读写自己的 `agents/`、`skills/`。

use std::sync::Arc;

use axum::extract::{FromRequestParts, Query};
use axum::http::StatusCode;
use axum::http::request::Parts;
use peco_core::workspace::WorkSpace;
use serde::Deserialize;

use crate::api_error::ApiError;
use crate::db::workspaces;
use crate::error::DaemonError;
use crate::state::AppState;

/// 空间选择器请求头。EventSource 无法自定义请求头，故另留 `?ws=` 兜底。
pub const SPACE_HEADER: &str = "X-Peco-Space";

/// 团队选择器请求头：只认团队能力根的请求用它，不需要空间实例。
pub const TEAM_HEADER: &str = "X-Peco-Team";

/// 请求所落的已索引空间。
#[derive(Debug, Clone)]
pub struct LocalUser {
    pub ws_id: String,
}

/// 请求所落的资源边界：团队 + 该团队的能力根实例。
///
/// 空间选择器给的是「某个空间所属团队」，团队选择器给的是团队 id；两者对
/// handler 是同一个东西，故打包成一份。
pub struct ResourceScope {
    pub team_id: String,
    workspace: ScopeWorkspace,
}

/// 能力根实例的取用结果。
///
/// 空间选择器给的空间可能还没打开 —— 列表类接口要能在这种状态下照常回答，
/// 所以「没打开」必须是一个可查询的状态而不是提取期的拒绝。
enum ScopeWorkspace {
    Open(Arc<WorkSpace>),
    NotOpen { ws_id: String },
}

impl ResourceScope {
    /// 团队能力根的实例。
    ///
    /// 空间选择器给的空间未打开时按既有语义报 `space_not_open`：需要实例的
    /// 端点保持「先打开再用」的约束，不被团队寻址顺手放宽。
    pub fn workspace(&self) -> Result<Arc<WorkSpace>, DaemonError> {
        match &self.workspace {
            ScopeWorkspace::Open(ws) => Ok(Arc::clone(ws)),
            ScopeWorkspace::NotOpen { ws_id } => Err(DaemonError::SpaceNotOpen {
                ws_id: ws_id.clone(),
            }),
        }
    }

    /// 已打开时为实例，空间未打开时为 `None` —— 列表类接口的降级入口。
    pub fn opened(&self) -> Option<Arc<WorkSpace>> {
        match &self.workspace {
            ScopeWorkspace::Open(ws) => Some(Arc::clone(ws)),
            ScopeWorkspace::NotOpen { .. } => None,
        }
    }
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
        let ws_id = indexed_space_id(parts, state).await?.ok_or_else(|| {
            ApiError::coded(
                StatusCode::BAD_REQUEST,
                "space_required",
                "missing space selector: send the X-Peco-Space header or ?ws=",
            )
        })?;
        Ok(LocalUser { ws_id })
    }
}

impl FromRequestParts<AppState> for ResourceScope {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // 空间优先：带了空间选择器就走原路径（团队从空间行反查），
        // 同时带两个头时空间说的算。
        if let Some(ws_id) = indexed_space_id(parts, state).await? {
            let team_id = state.spaces.team_id(&ws_id).await?;
            let workspace = match state.spaces.opened(&ws_id).await? {
                Some(handle) => ScopeWorkspace::Open(handle.workspace()),
                None => ScopeWorkspace::NotOpen { ws_id },
            };
            return Ok(ResourceScope { team_id, workspace });
        }

        let team_id = header_team(parts).ok_or_else(|| {
            ApiError::coded(
                StatusCode::BAD_REQUEST,
                "space_required",
                "missing scope selector: send the X-Peco-Space header, ?ws=, \
                 or the X-Peco-Team header",
            )
        })?;
        let workspace = state.spaces.team_workspace(&team_id).await?;
        Ok(ResourceScope {
            team_id,
            workspace: ScopeWorkspace::Open(workspace),
        })
    }
}

/// 请求里的空间 id（先请求头、后 `?ws=`），已确认它有索引行。
///
/// 这里只判存在性：未打开的空间仍要能走列表类接口（它们只读 DB），
/// 需要实例的调用方自己经 `SpaceRegistry` 拿，拿到 409 才算失败。
async fn indexed_space_id(parts: &Parts, state: &AppState) -> Result<Option<String>, ApiError> {
    let Some(ws_id) = header_space(parts).or_else(|| query_space(parts)) else {
        return Ok(None);
    };
    let exists = workspaces::by_id(&state.pool, &ws_id).await?.is_some();
    if !exists {
        return Err(DaemonError::SpaceNotFound { ws_id }.into());
    }
    Ok(Some(ws_id))
}

fn header_space(parts: &Parts) -> Option<String> {
    header_value(parts, SPACE_HEADER)
}

fn header_team(parts: &Parts) -> Option<String> {
    header_value(parts, TEAM_HEADER)
}

fn header_value(parts: &Parts, name: &str) -> Option<String> {
    parts
        .headers
        .get(name)
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
