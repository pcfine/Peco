// ============================================================================
// Agent Handlers — CRUD 操作
// ============================================================================

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::db::agents::{self, CreateAgentParams, UpdateAgentParams};
use crate::error::ApiError;
use crate::state::AppState;

// ── Request / Response 类型 ─────────────────────────────────────────────────

/// 创建 Agent 的请求体。
#[derive(Debug, Deserialize)]
pub struct CreateAgentRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_icon")]
    pub icon: String,
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

fn default_model() -> String {
    "deepseek-v4-flash".into()
}
fn default_provider() -> String {
    "deepseek".into()
}
fn default_icon() -> String {
    "🤖".into()
}
fn default_color() -> String {
    "#6366f1".into()
}

/// 更新 Agent 的请求体（全部字段可选）。
#[derive(Debug, Deserialize)]
pub struct UpdateAgentRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub tools: Option<Vec<String>>,
    pub mcp_servers: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
}

/// Agent 列表项响应（不含 system_prompt 和 config_json 细节）。
#[derive(Debug, Serialize)]
pub struct AgentListItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub model: String,
    pub provider: String,
    pub icon: String,
    pub color: String,
    pub status: String,
    pub tools: Vec<String>,
    pub created_at: String,
}

/// Agent 详情响应（含完整 system_prompt 和 config）。
#[derive(Debug, Serialize)]
pub struct AgentDetail {
    pub id: String,
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    pub model: String,
    pub provider: String,
    pub icon: String,
    pub color: String,
    pub status: String,
    pub tools: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub skills: Vec<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
}

/// 简单成功响应。
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
}

// ── 辅助转换函数 ────────────────────────────────────────────────────────────

/// 将 config_json 字符串解析为结构化字段。
fn parse_config_json(config_json: &str) -> (Vec<String>, Vec<String>, Vec<String>, Option<f64>, Option<u64>) {
    #[derive(Deserialize)]
    struct Cfg {
        #[serde(default)]
        tools: Vec<String>,
        #[serde(default)]
        mcp_servers: Vec<String>,
        #[serde(default)]
        skills: Vec<String>,
        #[serde(default)]
        temperature: Option<f64>,
        #[serde(default)]
        max_tokens: Option<u64>,
    }

    if config_json.is_empty() || config_json == "{}" {
        return (Vec::new(), Vec::new(), Vec::new(), None, None);
    }

    let cfg: Cfg = serde_json::from_str(config_json).unwrap_or(Cfg {
        tools: Vec::new(),
        mcp_servers: Vec::new(),
        skills: Vec::new(),
        temperature: None,
        max_tokens: None,
    });

    (cfg.tools, cfg.mcp_servers, cfg.skills, cfg.temperature, cfg.max_tokens)
}

/// 将请求中的 config 字段序列化为 config_json 字符串。
fn make_config_json(
    tools: &[String],
    mcp_servers: &[String],
    skills: &[String],
    temperature: Option<f64>,
    max_tokens: Option<u64>,
) -> String {
    let cfg = serde_json::json!({
        "tools": tools,
        "mcp_servers": mcp_servers,
        "skills": skills,
        "temperature": temperature,
        "max_tokens": max_tokens,
    });
    cfg.to_string()
}

/// 将 DB 行转换为列表项响应。
fn row_to_list_item(row: &agents::AgentRow) -> AgentListItem {
    let (tools, _, _, _, _) = parse_config_json(&row.config_json);
    AgentListItem {
        id: row.id.clone(),
        name: row.name.clone(),
        description: row.description.clone(),
        model: row.model.clone(),
        provider: row.provider.clone(),
        icon: row.icon.clone(),
        color: row.color.clone(),
        status: row.status.clone(),
        tools,
        created_at: row.created_at.clone(),
    }
}

