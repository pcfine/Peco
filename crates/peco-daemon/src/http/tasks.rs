//! 任务面：跨空间派发台账的只读查询 + 空间 TaskHub 的实时观察。
//!
//! 台账由 `space::tasks::TaskObserver` 写入；HTTP 只做筛选、投影与 404。
//! ws_id 与目录的映射始终来自 `workspaces` 行，不维护第二份缓存。
//!
//! 实时面（`/live`、`/stream`、`/cancel`）只读 Hub 内存，不碰台账：任务在飞时
//! 完整输出只在 Hub 里，空间一关就没有了 —— 那时回落到台账的 512 字摘要。

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use chrono::{DateTime, Utc};
use futures::Stream;
use peco_core::machine::{
    Artifact, MachineError, ObsEvent, ObsStream, OutputView, Progress, TaggedJson, TaskFilter,
    TaskId, TaskOutcome, TaskSnapshot, Update,
};
use peco_core::tools::{WorkspaceHub, WorkspaceToolError};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use crate::db::dispatches::{self, DispatchFilter, DispatchRow};
use crate::db::workspaces;
use crate::error::DaemonError;
use crate::state::AppState;

/// 实时观察流的心跳：反代与浏览器都会掐掉长时间静默的连接。
const HEARTBEAT: Duration = Duration::from_secs(15);

/// UI 需要的台账视图。
///
/// 任务态与唤醒态是两套独立状态：一条任务可以已经 `completed`，而唤醒仍是
/// `pending`（还没叫醒 Peco）。`wake_skip_reason` 只在 `wake_state='skipped'`
/// 时给值，让「为什么没叫醒」不再需要翻库或看日志。
#[derive(Debug, Serialize)]
pub struct TaskView {
    pub id: String,
    pub task_id: String,
    pub target_ws_id: String,
    pub target_work_dir: String,
    pub target_team_id: String,
    pub origin_ws_id: String,
    pub origin_kind: String,
    pub agent_name: String,
    pub task_state: String,
    pub wake_state: String,
    pub wake_skip_reason: Option<String>,
    pub created_at: String,
    pub terminal_at: Option<String>,
    pub result_digest: Option<String>,
    /// 派发入参里那段任务描述的前 512 字；旧行是 `null`（当时没有记录）。
    pub prompt_digest: Option<String>,
}

