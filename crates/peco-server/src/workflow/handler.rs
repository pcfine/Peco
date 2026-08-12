// ============================================================================
// Workflow API Handlers — CRUD + 执行 + 统计
// ============================================================================

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use peco_core::workflow::{StepOutcome, WorkflowConfig};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

use super::types::*;

// ── 辅助函数 ────────────────────────────────────────────────────────────────

/// 获取文件修改时间的 ISO 8601 字符串。
fn file_mtime(path: &std::path::Path) -> String {
    path.metadata()
        .and_then(|m| m.modified())
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
        .unwrap_or_default()
}

/// 从 DB 行构建 ExecutionSummary。
fn row_to_summary(r: &crate::db::workflow_executions::WorkflowExecutionRow) -> ExecutionSummary {
    ExecutionSummary {
        run_id: r.id.clone(),
        workflow_name: r.workflow_name.clone(),
        trigger_type: r.trigger_type.clone(),
        status: r.status.clone(),
        total_steps: r.total_steps as usize,
        steps_completed: r.steps_completed as usize,
        steps_failed: r.steps_failed as usize,
        steps_skipped: r.steps_skipped as usize,
        total_duration_ms: r.total_duration_ms,
        started_at: r.started_at.clone(),
        finished_at: r.finished_at.clone(),
    }
}

// ── Workflow 定义 CRUD ──────────────────────────────────────────────────────