/// 将 DB 行转换为详情响应。
fn row_to_detail(row: &agents::AgentRow) -> AgentDetail {
    let (tools, mcp_servers, skills, temperature, max_tokens) =
        parse_config_json(&row.config_json);
    AgentDetail {
        id: row.id.clone(),
        name: row.name.clone(),
        description: row.description.clone(),
        system_prompt: row.system_prompt.clone(),
        model: row.model.clone(),
        provider: row.provider.clone(),
        icon: row.icon.clone(),
        color: row.color.clone(),
        status: row.status.clone(),
        tools,
        mcp_servers,
        skills,
        temperature,
        max_tokens,
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    }
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `GET /api/agents`
///
/// 返回当前用户的 Agent 列表。
pub async fn list(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<AgentListItem>>, ApiError> {
    let rows = agents::list_by_user(&state.db, &user_id).await?;
    let items: Vec<AgentListItem> = rows.iter().map(row_to_list_item).collect();
    Ok(Json(items))
}

/// `POST /api/agents`
///
/// 创建新的 Agent 配置。
///
/// # Body
///
/// ```json
/// {
///   "name": "代码审查员",
///   "description": "负责代码质量审查",
///   "system_prompt": "你是一位资深代码审查专家...",
///   "model": "deepseek-v4-flash",
///   "tools": ["shell", "fetch"],
///   "icon": "🔍"
/// }
/// ```
pub async fn create(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateAgentRequest>,
) -> Result<(StatusCode, Json<AgentDetail>), ApiError> {
    // ── 输入验证 ──────────────────────────────────────────────────────────
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("agent name is required".into()));
    }

    // 检查名称重复
    if let Some(_existing) = agents::find_by_name_and_user(&state.db, name, &user_id).await? {
        return Err(ApiError::Conflict(format!(
            "agent with name '{name}' already exists"
        )));
    }

    // ── 构造 config_json ──────────────────────────────────────────────────
    let config_json = make_config_json(
        &req.tools,
        &req.mcp_servers,
        &req.skills,
        req.temperature,
        req.max_tokens,
    );

    // ── 插入 DB ───────────────────────────────────────────────────────────
    let agent_id = Uuid::new_v4().to_string();

    let params = CreateAgentParams {
        id: agent_id.clone(),
        user_id: user_id.clone(),
        name: name.to_string(),
        description: req.description.trim().to_string(),
        system_prompt: req.system_prompt.trim().to_string(),
        model: req.model,
        provider: req.provider,
        icon: req.icon,
        color: req.color,
        config_json,
    };

    agents::insert(&state.db, &params).await?;

    // ── 写入 agent.md 文件（通过 Workspace）───────────────────────────────
    let ws = state.workspace_manager.get(&user_id)?;
    write_agent_md_to_workspace(&ws, &params)?;

    // ── 读取完整行返回 ────────────────────────────────────────────────────
    let row = agents::find_by_id(&state.db, &agent_id)
        .await?
        .ok_or_else(|| ApiError::Internal("agent created but not found".into()))?;

    tracing::info!(
        user_id = %user_id,
        agent_id = %agent_id,
        agent_name = %name,
        "Agent created"
    );

    Ok((StatusCode::CREATED, Json(row_to_detail(&row))))
}

