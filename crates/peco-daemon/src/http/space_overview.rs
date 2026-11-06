//! `/api/spaces/{ws_id}/overview` —— 看板一张卡片所需的全部计数。
//!
//! 聚合在服务端完成：卡片上的五项来自四张表，前端逐个取会拿到互相错开的
//! 时间点，且一屏十几张卡片就是几十次往返。
//!
//! 与关闭门槛无关：这里只是读，空间未打开时计数退化成 0 而不是报错。

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use serde::Serialize;

use crate::db::dispatches::{self, DispatchFilter, DispatchRow};
use crate::db::workspaces;
use crate::error::DaemonError;
use crate::state::AppState;

/// 卡片上的「最近任务」条数：再多就不叫概览了。
const RECENT_LIMIT: i64 = 5;

#[derive(Debug, Serialize)]
pub struct OverviewView {
    pub ws_id: String,
    pub team_id: String,
    pub label: Option<String>,
    pub closed_at: Option<String>,
    /// 在飞任务的两组计数，与关闭门槛、`/api/spaces` 同一来源。
    pub inflight: InflightView,
    pub tasks: TasksView,
    /// 已打开实例里的 agent 数；空间未打开时为 0。
    pub agent_count: usize,
    /// 已加载的 skill 数；空间未打开时为 0。
    pub skill_count: usize,
}

#[derive(Debug, Serialize)]
pub struct InflightView {
    pub dispatch: i64,
    pub foreign: i64,
}

#[derive(Debug, Serialize)]
pub struct TasksView {
    pub today: TodayView,
    pub recent: Vec<RecentTaskView>,
}

/// 同一天的四格互斥，加起来就是今天的任务数。
#[derive(Debug, Serialize)]
pub struct TodayView {
    /// 只有跑完的。
    pub completed: i64,
    /// 失败侧终态合计：单列一格，不并进「已完成」。
    pub failed: i64,
    /// 在飞且不在等用户输入。
    pub active: i64,
    pub waiting: i64,
}

#[derive(Debug, Serialize)]
pub struct RecentTaskView {
    pub id: String,
    pub task_id: String,
    pub task_state: String,
    /// 唤醒态与原因让卡片直接显示「跑完了但没叫醒」的一类行。
    pub wake_state: String,
    pub wake_skip_reason: Option<String>,
    pub agent_name: String,
    pub origin_kind: String,
    pub created_at: String,
    pub terminal_at: Option<String>,
    pub digest: Option<String>,
}

impl From<DispatchRow> for RecentTaskView {
    fn from(row: DispatchRow) -> Self {
        Self {
            id: row.id,
            task_id: row.task_id,
            task_state: row.task_state,
            wake_state: row.wake_state,
            wake_skip_reason: row.wake_skip_reason,
            agent_name: row.agent_name,
            origin_kind: row.origin_kind,
            created_at: row.created_at,
            terminal_at: row.terminal_at,
            digest: row.result_digest,
        }
    }
}

pub async fn overview(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
) -> Result<Json<OverviewView>, DaemonError> {
    let row = workspaces::by_id(&state.pool, &ws_id)
        .await?
        .ok_or_else(|| DaemonError::SpaceNotFound {
            ws_id: ws_id.clone(),
        })?;

    let inflight = dispatches::inflight_counts(&state.pool, &ws_id, &state.env.peco.ws_id).await?;
    let today = dispatches::today_counts(&state.pool, &ws_id).await?;
    let recent = dispatches::list(
        &state.pool,
        DispatchFilter {
            target_ws_id: Some(ws_id.as_str()),
            limit: Some(RECENT_LIMIT),
            ..DispatchFilter::default()
        },
    )
    .await?;

    let (agent_count, skill_count) = match state.spaces.opened(&ws_id).await? {
        Some(handle) => {
            let ws = handle.workspace();
            (
                ws.agent_manager().list_names().len(),
                ws.skill_registry().all_meta().len(),
            )
        }
        None => (0, 0),
    };

    Ok(Json(OverviewView {
        ws_id: row.id,
        team_id: row.team_id,
        label: row.label,
        closed_at: row.closed_at,
        inflight: InflightView {
            dispatch: inflight.dispatch,
            foreign: inflight.foreign,
        },
        tasks: TasksView {
            today: TodayView {
                completed: today.completed,
                failed: today.failed,
                active: today.active,
                waiting: today.waiting,
            },
            recent: recent.into_iter().map(RecentTaskView::from).collect(),
        },
        agent_count,
        skill_count,
    }))
}