impl From<DispatchRow> for TaskView {
    fn from(row: DispatchRow) -> Self {
        Self {
            id: row.id,
            task_id: row.task_id,
            target_ws_id: row.target_ws_id,
            target_work_dir: row.target_work_dir,
            target_team_id: row.target_team_id,
            origin_ws_id: row.origin_ws_id,
            origin_kind: row.origin_kind,
            agent_name: row.agent_name,
            task_state: row.task_state,
            wake_state: row.wake_state,
            wake_skip_reason: row.wake_skip_reason,
            created_at: row.created_at,
            terminal_at: row.terminal_at,
            result_digest: row.result_digest,
            prompt_digest: row.prompt_digest,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub target_ws_id: Option<String>,
    pub origin_ws_id: Option<String>,
    pub task_state: Option<String>,
    /// 改名前的查询参数；外部脚本还带着 `?state=`，认它一版，避免静默放宽成
    /// 「不过滤」——那会让筛选看起来失效而不是报错。
    #[serde(rename = "state")]
    pub legacy_state: Option<String>,
    pub limit: Option<i64>,
}

impl ListQuery {
    /// 两个参数同时出现时以 `task_state` 为准。
    fn task_state_filter(&self) -> Option<&str> {
        self.task_state.as_deref().or(self.legacy_state.as_deref())
    }
}

/// `GET /api/tasks?target_ws_id=&origin_ws_id=&task_state=&limit=`
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<TaskView>>, DaemonError> {
    let rows = dispatches::list(
        &state.pool,
        DispatchFilter {
            target_ws_id: query.target_ws_id.as_deref(),
            origin_ws_id: query.origin_ws_id.as_deref(),
            task_state: query.task_state_filter(),
            limit: query.limit,
        },
    )
    .await?;
    Ok(Json(rows.into_iter().map(TaskView::from).collect()))
}

/// `GET /api/spaces/{ws_id}/tasks` —— 该空间作为 target 的台账。
pub async fn list_for_space(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
) -> Result<Json<Vec<TaskView>>, DaemonError> {
    if workspaces::by_id(&state.pool, &ws_id).await?.is_none() {
        return Err(DaemonError::SpaceNotFound { ws_id });
    }
    let rows = dispatches::list(
        &state.pool,
        DispatchFilter {
            target_ws_id: Some(ws_id.as_str()),
            limit: Some(200),
            ..DispatchFilter::default()
        },
    )
    .await?;
    Ok(Json(rows.into_iter().map(TaskView::from).collect()))
}

/// `GET /api/tasks/{task_id}` —— task_id 是 Hub 发放的 UUID，全局唯一。
pub async fn detail(
    State(state): State<AppState>,
    UrlPath(task_id): UrlPath<String>,
) -> Result<Json<TaskView>, DaemonError> {
    dispatches::get_by_task_id(&state.pool, &task_id)
        .await?
        .map(TaskView::from)
        .map(Json)
        .ok_or(DaemonError::TaskNotFound { task_id })
}

// ============================================================================
// 空间实时观察：TaskHub 内存快照 + 增量
// ============================================================================

/// Hub 快照的对外子集：`deadline` / `source` / `labels` / `scope` / `last_seq`
/// 是执行体与调度的内部细节，UI 不消费，故不外露。
///
/// `output.tail` 保持字节数组而不是转成字符串：增量片可能切断一个多字节字符，
/// 在这里解码会把半截字符换成替换符，客户端就没法再把片拼成原文。
#[derive(Debug, Serialize)]
pub struct LiveTaskView {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub state_detail: Option<TaggedJson>,
    pub progress: Progress,
    pub output: OutputView,
    pub outcome: Option<TaskOutcome>,
    pub artifacts: Vec<Artifact>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<TaskSnapshot> for LiveTaskView {
    fn from(snapshot: TaskSnapshot) -> Self {
        Self {
            id: snapshot.id.to_string(),
            kind: snapshot.kind,
            // 与台账同一套状态串：两处各写一份会让 UI 的映射表分叉。
            state: snapshot.state.as_str().to_string(),
            state_detail: snapshot.state_detail,
            progress: snapshot.progress,
            output: snapshot.output,
            outcome: snapshot.outcome,
            artifacts: snapshot.artifacts,
            created_at: snapshot.created_at,
            updated_at: snapshot.updated_at,
        }
    }
}

/// `GET /api/spaces/{ws_id}/tasks/live`
///
/// 空间没打开是 409，不是空数组：空数组会让「这个空间的 Hub 不在」看起来像
/// 「空间开着、只是没有任务」，前端于是把在飞任务显示成已完成。
pub async fn live(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
) -> Result<Json<Vec<LiveTaskView>>, DaemonError> {
    let work_dir = work_dir_of(&state, &ws_id).await?;
    let snapshots = state
        .hub
        .list_tasks(&work_dir, &TaskFilter::default())
        .await
        .map_err(|err| observation_error(err, &ws_id, None))?;
    Ok(Json(
        snapshots.into_iter().map(LiveTaskView::from).collect(),
    ))
}

/// `GET /api/spaces/{ws_id}/tasks/{task_id}/stream` —— 首帧快照 + 后续增量。
///
/// 帧名固定两种：`snapshot` 是权威视图（重订阅后也会再发一帧），`update` 是
/// 增量。客户端因此不需要区分「首帧」与「补偿帧」，逻辑只有一条：快照覆盖，
/// 增量追加。
pub async fn stream(
    State(state): State<AppState>,
    UrlPath((ws_id, task_id)): UrlPath<(String, String)>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, DaemonError> {
    let work_dir = work_dir_of(&state, &ws_id).await?;
    let task = parse_task_id(&task_id)?;
    // 先订阅一次再交还流：任务不存在 / 空间没打开是 HTTP 错误，不该被塞进一个
    // 立刻结束的 200 事件流里。
    let first = state
        .hub
        .subscribe_task(&work_dir, task)
        .await
        .map_err(|err| observation_error(err, &ws_id, Some(&task_id)))?;

    let frames = futures::stream::unfold(
        StreamState {
            hub: state.hub.clone(),
            work_dir,
            task,
            stream: first,
        },
        |mut state| async move {
            loop {
                // 先取出事件再匹配：`Lagged` 分支要换掉 `state.stream`，借用必须
                // 在此之前结束。
                let event = state.stream.next().await;
                match event {
                    Some(ObsEvent::Snapshot(snapshot)) => {
                        return Some((snapshot_frame(snapshot), state));
                    }
                    Some(ObsEvent::Update(update)) => return Some((update_frame(&update), state)),
                    // 丢帧后本地视图已不连续：重新订阅，下一帧就是新快照。
                    Some(ObsEvent::Lagged { dropped }) => {
                        warn!(
                            task_id = %state.task,
                            dropped,
                            "task observation stream lagged; resubscribing for a fresh snapshot"
                        );
                        match state.hub.subscribe_task(&state.work_dir, state.task).await {
                            Ok(stream) => state.stream = stream,
                            Err(err) => {
                                warn!(task_id = %state.task, error = %err, "cannot resubscribe");
                                return None;
                            }
                        }
                    }
                    // 终态已无空洞地投递完，或空间已关闭：连接跟着结束。
                    Some(ObsEvent::End) | None => return None,
                }
            }
        },
    );
    Ok(Sse::new(frames).keep_alive(KeepAlive::new().interval(HEARTBEAT).text("ping")))
}

/// 观察流的当前订阅；`Lagged` 后换成一条新流（其首帧仍是快照）。
struct StreamState {
    hub: Arc<WorkspaceHub>,
    work_dir: PathBuf,
    task: TaskId,
    stream: ObsStream,
}

fn snapshot_frame(snapshot: TaskSnapshot) -> Result<Event, Infallible> {
    to_event("snapshot", &LiveTaskView::from(snapshot))
}

fn update_frame(update: &Update) -> Result<Event, Infallible> {
    to_event("update", update)
}

fn to_event<T: Serialize>(name: &str, payload: &T) -> Result<Event, Infallible> {
    match Event::default().event(name).json_data(payload) {
        Ok(event) => Ok(event),
        // 序列化失败只可能来自上游 serde：留一条注释帧比 panic 掉整个连接好。
        Err(err) => {
            warn!(error = %err, "task stream payload is not serializable");
            Ok(Event::default().comment("unserializable task frame"))
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CancelView {
    pub cancelled: bool,
}

/// `POST /api/spaces/{ws_id}/tasks/{task_id}/cancel`
///
/// 只转发请求：取消的生效边界属执行体，Hub 在宽限期后兜底收敛。已终态的任务
/// 收到取消是幂等成功，故这里恒答 `cancelled: true`，真实终态由 `/live` 或
/// `/stream` 决定。
pub async fn cancel(
    State(state): State<AppState>,
    UrlPath((ws_id, task_id)): UrlPath<(String, String)>,
) -> Result<Json<CancelView>, DaemonError> {
    let work_dir = work_dir_of(&state, &ws_id).await?;
    let task = parse_task_id(&task_id)?;
    state
        .hub
        .cancel_task(&work_dir, task)
        .await
        .map_err(|err| observation_error(err, &ws_id, Some(&task_id)))?;
    Ok(Json(CancelView { cancelled: true }))
}

/// 实时观察都按空间行定位目录：台账、Hub 索引与签名用的都是同一个 `work_dir`。
///
/// 只查行，不查打开状态：`WorkspaceHub` 的实时方法自己会拒未打开的目录，两处
/// 各判一次会让「什么算未打开」多出第二份口径。
async fn work_dir_of(state: &AppState, ws_id: &str) -> Result<PathBuf, DaemonError> {
    let row = workspaces::by_id(&state.pool, ws_id)
        .await?
        .ok_or_else(|| DaemonError::SpaceNotFound {
            ws_id: ws_id.to_string(),
        })?;
    Ok(PathBuf::from(&row.work_dir))
}

/// 路径里的 task_id 来自 Hub 发放的 UUID；别的形状一定不是本进程发出去的任务。
fn parse_task_id(task_id: &str) -> Result<TaskId, DaemonError> {
    Uuid::parse_str(task_id)
        .map(TaskId)
        .map_err(|_| DaemonError::TaskNotFound {
            task_id: task_id.to_string(),
        })
}

/// `WorkspaceToolError` → daemon 稳定错误码：区分「空间没开」「任务不存在」与
/// 其它内部故障，前者是调用方能处理的，后者不是。
///
/// 空间没开是 409 而不是空数据：实时面没有可回落的数据源，装作「没有任务」会
/// 让前端把一个在飞任务显示成已完成。
fn observation_error(err: WorkspaceToolError, ws_id: &str, task_id: Option<&str>) -> DaemonError {
    match err {
        WorkspaceToolError::NotOpen { .. } => DaemonError::SpaceNotOpen {
            ws_id: ws_id.to_string(),
        },
        WorkspaceToolError::Machine(MachineError::NotFound { id }) => DaemonError::TaskNotFound {
            task_id: task_id.map_or_else(|| id.to_string(), str::to_string),
        },
        other => DaemonError::Internal {
            message: other.to_string(),
        },
    }
}