/// `GET /api/agents/:id`
///
/// 返回 Agent 完整详情。
pub async fn get(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
) -> Result<Json<AgentDetail>, ApiError> {
    let row = agents::find_by_id_and_user(&state.db, &agent_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent '{agent_id}' not found")))?;

    Ok(Json(row_to_detail(&row)))
}

/// `PATCH /api/agents/:id`
///
/// 更新 Agent 的部分字段。更新成功后使缓存失效。
pub async fn update(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
    Json(req): Json<UpdateAgentRequest>,
) -> Result<Json<AgentDetail>, ApiError> {
    // ── 确认 Agent 存在且属于当前用户 ─────────────────────────────────────
    let existing = agents::find_by_id_and_user(&state.db, &agent_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent '{agent_id}' not found")))?;

    // ── 检查名称重复 ──────────────────────────────────────────────────────
    if let Some(ref new_name) = req.name {
        let trimmed = new_name.trim();
        if !trimmed.is_empty() && trimmed != existing.name {
            if let Some(_dup) =
                agents::find_by_name_and_user(&state.db, trimmed, &user_id).await?
            {
                return Err(ApiError::Conflict(format!(
                    "agent with name '{trimmed}' already exists"
                )));
            }
        }
    }

    // ── 处理 config_json 的合并：未提供的字段沿用旧值 ────────────────────
    let (old_tools, old_mcp, old_skills, old_temp, old_max_tokens) =
        parse_config_json(&existing.config_json);

    let config_json = make_config_json(
        req.tools.as_ref().unwrap_or(&old_tools),
        req.mcp_servers.as_ref().unwrap_or(&old_mcp),
        req.skills.as_ref().unwrap_or(&old_skills),
        req.temperature.or(old_temp),
        req.max_tokens.or(old_max_tokens),
    );

    // ── 执行更新 ──────────────────────────────────────────────────────────
    let params = UpdateAgentParams {
        name: req.name.map(|s| s.trim().to_string()),
        description: req.description.map(|s| s.trim().to_string()),
        system_prompt: req.system_prompt.map(|s| s.trim().to_string()),
        model: req.model,
        provider: req.provider,
        icon: req.icon,
        color: req.color,
        config_json: Some(config_json),
    };

    let updated = agents::update(&state.db, &agent_id, &params).await?;
    if !updated {
        return Err(ApiError::NotFound(format!("agent '{agent_id}' not found")));
    }

    // ── 使缓存失效 ────────────────────────────────────────────────────────
    state.workspace_manager.invalidate_agent(&user_id, &existing.name)?;

    // ── 重新读取返回 ──────────────────────────────────────────────────────
    let row = agents::find_by_id(&state.db, &agent_id)
        .await?
        .ok_or_else(|| ApiError::Internal("agent updated but not found".into()))?;

    // 更新 agent.md 文件
    let ws = state.workspace_manager.get(&user_id)?;
    let create_params = CreateAgentParams {
        id: row.id.clone(),
        user_id: row.user_id.clone(),
        name: row.name.clone(),
        description: row.description.clone(),
        system_prompt: row.system_prompt.clone(),
        model: row.model.clone(),
        provider: row.provider.clone(),
        icon: row.icon.clone(),
        color: row.color.clone(),
        config_json: row.config_json.clone(),
    };
    write_agent_md_to_workspace(&ws, &create_params)?;

    tracing::info!(
        user_id = %user_id,
        agent_id = %agent_id,
        "Agent updated"
    );

    Ok(Json(row_to_detail(&row)))
}

/// `DELETE /api/agents/:id`
///
/// 删除 Agent 及其配置文件和缓存。
pub async fn delete(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    // ── 确认 Agent 存在且属于当前用户 ─────────────────────────────────────
    let existing = agents::find_by_id_and_user(&state.db, &agent_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent '{agent_id}' not found")))?;

    // ── 删除 DB 记录 ──────────────────────────────────────────────────────
    agents::delete(&state.db, &agent_id).await?;

    // ── 使缓存失效 ────────────────────────────────────────────────────────
    state.workspace_manager.invalidate_agent(&user_id, &existing.name)?;

    // ── 删除 agent.md 文件（通过 Workspace）───────────────────────────────
    let ws = state.workspace_manager.get(&user_id)?;
    if let Err(e) = ws.delete_agent(&existing.name) {
        tracing::warn!(error = %e, agent = %existing.name, "Failed to delete agent files");
    }

    tracing::info!(
        user_id = %user_id,
        agent_id = %agent_id,
        agent_name = %existing.name,
        "Agent deleted"
    );

    Ok(Json(SuccessResponse { success: true }))
}

// ── agent.md 文件生成 ──────────────────────────────────────────────────────

/// 将 Agent 配置写入 Workspace 的 agent.md 文件。
fn write_agent_md_to_workspace(
    ws: &peco_core::workspace::Workspace,
    params: &CreateAgentParams,
) -> Result<(), ApiError> {
    let (tools, mcp_servers, skills, temperature, max_tokens) =
        parse_config_json(&params.config_json);

    // 构建 YAML frontmatter
    let mut yaml = String::from("---\n");
    yaml.push_str("agent:\n");
    yaml.push_str(&format!("  name: \"{}\"\n", params.name));
    yaml.push_str(&format!("  description: \"{}\"\n", params.description));
    yaml.push_str("llm:\n");
    yaml.push_str(&format!("  provider: \"{}\"\n", params.provider));
    yaml.push_str(&format!("  model: \"{}\"\n", params.model));
    if let Some(t) = temperature {
        yaml.push_str(&format!("  temperature: {}\n", t));
    }
    if let Some(m) = max_tokens {
        yaml.push_str(&format!("  max_tokens: {}\n", m));
    }

    if !tools.is_empty() {
        yaml.push_str("tools:\n");
        for t in &tools {
            yaml.push_str(&format!("  - {}\n", t));
        }
    }

    if !mcp_servers.is_empty() {
        yaml.push_str("mcp:\n");
        for m in &mcp_servers {
            yaml.push_str(&format!("  - {}\n", m));
        }
    }

    if !skills.is_empty() {
        yaml.push_str("skills:\n");
        for s in &skills {
            yaml.push_str(&format!("  - {}\n", s));
        }
    }

    yaml.push_str("---\n\n");
    yaml.push_str(&params.system_prompt);
    yaml.push('\n');

    ws.save_agent(&params.name, &yaml)
        .map_err(|e| ApiError::Internal(format!("failed to write agent.md: {e}")))?;

    Ok(())
}
