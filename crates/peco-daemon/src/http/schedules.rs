//! `/api/schedules` —— 定时任务的列表、增删改、触发时刻预览、手动运行与执行历史。
//!
//! 每个 handler 只做三件事：校验请求、调领域函数、拼视图。cron 计算与状态机都在
//! [`crate::schedule`] 里，这里重复一遍就会有两份「什么时候该跑」的定义。

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{Local, Utc};
use serde::{Deserialize, Serialize};

use crate::api_error::ApiError;
use crate::db::schedules::{self, NewTask, ScheduledRunRow, ScheduledTaskRow};
use crate::db::workspaces;
use crate::error::DaemonError;
use crate::schedule::scheduler::RunNowError;
use crate::schedule::{cron, utc_text};
use crate::state::AppState;

/// 预览返回几个时刻。
const PREVIEW_COUNT: usize = 3;

/// 执行历史的默认条数与上限：一次 tick 最多 20 行，100 条足够看清最近几小时的走势。
const RUNS_LIMIT_DEFAULT: i64 = 20;
const RUNS_LIMIT_MAX: i64 = 100;

#[derive(Debug, Serialize)]
pub struct ScheduleView {
    pub id: String,
    pub name: String,
    pub target_ws_id: String,
    pub target_label: String,
    pub prompt: String,
    pub cron_expr: String,
    pub enabled: bool,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_status: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<ScheduledTaskRow> for ScheduleView {
    fn from(row: ScheduledTaskRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            target_ws_id: row.target_ws_id,
            target_label: row.target_label,
            prompt: row.prompt,
            cron_expr: row.cron_expr,
            enabled: row.enabled,
            next_run_at: row.next_run_at,
            last_run_at: row.last_run_at,
            last_status: row.last_status,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// 执行历史的视图。时间一律 UTC 文本原样输出 —— 前端的时区换算只在渲染侧做一次。
#[derive(Debug, Serialize)]
pub struct RunView {
    pub id: String,
    pub schedule_id: String,
    pub trigger_kind: String,
    pub scheduled_for: String,
    pub status: String,
    pub attempts: i64,
    pub conversation_id: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub result_digest: Option<String>,
    pub error: Option<String>,
    pub notify_state: String,
    pub notified_at: Option<String>,
}

impl From<ScheduledRunRow> for RunView {
    fn from(row: ScheduledRunRow) -> Self {
        Self {
            id: row.id,
            schedule_id: row.schedule_id,
            trigger_kind: row.trigger_kind,
            scheduled_for: row.scheduled_for,
            status: row.status,
            attempts: row.attempts,
            conversation_id: row.conversation_id,
            started_at: row.started_at,
            finished_at: row.finished_at,
            result_digest: row.result_digest,
            error: row.error,
            notify_state: row.notify_state,
            notified_at: row.notified_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    pub name: String,
    pub target_ws_id: String,
    pub prompt: String,
    pub cron_expr: String,
    /// 省略即启用。
    pub enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateRequest {
    pub name: Option<String>,
    pub target_ws_id: Option<String>,
    pub prompt: Option<String>,
    pub cron_expr: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PreviewRequest {
    pub cron_expr: String,
}

#[derive(Debug, Serialize)]
pub struct PreviewResponse {
    /// UTC 文本，最多 3 个，严格递增。
    pub next: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RunsQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct RunNowResponse {
    pub run_id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct RemovedResponse {
    pub removed: bool,
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<ScheduleView>>, ApiError> {
    let rows = schedules::list_tasks(&state.pool).await?;
    Ok(Json(rows.into_iter().map(ScheduleView::from).collect()))
}

pub async fn create(
    State(state): State<AppState>,
    Json(request): Json<CreateRequest>,
) -> Result<Response, ApiError> {
    let name = required_text(&request.name, "name")?;
    let prompt = required_text(&request.prompt, "prompt")?;
    let cron_expr = validated_cron(&request.cron_expr)?;
    // 空间必须已索引：外键会挡住不存在的 ws_id，但那会变成 500，
    // 而「目标空间不存在」是一次请求错误。
    ensure_space_exists(&state, &request.target_ws_id).await?;
    let enabled = request.enabled.unwrap_or(true);
    let next_run_at = next_run_at(&cron_expr, enabled)?;

    let id = uuid::Uuid::new_v4().to_string();
    schedules::insert_task(
        &state.pool,
        &NewTask {
            id: &id,
            name: &name,
            target_ws_id: &request.target_ws_id,
            prompt: &prompt,
            cron_expr: &cron_expr,
            enabled,
            next_run_at: next_run_at.as_deref(),
        },
    )
    .await?;
    let row = by_id(&state, &id).await?;
    Ok((StatusCode::CREATED, Json(ScheduleView::from(row))).into_response())
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateRequest>,
) -> Result<Json<ScheduleView>, ApiError> {
    let row = by_id(&state, &id).await?;

    let name = match request.name.as_deref() {
        Some(raw) => required_text(raw, "name")?,
        None => row.name.clone(),
    };
    let prompt = match request.prompt.as_deref() {
        Some(raw) => required_text(raw, "prompt")?,
        None => row.prompt.clone(),
    };
    let target_ws_id = match request.target_ws_id.as_deref() {
        Some(ws_id) => {
            ensure_space_exists(&state, ws_id).await?;
            ws_id.to_string()
        }
        None => row.target_ws_id.clone(),
    };
    // 按字符串比较：用户把同一串再发一遍不该被当成改排期。归一化过的历史行会
    // 比出「变了」，于是多算一次下次时刻——代价远小于漏改一条排期。
    let cron_expr = match request.cron_expr.as_deref() {
        Some(raw) => validated_cron(raw)?,
        None => row.cron_expr.clone(),
    };
    let cron_changed = cron_expr != row.cron_expr;
    let enabled = request.enabled.unwrap_or(row.enabled);

    // 只有「什么时候跑」变了才重算下次时刻：改名不该把已经排好的槽位往后推。
    let next_run_at = if cron_changed || enabled != row.enabled {
        next_run_at(&cron_expr, enabled)?
    } else {
        row.next_run_at.clone()
    };

    schedules::replace_task(
        &state.pool,
        &NewTask {
            id: &row.id,
            name: &name,
            target_ws_id: &target_ws_id,
            prompt: &prompt,
            cron_expr: &cron_expr,
            enabled,
            next_run_at: next_run_at.as_deref(),
        },
    )
    .await?;
    let row = by_id(&state, &id).await?;
    Ok(Json(ScheduleView::from(row)))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RemovedResponse>, ApiError> {
    // 先掐在途轮：run 行会随调度行级联消失，那一轮若还在跑就成了没人认领的会话。
    state.schedules.cancel_inflight(&id).await?;
    if schedules::delete_task(&state.pool, &id).await? == 0 {
        return Err(schedule_not_found(&id));
    }
    Ok(Json(RemovedResponse { removed: true }))
}

pub async fn preview(
    Json(request): Json<PreviewRequest>,
) -> Result<Json<PreviewResponse>, ApiError> {
    let cron_expr = validated_cron(&request.cron_expr)?;
    let times =
        cron::next_occurrences(&cron_expr, Local::now(), PREVIEW_COUNT).map_err(invalid_cron)?;
    Ok(Json(PreviewResponse {
        next: times
            .into_iter()
            .map(|at| utc_text(at.with_timezone(&Utc)))
            .collect(),
    }))
}

pub async fn run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunNowResponse>, ApiError> {
    // 停机序列先停调度再收尾在飞轮：此时手动触发的这一轮注定被取消。
    if state.shutdown.is_cancelled() {
        return Err(DaemonError::ShuttingDown.into());
    }
    match state.schedules.run_now(&id).await {
        Ok(run) => Ok(Json(RunNowResponse {
            run_id: run.run_id,
            status: run.status,
        })),
        Err(RunNowError::NotFound) => Err(schedule_not_found(&id)),
        Err(RunNowError::Busy) => Err(ApiError::coded(
            StatusCode::CONFLICT,
            "schedule_busy",
            format!("schedule '{id}' already has an unfinished run"),
        )),
        Err(RunNowError::Db(e)) => Err(e.into()),
    }
}

pub async fn runs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<RunsQuery>,
) -> Result<Json<Vec<RunView>>, ApiError> {
    // 先确认调度存在：空数组与「没有这条调度」在 UI 上是两种提示。
    by_id(&state, &id).await?;
    let limit = query
        .limit
        .unwrap_or(RUNS_LIMIT_DEFAULT)
        .clamp(1, RUNS_LIMIT_MAX);
    let rows = schedules::runs_by_schedule(&state.pool, &id, limit).await?;
    Ok(Json(rows.into_iter().map(RunView::from).collect()))
}

async fn by_id(state: &AppState, id: &str) -> Result<ScheduledTaskRow, ApiError> {
    schedules::task_by_id(&state.pool, id)
        .await?
        .ok_or_else(|| schedule_not_found(id))
}

fn schedule_not_found(id: &str) -> ApiError {
    ApiError::coded(
        StatusCode::NOT_FOUND,
        "schedule_not_found",
        format!("schedule '{id}' not found"),
    )
}

/// 名字与正文都必须非空：空名字在执行历史里无法辨认，空正文则是一条空消息。
fn required_text(raw: &str, field: &str) -> Result<String, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::coded(
            StatusCode::BAD_REQUEST,
            "invalid_schedule",
            format!("{field} must not be empty"),
        ));
    }
    Ok(text.to_string())
}

/// 校验表达式，返回用户原文（去掉首尾空白）。
///
/// 落库的是原文：归一化只发生在解析侧（[`cron::next_occurrence`] 自己补秒域），
/// 库里因此留着用户写的那串——5 字段的形状能被前端的预设匹配上，编辑时看到的
/// 也是自己填过的文本。
fn validated_cron(raw: &str) -> Result<String, ApiError> {
    let text = raw.trim();
    cron::validate(text).map_err(invalid_cron)?;
    Ok(text.to_string())
}

/// `invalid_cron` 的 details 必须是具体原因：前端只能展示它，解析不了 croner 的文案。
fn invalid_cron(error: cron::CronError) -> ApiError {
    ApiError::coded(
        StatusCode::BAD_REQUEST,
        "invalid_cron",
        format!("invalid cron expression: {error}"),
    )
}

/// 目标空间必须是一个可用空间：回收站里的行还在库里，但它不是合法的排期目标。
async fn ensure_space_exists(state: &AppState, ws_id: &str) -> Result<(), ApiError> {
    match workspaces::by_id(&state.pool, ws_id).await? {
        Some(row) if row.is_trashed() => Err(DaemonError::SpaceTrashed {
            ws_id: ws_id.to_string(),
        }
        .into()),
        Some(_) => Ok(()),
        None => Err(DaemonError::SpaceNotFound {
            ws_id: ws_id.to_string(),
        }
        .into()),
    }
}

/// 下一次触发时刻；停用的调度没有下一次（`NULL`），列表里因此是 `null`。
fn next_run_at(cron_expr: &str, enabled: bool) -> Result<Option<String>, ApiError> {
    if !enabled {
        return Ok(None);
    }
    let at = cron::next_occurrence(cron_expr, Local::now()).map_err(invalid_cron)?;
    Ok(Some(utc_text(at.with_timezone(&Utc))))
}