/// `GET /api/workflows` — 列出所有 Workflow 定义。
pub async fn list_workflows(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<WorkflowListItem>>, ApiError> {
    let ws = state.workspace_manager.get(&user_id)?;
    let metas = ws.workflow_manager().list_meta();

    let schedules = super::helper::get_user_schedules(&state.db, &user_id).await;
    let recent_executions = super::helper::get_recent_executions(&state.db, &user_id).await;

    let mut items = Vec::with_capacity(metas.len());
    for meta in &metas {
        let path = ws
            .workflow_manager()
            .workflows_dir()
            .join(&meta.name)
            .join("workflow.md");

        let schedule = schedules.get(&meta.name).map(|s| ScheduleInfo {
            cron: s.cron.clone(),
            enabled: s.enabled,
            timezone: s.timezone.clone(),
        });

        let last_execution = recent_executions.get(&meta.name).cloned();

        items.push(WorkflowListItem {
            name: meta.name.clone(),
            description: meta.description.clone(),
            version: meta.version.clone(),
            step_count: meta.step_count,
            schedule,
            last_execution,
            created_at: file_mtime(&path),
            updated_at: file_mtime(&path),
        });
    }

    Ok(Json(items))
}

/// `POST /api/workflows` — 创建新 Workflow。
pub async fn create_workflow(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateWorkflowRequest>,
) -> Result<(StatusCode, Json<WorkflowDetailResponse>), ApiError> {
    // 解析 YAML 提取 name
    let parsed: serde_json::Value = serde_yaml::from_str(&body.yaml)
        .map_err(|e| ApiError::BadRequest(format!("YAML parse error: {e}")))?;
    let name = parsed
        .get("workflow")
        .and_then(|w| w.get("name"))
        .and_then(|n| n.as_str())
        .ok_or_else(|| ApiError::BadRequest("YAML must contain 'workflow.name'".into()))?;

    let ws = state.workspace_manager.get(&user_id)?;

    // 创建
    let definition = ws
        .workflow_manager()
        .create(name, &body.yaml)
        .map_err(|e| match e {
            peco_core::workflow::WorkflowError::AlreadyExists(_) => {
                ApiError::Conflict(e.to_string())
            }
            peco_core::workflow::WorkflowError::InvalidName(_) => {
                ApiError::BadRequest(e.to_string())
            }
            e => ApiError::Internal(format!("create workflow: {e}")),
        })?;

    let detail = build_detail_response(&state, name, &definition, &body.yaml, &user_id).await;
    Ok((StatusCode::CREATED, Json(detail)))
}

/// `GET /api/workflows/:name` — 获取 Workflow 详情。
pub async fn get_workflow(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<WorkflowDetailResponse>, ApiError> {
    let ws = state.workspace_manager.get(&user_id)?;
    let definition = ws
        .workflow_manager()
        .load(&name)
        .map_err(|e| ApiError::NotFound(format!("workflow '{name}' not found: {e}")))?;

    // 读取原始 workflow.md 内容，保留用户格式、注释和字段顺序。
    // 避免 serde_yaml 往返序列化产生的格式变化。
    let yaml_path = ws
        .workflow_manager()
        .workflows_dir()
        .join(&name)
        .join("workflow.md");
    let yaml = tokio::fs::read_to_string(&yaml_path)
        .await
        .map_err(|e| ApiError::Internal(format!("read workflow.md: {e}")))?;

    let detail = build_detail_response(&state, &name, &definition, &yaml, &user_id).await;
    Ok(Json(detail))
}

/// `PUT /api/workflows/:name` — 更新 Workflow 定义。
pub async fn update_workflow(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<UpdateWorkflowRequest>,
) -> Result<Json<WorkflowDetailResponse>, ApiError> {
    let ws = state.workspace_manager.get(&user_id)?;
    let definition = ws
        .workflow_manager()
        .update(&name, &body.yaml)
        .map_err(|e| ApiError::BadRequest(format!("update workflow: {e}")))?;

    let detail = build_detail_response(&state, &name, &definition, &body.yaml, &user_id).await;
    Ok(Json(detail))
}

/// `DELETE /api/workflows/:name` — 删除 Workflow。
pub async fn delete_workflow(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    // 检查是否有活跃执行
    let active_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM workflow_executions \
         WHERE user_id = ? AND workflow_name = ? AND status IN ('running', 'paused')",
    )
    .bind(&user_id)
    .bind(&name)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if active_count > 0 {
        return Err(ApiError::Conflict(format!(
            "workflow '{name}' has {active_count} active execution(s); cancel them first"
        )));
    }

    // 先从调度器移除（如有）
    if state
        .cron_scheduler
        .contains_workflow(&name, &user_id)
        .await
    {
        let _ = state.cron_scheduler.remove_workflow(&name, &user_id).await;
    }

    // 删除调度记录
    let _ = crate::db::workflow_schedules::delete(&state.db, &user_id, &name).await;

    // 删除 workflow 定义文件
    let ws = state.workspace_manager.get(&user_id)?;
    ws.workflow_manager()
        .delete(&name)
        .map_err(|e| ApiError::NotFound(format!("workflow '{name}' not found: {e}")))?;

    Ok(Json(SuccessResponse { success: true }))
}

// ── 执行操作 ────────────────────────────────────────────────────────────────

/// `POST /api/workflows/:name/execute` — 手动触发执行。
pub async fn execute_workflow(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<ExecuteWorkflowRequest>,
) -> Result<(StatusCode, Json<ExecuteResponse>), ApiError> {
    let ws = state
        .workspace_manager
        .get(&user_id)
        .map_err(|e| ApiError::Internal(format!("get workspace: {e}")))?;

    let definition = ws
        .workflow_manager()
        .load(&name)
        .map_err(|e| ApiError::NotFound(format!("workflow '{name}' not found: {e}")))?;

    let inputs = body.inputs.unwrap_or_default();
    definition
        .validate_inputs(&inputs)
        .map_err(|e| ApiError::BadRequest(format!("input validation failed: {e}")))?;

    // 创建 per-user persister
    let persister = Arc::new(state.workflow_persister_for(&user_id));
    let run_id = Uuid::new_v4().to_string();
    let started_at = chrono::Utc::now();

    // 写入初始执行记录
    crate::db::workflow_executions::insert(
        &state.db,
        &crate::db::workflow_executions::CreateExecutionParams {
            id: run_id.clone(),
            user_id: user_id.clone(),
            workflow_name: name.clone(),
            trigger_type: "manual".to_string(),
            inputs_json: serde_json::to_string(&inputs).ok(),
            total_steps: definition.steps.len(),
            started_at: started_at.to_rfc3339(),
        },
    )
    .await
    .map_err(|e| ApiError::Internal(format!("insert execution: {e}")))?;

    // 启动引擎（后台任务）。ws 自身实现 AgentAccess，直接传入。
    let config = WorkflowConfig::default();
    let handle = ws
        .workflow_manager()
        .execute(&name, ws.clone(), persister.clone(), config, inputs)
        .map_err(|e| ApiError::Internal(format!("execute workflow: {e}")))?;

    // 注册到活跃执行表
    super::active::insert_run(&run_id, handle).await;

    Ok((
        StatusCode::ACCEPTED,
        Json(ExecuteResponse {
            run_id,
            workflow_name: name.clone(),
            status: "running".to_string(),
            trigger_type: "manual".to_string(),
            started_at: started_at.to_rfc3339(),
        }),
    ))
}

/// `GET /api/workflows/executions` — 执行历史列表。
pub async fn list_executions(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ExecutionQueryParams>,
) -> Result<Json<ExecutionListResponse>, ApiError> {
    let rows = crate::db::workflow_executions::list_by_user_with_filters(
        &state.db,
        &user_id,
        params.workflow_name.as_deref(),
        params.status.as_deref(),
        params.trigger_type.as_deref(),
        params.offset,
        params.limit,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("list executions: {e}")))?;

    let total = crate::db::workflow_executions::count_by_user_with_filters(
        &state.db,
        &user_id,
        params.workflow_name.as_deref(),
        params.status.as_deref(),
        params.trigger_type.as_deref(),
    )
    .await
    .unwrap_or(0);

    let executions: Vec<ExecutionSummary> = rows.iter().map(row_to_summary).collect();

    Ok(Json(ExecutionListResponse {
        executions,
        total,
        offset: params.offset,
        limit: params.limit,
    }))
}

