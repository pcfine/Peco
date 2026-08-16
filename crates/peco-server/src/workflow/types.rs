// ============================================================================
// Workflow API 请求/响应类型
// ============================================================================

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Workflow 定义相关 ──────────────────────────────────────────────

/// 创建 Workflow 的请求体。
#[derive(Debug, Deserialize)]
pub struct CreateWorkflowRequest {
    pub yaml: String,
}

/// 更新 Workflow 的请求体。
#[derive(Debug, Deserialize)]
pub struct UpdateWorkflowRequest {
    pub yaml: String,
}

/// Workflow 列表项的响应体。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowListItem {
    pub name: String,
    pub description: String,
    pub version: String,
    pub step_count: usize,
    pub schedule: Option<ScheduleInfo>,
    pub last_execution: Option<ExecutionSummary>,
    pub created_at: String,
    pub updated_at: String,
}

/// 调度信息摘要（来自 workflow_schedules 表）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleInfo {
    pub cron: String,
    pub enabled: bool,
    pub timezone: Option<String>,
}

/// Workflow 详情的响应体。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDetailResponse {
    pub name: String,
    pub description: String,
    pub version: String,
    pub timeout_seconds: Option<u64>,
    pub inputs: std::collections::HashMap<String, Value>,
    pub step_count: usize,
    pub yaml: String,
    pub schedule: Option<ScheduleInfo>,
    pub last_execution: Option<ExecutionSummary>,
}

// ── 执行相关 ──────────────────────────────────────────────────────

/// 手动触发执行的请求体。
#[derive(Debug, Deserialize)]
pub struct ExecuteWorkflowRequest {
    #[serde(default)]
    pub inputs: Option<std::collections::HashMap<String, Value>>,
}

/// 执行触发响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteResponse {
    pub run_id: String,
    pub workflow_name: String,
    pub status: String,
    pub trigger_type: String,
    pub started_at: String,
}

/// 执行摘要（列表项/详情用的公共字段）。
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSummary {
    pub run_id: String,
    pub workflow_name: String,
    pub trigger_type: String,
    pub status: String,
    pub total_steps: usize,
    pub steps_completed: usize,
    pub steps_failed: usize,
    pub steps_skipped: usize,
    pub total_duration_ms: Option<i64>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// 执行列表响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionListResponse {
    pub executions: Vec<ExecutionSummary>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

/// 执行详情响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionDetailResponse {
    pub summary: ExecutionSummary,
    pub inputs: Option<Value>,
    pub error: Option<String>,
    pub step_results: Vec<StepResultResponse>,
}

/// 步骤结果响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepResultResponse {
    pub step_id: String,
    pub step_name: String,
    pub step_type: String,
    /// "success" | "failed" | "skipped" — 精确匹配，不用 Debug 格式化
    pub outcome: String,
    /// 失败时的错误信息（仅 outcome = "failed" 时有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 跳过原因（仅 outcome = "skipped" 时有值，如 "condition false"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub output: Option<String>,
    pub duration_ms: u64,
    pub attempt: usize,
}

/// 执行列表查询参数。
#[derive(Debug, Deserialize)]
pub struct ExecutionQueryParams {
    #[serde(default)]
    pub workflow_name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub trigger_type: Option<String>,
    #[serde(default = "default_offset")]
    pub offset: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_offset() -> i64 {
    0
}
fn default_limit() -> i64 {
    20
}

// ── 执行控制相关 ──────────────────────────────────────────────────

/// 审批请求体。
#[derive(Debug, Deserialize)]
pub struct ApproveRequest {
    pub decision: String,
    #[serde(default)]
    pub note: Option<String>,
}

// ── 统计相关 ──────────────────────────────────────────────────────

/// 统计查询参数。
#[derive(Debug, Deserialize)]
pub struct StatisticsQuery {
    #[serde(default = "default_days")]
    pub days: u32,
}

fn default_days() -> u32 {
    30
}

/// Workflow 统计响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatisticsResponse {
    pub workflow_name: String,
    pub total_runs: i64,
    pub success_count: i64,
    pub failure_count: i64,
    pub cancelled_count: i64,
    pub timed_out_count: i64,
    pub success_rate: f64,
    pub avg_duration_ms: f64,
    pub min_duration_ms: i64,
    pub max_duration_ms: i64,
    pub last_run: Option<ExecutionSummary>,
    pub run_history_30d: Vec<DailyRunStat>,
    pub step_stats: Vec<StepStatResponse>,
}

/// 每日运行统计。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyRunStat {
    pub date: String,
    pub total: i64,
    pub success: i64,
    pub failure: i64,
}

/// 步骤统计。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepStatResponse {
    pub step_id: String,
    pub step_name: String,
    pub avg_duration_ms: f64,
    pub failure_rate: f64,
}

/// 通用成功响应。
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
}
