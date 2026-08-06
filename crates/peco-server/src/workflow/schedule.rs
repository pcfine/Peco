// ============================================================================
// ScheduleConfig — 调度配置类型定义
// ============================================================================

use serde::{Deserialize, Serialize};

/// 调度配置 — 存储在 workflow_schedules 表中，独立于 workflow.md。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleConfig {
    pub workflow_name: String,
    pub cron: String,
    pub enabled: bool,
    pub timezone: Option<String>,
    pub user_id: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 创建调度的请求体。
#[derive(Debug, Deserialize)]
pub struct CreateScheduleRequest {
    pub workflow_name: String,
    pub cron: String,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// 部分更新调度的请求体（PATCH，全部字段可选）。
#[derive(Debug, Deserialize)]
pub struct UpdateScheduleRequest {
    pub cron: Option<String>,
    pub enabled: Option<bool>,
    pub timezone: Option<String>,
}

/// 完整替换调度的请求体（PUT）。
#[derive(Debug, Deserialize)]
pub struct ReplaceScheduleRequest {
    pub cron: String,
    pub enabled: bool,
    pub timezone: Option<String>,
}

/// 调度配置的 API 响应体（不含 user_id）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleResponse {
    pub workflow_name: String,
    pub cron: String,
    pub enabled: bool,
    pub timezone: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