/// `GET /api/workflows/executions/:run_id` — 执行详情。
pub async fn get_execution(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Result<Json<ExecutionDetailResponse>, ApiError> {
    let row = crate::db::workflow_executions::find_by_id_and_user(&state.db, &run_id, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("get execution: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("execution '{run_id}' not found")))?;

    let summary = row_to_summary(&row);

    let inputs: Option<serde_json::Value> = row
        .inputs_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());

    // 从 snapshot_json 提取 step_results 和 workflow-level error
    let (step_results, snapshot_error) = row
        .snapshot_json
        .as_deref()
        .and_then(|s| {
            let parsed: serde_json::Value = serde_json::from_str(s).ok()?;
            let results: HashMap<String, peco_core::workflow::StepResult> =
                serde_json::from_value(parsed.get("step_results")?.clone()).ok()?;
            let snap_err: Option<String> = parsed
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let step_results = results
                .into_iter()
                .map(|(id, r)| {
                    let (outcome, error, reason) = match &r.outcome {
                        StepOutcome::Success(_) => ("success".to_string(), None, None),
                        StepOutcome::Skipped(reason) => {
                            ("skipped".to_string(), None, Some(reason.clone()))
                        }
                        StepOutcome::Failed(err) => ("failed".to_string(), Some(err.clone()), None),
                    };
                    StepResultResponse {
                        step_id: id,
                        step_name: r.step.name.clone(),
                        step_type: format!("{:?}", r.step.step_type).to_lowercase(),
                        outcome,
                        error,
                        reason,
                        output: r.output,
                        duration_ms: r.duration.as_millis() as u64,
                        attempt: r.attempt,
                    }
                })
                .collect::<Vec<_>>();
            Some((step_results, snap_err))
        })
        .unwrap_or_default();

    // Prefer snapshot-level error (from engine), fall back to DB row error
    let execution_error = snapshot_error.or(row.error);

    Ok(Json(ExecutionDetailResponse {
        summary,
        inputs,
        error: execution_error,
        step_results,
    }))
}

/// `POST /api/workflows/executions/:run_id/cancel` — 取消执行。
pub async fn cancel_execution(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    // 验证执行记录归属当前用户
    let _row = crate::db::workflow_executions::find_by_id_and_user(&state.db, &run_id, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("get execution: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("execution '{run_id}' not found")))?;

    super::active::cancel_run(&run_id).await;
    Ok(Json(SuccessResponse { success: true }))
}

/// `POST /api/workflows/executions/:run_id/approve` — 审批决策。
pub async fn approve_execution(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
    Json(body): Json<ApproveRequest>,
) -> Result<Json<SuccessResponse>, ApiError> {
    // 验证执行记录归属当前用户
    let _row = crate::db::workflow_executions::find_by_id_and_user(&state.db, &run_id, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("get execution: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("execution '{run_id}' not found")))?;

    let decision = match body.decision.as_str() {
        "proceed" => peco_core::workflow::ApprovalDecision::Proceed,
        "abort" => peco_core::workflow::ApprovalDecision::Abort,
        _ => {
            return Err(ApiError::BadRequest(
                "decision must be 'proceed' or 'abort'".into(),
            ));
        }
    };
    super::active::approve_run(&run_id, decision, body.note).await;
    Ok(Json(SuccessResponse { success: true }))
}

// ── 统计 ────────────────────────────────────────────────────────────────────

/// 统计缓存（60 秒 TTL）。
/// Key: (user_id, workflow_name, days) — user_id 确保跨用户隔离。
static STATS_CACHE: std::sync::LazyLock<
    Mutex<HashMap<(String, String, u32), (Instant, StatisticsResponse)>>,
> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// `GET /api/workflows/:name/statistics` — 获取 Workflow 统计。
pub async fn get_statistics(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<StatisticsQuery>,
) -> Result<Json<StatisticsResponse>, ApiError> {
    let days = query.days.min(365);

    // 检查缓存
    {
        let cache = STATS_CACHE.lock().unwrap();
        if let Some((ts, stats)) = cache.get(&(user_id.clone(), name.clone(), days)) {
            if ts.elapsed().as_secs() < 60 {
                return Ok(Json(stats.clone()));
            }
        }
    }

    // 验证 Workflow 存在
    let ws = state.workspace_manager.get(&user_id)?;
    ws.workflow_manager()
        .load(&name)
        .map_err(|_| ApiError::NotFound(format!("workflow '{name}' not found")))?;

    // 聚合查询
    let stats = compute_statistics(&state.db, &user_id, &name, days).await?;

    // 写入缓存
    {
        let mut cache = STATS_CACHE.lock().unwrap();
        cache.insert((user_id, name, days), (Instant::now(), stats.clone()));
    }

    Ok(Json(stats))
}

// ── 内部辅助 ────────────────────────────────────────────────────────────────

async fn build_detail_response(
    state: &AppState,
    name: &str,
    definition: &peco_core::workflow::WorkflowDefinition,
    yaml: &str,
    user_id: &str,
) -> WorkflowDetailResponse {
    let schedules = super::helper::get_user_schedules(&state.db, user_id).await;
    let recent = super::helper::get_recent_executions(&state.db, user_id).await;

    WorkflowDetailResponse {
        name: definition.name.clone(),
        description: definition.description.clone(),
        version: definition.version.clone(),
        timeout_seconds: definition.timeout_seconds,
        inputs: definition
            .inputs
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    serde_json::json!({
                        "type": v.input_type,
                        "description": v.description,
                        "required": v.required,
                        "default": v.default,
                    }),
                )
            })
            .collect(),
        step_count: definition.steps.len(),
        yaml: yaml.to_string(),
        schedule: schedules.get(name).map(|s| ScheduleInfo {
            cron: s.cron.clone(),
            enabled: s.enabled,
            timezone: s.timezone.clone(),
        }),
        last_execution: recent.get(name).cloned(),
    }
}

async fn compute_statistics(
    pool: &sqlx::SqlitePool,
    user_id: &str,
    workflow_name: &str,
    days: u32,
) -> Result<StatisticsResponse, ApiError> {
    use sqlx::Row;

    let cutoff = chrono::Utc::now() - chrono::Duration::days(days as i64);
    let cutoff_str = cutoff.to_rfc3339();

    // 基础聚合
    let stats_row = sqlx::query(
        "SELECT \
         COUNT(*) as total, \
         SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END) as success, \
         SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) as failure, \
         SUM(CASE WHEN status = 'cancelled' THEN 1 ELSE 0 END) as cancelled, \
         AVG(total_duration_ms) as avg_dur, \
         MIN(total_duration_ms) as min_dur, \
         MAX(total_duration_ms) as max_dur \
         FROM workflow_executions \
         WHERE user_id = ? AND workflow_name = ? AND started_at >= ?",
    )
    .bind(user_id)
    .bind(workflow_name)
    .bind(&cutoff_str)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(format!("aggregate stats: {e}")))?;

    let total: i64 = stats_row.get(0);
    let success: i64 = stats_row.get(1);
    let failure: i64 = stats_row.get(2);
    let cancelled: i64 = stats_row.get(3);
    let avg_dur: Option<f64> = stats_row.get(4);
    let min_dur: Option<i64> = stats_row.get(5);
    let max_dur: Option<i64> = stats_row.get(6);

    let success_rate = if total > 0 {
        success as f64 / total as f64
    } else {
        0.0
    };

    // 最近执行
    let last_execution = crate::db::workflow_executions::list_by_user_and_workflow(
        pool,
        user_id,
        workflow_name,
        0,
        1,
    )
    .await
    .ok()
    .and_then(|rows| rows.first().map(row_to_summary));

    // 按日统计
    let daily_rows = sqlx::query(
        "SELECT date(started_at) as d, \
         COUNT(*) as cnt, \
         SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END) as s, \
         SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) as f \
         FROM workflow_executions \
         WHERE user_id = ? AND workflow_name = ? AND started_at >= ? \
         GROUP BY d ORDER BY d ASC",
    )
    .bind(user_id)
    .bind(workflow_name)
    .bind(&cutoff_str)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(format!("daily stats: {e}")))?;

    let run_history: Vec<DailyRunStat> = daily_rows
        .iter()
        .map(|r| DailyRunStat {
            date: r.get::<String, _>(0),
            total: r.get(1),
            success: r.get(2),
            failure: r.get(3),
        })
        .collect();

    Ok(StatisticsResponse {
        workflow_name: workflow_name.to_string(),
        total_runs: total,
        success_count: success,
        failure_count: failure,
        cancelled_count: cancelled,
        success_rate,
        avg_duration_ms: avg_dur.unwrap_or(0.0),
        min_duration_ms: min_dur.unwrap_or(0),
        max_duration_ms: max_dur.unwrap_or(0),
        last_run: last_execution,
        run_history_30d: run_history,
        step_stats: vec![], // Phase 1: step-level stats require JSON parsing, skip
    })
}
