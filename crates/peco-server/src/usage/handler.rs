// Usage Handler — Token 用量统计

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    #[serde(default = "default_period")]
    pub period: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub agent: Option<String>,
}

fn default_period() -> String {
    "7d".to_string()
}

#[derive(Debug, Serialize)]
pub struct UsageSummary {
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_requests: i64,
    pub by_agent: Vec<AgentUsage>,
}

#[derive(Debug, Serialize)]
pub struct AgentUsage {
    pub agent_name: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub request_count: i64,
}

pub async fn get_summary(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<UsageQuery>,
) -> Result<Json<UsageSummary>, ApiError> {
    let period_expr = match params.period.as_str() {
        "7d" => "-7 days",
        "30d" => "-30 days",
        "90d" => "-90 days",
        _ => "-7 days",
    };

    // Aggregate from usage_logs
    let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT agent_name, \
         COALESCE(SUM(input_tokens), 0) as total_input, \
         COALESCE(SUM(output_tokens), 0) as total_output, \
         COUNT(*) as cnt \
         FROM usage_logs \
         WHERE user_id = ? AND created_at > datetime('now', ?) \
         GROUP BY agent_name \
         ORDER BY total_input + total_output DESC",
    )
    .bind(&user_id)
    .bind(period_expr)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::Internal(format!("query failed: {e}")))?;

    let by_agent: Vec<AgentUsage> = rows
        .iter()
        .map(|(name, input, output, cnt)| AgentUsage {
            agent_name: name.clone(),
            input_tokens: *input,
            output_tokens: *output,
            request_count: *cnt,
        })
        .collect();

    let total_input_tokens: i64 = by_agent.iter().map(|a| a.input_tokens).sum();
    let total_output_tokens: i64 = by_agent.iter().map(|a| a.output_tokens).sum();
    let total_requests: i64 = by_agent.iter().map(|a| a.request_count).sum();

    Ok(Json(UsageSummary {
        total_input_tokens,
        total_output_tokens,
        total_requests,
        by_agent,
    }))
}
