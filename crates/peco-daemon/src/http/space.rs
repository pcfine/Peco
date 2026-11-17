//! `/api/spaces` —— 空间列表、创建、详情、运行属性修改、打开与关闭、删除。
//!
//! 每个 handler 只做三件事：取参数、调领域函数、拼响应；门槛与决策表都在
//! `space::close` 与 `space::open` 里，这里重复一遍就会有两份定义。

use std::path::Path;

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::config::resolve_exit_policy;
use crate::db::dispatches;
use crate::db::workspaces::{self, WorkspaceRow};
use crate::error::DaemonError;
use crate::space::close as space_close;
use crate::space::manifest;
use crate::space::open;
use crate::space::peco::PECO_TEAM_ID;
use crate::state::AppState;

const STATUS_OPEN: &str = "open";
const STATUS_CLOSED: &str = "closed";

/// marker 的身份字段；`exit_policy` / `label` 属于 DB，不在这里出现。
///
/// `team` 是团队目录末段（团队路径的 id），`user_id` 是单用户 daemon 的固定值
/// —— 记录文件本身不再保存创建者。
#[derive(Debug, Serialize)]
pub struct MarkerInfo {
    pub team: String,
    pub user_id: String,
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct SpaceView {
    pub ws_id: String,
    pub work_dir: String,
    pub team_id: String,
    pub label: Option<String>,
    pub exit_policy: String,
    /// 两级配置解析后的有效退出策略，仅供 UI 展示与将来自动关闭使用。
    /// 本期关闭动作由 UI 显式触发，`POST /close` 无条件执行。
    pub effective_exit_policy: String,
    /// `open`（实例在注册表里）或 `closed`。
    ///
    /// 只说明「本次进程里开没开」，回答不了「是否曾经被显式关闭」——
    /// 三组划分（进行中 / 已存在 / 已关闭）需要下面的两个时刻列。
    pub status: &'static str,
    /// 显式关闭的时刻；重新打开会清空。
    pub closed_at: Option<String>,
    /// 最近一次成功打开的时刻；从未打开过为 `null`。
    pub last_opened_at: Option<String>,
    /// Peco 默认空间不可删除（`DELETE` 返回 400 `reserved_space`）：
    /// 前端据此隐藏删除按钮，只留关闭。
    pub reserved: bool,
    /// 门槛口径的两个计数，与 close 返回的 409 同一来源。
    pub inflight_dispatch: i64,
    pub inflight_foreign: i64,
    pub marker: Option<MarkerInfo>,
}

/// 打开空间后的响应：`agents` 来自刚打开实例的定义清单。
#[derive(Debug, Serialize)]
pub struct SpaceOpened {
    pub ws_id: String,
    pub work_dir: String,
    pub team_id: String,
    pub agents: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    pub work_dir: String,
    pub team_id: String,
    pub label: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateRequest {
    pub team_id: Option<String>,
    pub label: Option<String>,
    pub exit_policy: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CloseRequest {
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub include_closed: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PurgeQuery {
    pub force: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct CloseResponse {
    pub stopped: bool,
}

#[derive(Debug, Serialize)]
pub struct PurgeResponse {
    pub purged: bool,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<SpaceView>>, DaemonError> {
    let include_closed = query.include_closed.unwrap_or(true);
    let mut views = Vec::new();
    for row in workspaces::list(&state.pool).await? {
        let view = space_view(&state, row).await?;
        if !include_closed && view.status != STATUS_OPEN {
            continue;
        }
        views.push(view);
    }
    Ok(Json(views))
}

pub async fn detail(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
) -> Result<Json<SpaceView>, DaemonError> {
    let row = by_id(&state, &ws_id).await?;
    Ok(Json(space_view(&state, row).await?))
}

pub async fn create(
    State(state): State<AppState>,
    Json(request): Json<CreateRequest>,
) -> Result<Response, DaemonError> {
    // Peco 只有 `$PECO_HOME/spaces/peco/work` 一个保留空间，不再由用户建第二个。
    // 这道门槛必须早于任何 I/O：`resolve_ws_id` 会补索引行、`create_dir_all` 会
    // 留下目录，拒绝时两者都不该发生。
    if request.team_id == PECO_TEAM_ID {
        return Err(DaemonError::InvalidArgument {
            message: format!("team '{PECO_TEAM_ID}' cannot own new spaces"),
        });
    }
    let work_dir = open::normalize_work_dir(Path::new(&request.work_dir))?;
    let key = work_dir.to_string_lossy().into_owned();
    let existed = workspaces::by_work_dir(&state.pool, &key).await?.is_some();

    // 团队校验发生在 resolve_ws_id 内部，且先于任何建目录动作：team 不存在时
    // work_dir 不该被留下来。
    let ws_id = open::resolve_ws_id(&state.env, &work_dir, &request.team_id).await?;
    if let Some(label) = request.label.as_deref()
        && !existed
    {
        workspaces::set_label(&state.pool, &ws_id, Some(label)).await?;
    }

    // 团队校验已通过；Hub 的 `ensure_open` 要求目录存在，旧路径的
    // `open_workspace` 也会在打开前 `create_dir_all`，这里保持同一语义。
    std::fs::create_dir_all(&work_dir).map_err(|e| DaemonError::WorkDirUnusable {
        work_dir: key.clone(),
        reason: e.to_string(),
    })?;
    let handle = state
        .hub
        .ensure_open(&work_dir, None)
        .await
        .map_err(|e| open::map_tool_error(e, &ws_id))?;
    let ws = handle.workspace();
    let row = by_id(&state, &ws_id).await?;
    let body = SpaceOpened {
        ws_id: row.id.clone(),
        work_dir: row.work_dir.clone(),
        team_id: row.team_id.clone(),
        agents: ws.agent_manager().list_names(),
    };

    let status = if existed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(body)).into_response())
}

pub async fn update(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
    Json(request): Json<UpdateRequest>,
) -> Result<Json<SpaceView>, DaemonError> {
    let row = by_id(&state, &ws_id).await?;

    // 全部校验先于任何写入：改绑失败时 marker 与 DB 都必须保持原样。
    if let Some(exit_policy) = request.exit_policy.as_deref()
        && !matches!(exit_policy, "inherit" | "keep" | "stop")
    {
        return Err(DaemonError::InvalidArgument {
            message: format!("unknown exit_policy '{exit_policy}'"),
        });
    }
    if let Some(team_id) = request.team_id.as_deref() {
        // 改绑到 Peco 同样被拒：保留空间的身份由引导独占，接管的目录会跟它抢
        // 同一个团队能力根。
        if team_id == PECO_TEAM_ID {
            return Err(DaemonError::InvalidArgument {
                message: format!("team '{PECO_TEAM_ID}' is reserved"),
            });
        }
        // 目录不存在 ⇒ `team_missing`（此时什么都没写）；目录存在但缺索引行 ⇒ 先补行。
        open::ensure_team_indexed(&state.env, team_id).await?;
    }

    if let Some(team_id) = request.team_id.as_deref()
        && row.team_id != team_id
    {
        crate::space::rebind_team(&state.env, &row, team_id).await?;
        if state.hub.is_open(Path::new(&row.work_dir)) {
            // 已打开的实例持有旧团队的能力根；换根要重建实例，不能在请求里悄悄做。
            tracing::warn!(
                ws_id = %row.id,
                team_id,
                "team rebound while the space is open; the live instance keeps the old team until reopened"
            );
        }
    }
    if let Some(label) = request.label.as_deref() {
        workspaces::set_label(&state.pool, &row.id, Some(label)).await?;
    }
    if let Some(exit_policy) = request.exit_policy.as_deref() {
        workspaces::set_exit_policy(&state.pool, &row.id, exit_policy).await?;
    }

    let row = by_id(&state, &row.id).await?;
    Ok(Json(space_view(&state, row).await?))
}

/// 打开一个已索引的空间；不重建实例，已经打开就原样返回。
///
/// 打开动作会改 `closed_at` 与 `last_opened_at`，所以视图基于打开之后重读的那一行，
/// 不是打开之前取到的快照。
pub async fn open(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
) -> Result<Json<SpaceView>, DaemonError> {
    // 未知 id 在 opener 之前就拒掉：路径错的请求不该走一遍目录判定。
    let row = by_id(&state, &ws_id).await?;
    // 实例活着就已经是「打开」，目录在不在只影响下一次打开：这里放行，
    // 否则删掉一个正在跑的目录会把活着的实例报成 409。
    if state.hub.is_open(Path::new(&row.work_dir)) {
        return Ok(Json(space_view(&state, row).await?));
    }
    open::ensure_open_indexed(&state.env, &state.hub, &ws_id).await?;
    let row = by_id(&state, &ws_id).await?;
    Ok(Json(space_view(&state, row).await?))
}

pub async fn close(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
    Json(request): Json<CloseRequest>,
) -> Result<Json<CloseResponse>, DaemonError> {
    let outcome =
        space_close::close_workspace(&state.env, &state.hub, &ws_id, request.force).await?;
    tracing::info!(
        ws_id,
        force = request.force,
        orphaned = outcome.orphaned,
        "space closed"
    );
    Ok(Json(CloseResponse { stopped: true }))
}

pub async fn purge(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
    Query(query): Query<PurgeQuery>,
) -> Result<Json<PurgeResponse>, DaemonError> {
    let force = query.force.unwrap_or(false);
    let outcome = space_close::purge_workspace(&state.env, &state.hub, &ws_id, force).await?;
    tracing::info!(
        ws_id,
        force,
        orphaned = outcome.orphaned,
        "space row purged; ledger kept"
    );
    Ok(Json(PurgeResponse { purged: true }))
}

async fn by_id(state: &AppState, ws_id: &str) -> Result<WorkspaceRow, DaemonError> {
    workspaces::by_id(&state.pool, ws_id)
        .await?
        .ok_or_else(|| DaemonError::SpaceNotFound {
            ws_id: ws_id.to_string(),
        })
}

async fn space_view(state: &AppState, row: WorkspaceRow) -> Result<SpaceView, DaemonError> {
    let is_open = state.hub.is_open(Path::new(&row.work_dir));
    let reserved = state.env.peco.is_peco_space(&row.id);
    let inflight = dispatches::inflight_counts(&state.pool, &row.id, &state.env.peco.ws_id).await?;
    let effective_exit_policy = resolve_exit_policy(&row.exit_policy, state.config.exit_policy);
    Ok(SpaceView {
        ws_id: row.id,
        work_dir: row.work_dir.clone(),
        team_id: row.team_id,
        label: row.label,
        exit_policy: row.exit_policy,
        effective_exit_policy: effective_exit_policy.as_str().to_string(),
        status: if is_open { STATUS_OPEN } else { STATUS_CLOSED },
        closed_at: row.closed_at,
        last_opened_at: row.last_opened_at,
        reserved,
        inflight_dispatch: inflight.dispatch,
        inflight_foreign: inflight.foreign,
        // 坏 marker 不该让整个列表 409：列表只降级掉这一项，详情里也照此处理。
        marker: marker_info(Path::new(&row.work_dir)),
    })
}

fn marker_info(work_dir: &Path) -> Option<MarkerInfo> {
    match manifest::read(work_dir) {
        Ok(Some(found)) => Some(MarkerInfo {
            team: manifest::team_id_of(&found).unwrap_or_default(),
            user_id: crate::LOCAL_USER_ID.to_string(),
            session_id: found.session_id,
        }),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(
                work_dir = %work_dir.display(),
                code = e.code(),
                error = %e,
                "marker unusable; reporting the space without marker fields"
            );
            None
        }
    }
}
