// ============================================================================
// Agent Handlers — CRUD 操作
// ============================================================================
//
// 架构：agent.md 文件 = 唯一真相源，DB 仅保存轻量索引。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use peco_core::agent::agent_config::{self, AgentProfile, AssembleAgentMdParams};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::db::agents::{self, AgentRow, CreateAgentParams, UpdateAgentParams};
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
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
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
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub max_turns: Option<usize>,
    #[serde(default)]
    pub knowledge_bases: Option<Vec<String>>,
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
    pub model: Option<Option<String>>, // Option<Option<T>>: None=不更新, Some(None)=清空
    pub provider: Option<Option<String>>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub tools: Option<Vec<String>>,
    pub mcp_servers: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub temperature: Option<Option<f64>>,
    pub max_tokens: Option<Option<u64>>,
    pub stream: Option<Option<bool>>,
    pub reasoning_effort: Option<Option<String>>,
    pub max_turns: Option<usize>,
    pub knowledge_bases: Option<Vec<String>>,
}

/// Agent 列表项响应（含 model/provider/tools 便于列表页展示，不含 system_prompt）。
#[derive(Debug, Serialize)]
pub struct AgentListItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub icon: String,
    pub color: String,
    pub status: String,
    pub tools: Vec<String>,
    pub knowledge_bases: Vec<String>,
    pub created_at: String,
}

/// Agent 详情响应（完整字段，来自 agent.md + DB 索引）。
#[derive(Debug, Serialize)]
pub struct AgentDetail {
    pub id: String,
    pub name: String,
    pub description: String,
    pub system_prompt: String,            // agent.md body
    pub model: Option<String>,            // agent.md llm.model（可能未指定）
    pub provider: Option<String>,         // agent.md llm.provider（可能未指定）
    pub icon: String,                     // DB only
    pub color: String,                    // DB only
    pub status: String,                   // DB only
    pub tools: Vec<String>,               // agent.md tools
    pub mcp_servers: Vec<String>,         // agent.md mcp
    pub skills: Vec<String>,              // agent.md skills
    pub knowledge_bases: Vec<String>,     // agent.md knowledge_bases
    pub temperature: Option<f64>,         // agent.md llm.temperature
    pub max_tokens: Option<u64>,          // agent.md llm.max_tokens
    pub stream: Option<bool>,             // agent.md llm.stream
    pub reasoning_effort: Option<String>, // agent.md llm.reasoning_effort
    pub max_turns: usize,                 // agent.md max_turns
    pub created_at: String,               // DB
    pub updated_at: String,               // DB
}

/// 简单成功响应。
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
}

// ── 辅助函数 ────────────────────────────────────────────────────────────────

/// 从 agent.md 解析结果 + DB 索引行 构建 AgentDetail。
fn agent_detail_from_profile(
    agent_id: &str,
    db_row: &agents::AgentRow,
    profile: &AgentProfile,
    body: &str,
) -> AgentDetail {
    let llm = profile.llm.as_ref();
    AgentDetail {
        id: agent_id.to_string(),
        name: db_row.name.clone(),
        description: profile.agent.description.clone(),
        system_prompt: body.to_string(),
        model: llm.and_then(|l| l.model.clone()),
        provider: llm.and_then(|l| l.provider.clone()),
        icon: db_row.icon.clone(),
        color: db_row.color.clone(),
        status: db_row.status.clone(),
        tools: profile.tools.clone(),
        mcp_servers: profile.mcp.clone(),
        skills: profile.skills.clone(),
        knowledge_bases: profile.knowledge_bases.clone(),
        temperature: llm.and_then(|l| l.temperature),
        max_tokens: llm.and_then(|l| l.max_tokens),
        stream: llm.and_then(|l| l.stream),
        reasoning_effort: llm.and_then(|l| l.reasoning_effort.clone()),
        max_turns: profile.max_turns,
        created_at: db_row.created_at.clone(),
        updated_at: db_row.updated_at.clone(),
    }
}

/// 从 CreateAgentRequest 构造 AssembleAgentMdParams。
fn assemble_params_from_request(req: &CreateAgentRequest) -> AssembleAgentMdParams {
    AssembleAgentMdParams {
        name: req.name.trim().to_string(),
        description: req.description.trim().to_string(),
        provider: req.provider.clone().unwrap_or_else(|| "deepseek".into()),
        model: req
            .model
            .clone()
            .unwrap_or_else(|| "deepseek-v4-flash".into()),
        temperature: req.temperature,
        max_tokens: req.max_tokens,
        stream: req.stream,
        reasoning_effort: req.reasoning_effort.clone(),
        tools: req.tools.clone(),
        mcp_servers: req.mcp_servers.clone(),
        skills: req.skills.clone(),
        knowledge_bases: req.knowledge_bases.clone().unwrap_or_default(),
        max_turns: req.max_turns.unwrap_or(20),
        system_prompt: req.system_prompt.trim().to_string(),
    }
}

/// 深度合并：请求中非 None 字段覆盖旧值，保留未指定的字段。
fn merge_agent_profile(
    old_profile: &AgentProfile,
    old_body: &str,
    req: &UpdateAgentRequest,
) -> AssembleAgentMdParams {
    let old_llm = old_profile.llm.as_ref();

    // 对于 Option<Option<T>> 类型：
    // - None → 不更新（使用旧值）
    // - Some(None) → 清空
    // - Some(Some(v)) → 更新为新值
    fn merge_opt<T: Clone>(req_val: &Option<Option<T>>, old_val: Option<T>) -> Option<T> {
        match req_val {
            None => old_val,
            Some(None) => None,
            Some(Some(v)) => Some(v.clone()),
        }
    }

    AssembleAgentMdParams {
        name: req
            .name
            .clone()
            .unwrap_or_else(|| old_profile.agent.name.clone()),
        description: req
            .description
            .clone()
            .unwrap_or_else(|| old_profile.agent.description.clone()),
        provider: merge_opt(&req.provider, old_llm.and_then(|l| l.provider.clone()))
            .unwrap_or_default(),
        model: merge_opt(&req.model, old_llm.and_then(|l| l.model.clone())).unwrap_or_default(),
        temperature: merge_opt(&req.temperature, old_llm.and_then(|l| l.temperature)),
        max_tokens: merge_opt(&req.max_tokens, old_llm.and_then(|l| l.max_tokens)),
        stream: merge_opt(&req.stream, old_llm.and_then(|l| l.stream)),
        reasoning_effort: merge_opt(
            &req.reasoning_effort,
            old_llm.and_then(|l| l.reasoning_effort.clone()),
        ),
        tools: req
            .tools
            .clone()
            .unwrap_or_else(|| old_profile.tools.clone()),
        mcp_servers: req
            .mcp_servers
            .clone()
            .unwrap_or_else(|| old_profile.mcp.clone()),
        skills: req
            .skills
            .clone()
            .unwrap_or_else(|| old_profile.skills.clone()),
        knowledge_bases: req
            .knowledge_bases
            .clone()
            .unwrap_or_else(|| old_profile.knowledge_bases.clone()),
        max_turns: req.max_turns.unwrap_or(old_profile.max_turns),
        system_prompt: req
            .system_prompt
            .clone()
            .unwrap_or_else(|| old_body.to_string()),
    }
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `GET /api/agents`
///
/// 返回当前用户的 Agent 列表（含 model/provider/tools 便于列表页展示，
/// 不含 system_prompt 等大字段）。
pub async fn list(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<AgentListItem>>, ApiError> {
    let ws = state.workspace_manager.get(&user_id)?;

    // ── 自动注册：磁盘上存在但 DB 中缺失的 Agent ─────────────────────
    // 确保手动创建或模板安装的 Agent 自动出现在列表中。
    {
        let db_names: std::collections::HashSet<String> =
            agents::list_index_by_user(&state.db, &user_id)
                .await?
                .into_iter()
                .map(|r| r.name)
                .collect();

        let disk_metas = ws.agent_manager().list_meta();
        for meta in &disk_metas {
            if !db_names.contains(&meta.name) {
                let params = CreateAgentParams {
                    id: Uuid::new_v4().to_string(),
                    user_id: user_id.clone(),
                    name: meta.name.clone(),
                    description: meta.description.clone(),
                    icon: "🤖".to_string(),
                    color: "#6366f1".to_string(),
                };
                if let Err(e) = agents::insert(&state.db, &params).await {
                    tracing::warn!(
                        agent = %meta.name,
                        error = %e,
                        "Failed to auto-register agent found on disk"
                    );
                } else {
                    tracing::info!(
                        agent = %meta.name,
                        "Auto-registered agent from disk"
                    );
                }
            }
        }
    }

    let rows = agents::list_index_by_user(&state.db, &user_id).await?;

    let mut items: Vec<AgentListItem> = Vec::with_capacity(rows.len());
    for r in &rows {
        // 尝试从 agent.md 读取 tools/model/provider/knowledge_bases（失败则使用空默认值）
        let (tools, model, provider, knowledge_bases) = read_agent_md_light(ws.agent_manager(), &r.name);
        items.push(AgentListItem {
            id: r.id.clone(),
            name: r.name.clone(),
            description: r.description.clone(),
            model,
            provider,
            icon: r.icon.clone(),
            color: r.color.clone(),
            status: r.status.clone(),
            tools,
            knowledge_bases,
            created_at: r.created_at.clone(),
        });
    }
    Ok(Json(items))
}

/// 从 agent.md 读取轻量字段（tools, model, provider, knowledge_bases）。
/// 读取失败时返回空默认值，不阻塞列表渲染。
fn read_agent_md_light(
    agent_manager: &peco_core::agent::AgentManager,
    name: &str,
) -> (Vec<String>, Option<String>, Option<String>, Vec<String>) {
    let path = agent_manager.md_path(name);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return (Vec::new(), None, None, Vec::new()),
    };
    let (profile, _body) = match peco_core::agent::agent_config::parse_agent_md(&content) {
        Ok(p) => p,
        Err(_) => return (Vec::new(), None, None, Vec::new()),
    };
    let model = profile.llm.as_ref().and_then(|l| l.model.clone());
    let provider = profile.llm.as_ref().and_then(|l| l.provider.clone());
    (profile.tools, model, provider, profile.knowledge_bases)
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
    if agents::find_id_by_name_and_user(&state.db, name, &user_id)
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict(format!(
            "agent with name '{name}' already exists"
        )));
    }

    // ── 组装 agent.md 内容（内存中，尚未写盘）───────────────────────────
    let assemble_params = assemble_params_from_request(&req);
    let content = agent_config::assemble_agent_md(&assemble_params);

    let ws = state.workspace_manager.get(&user_id)?;
    let agent_id = Uuid::new_v4().to_string();

    // ── 先写 DB 索引（利用 UNIQUE 约束原子性防止并发竞态）──────────────
    let db_params = CreateAgentParams {
        id: agent_id.clone(),
        user_id: user_id.clone(),
        name: name.to_string(),
        description: req.description.trim().to_string(),
        icon: req.icon.clone(),
        color: req.color.clone(),
    };
    agents::insert(&state.db, &db_params).await?;

    // ── 后写 agent.md 文件（真相源）─────────────────────────────────────
    if let Err(e) = ws.agent_manager().save(name, &content) {
        // 回滚：删除已写入的 DB 索引记录
        let _ = agents::delete(&state.db, &agent_id).await;
        return Err(ApiError::Internal(format!("failed to write agent.md: {e}")));
    }

    tracing::info!(
        user_id = %user_id,
        agent_id = %agent_id,
        agent_name = %name,
        "Agent created"
    );

    // ── 读取 DB 获取时间戳，直接从 assemble_params 构造响应 ─────────────
    let db_row = agents::find_index_by_id_and_user(&state.db, &agent_id, &user_id)
        .await?
        .unwrap_or_else(|| AgentRow {
            id: agent_id.clone(),
            user_id: user_id.clone(),
            name: name.to_string(),
            description: String::new(),
            icon: req.icon.clone(),
            color: req.color.clone(),
            status: "idle".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        });

    let detail = AgentDetail {
        id: agent_id,
        name: name.to_string(),
        description: assemble_params.description,
        system_prompt: assemble_params.system_prompt,
        model: if assemble_params.model.is_empty() {
            None
        } else {
            Some(assemble_params.model)
        },
        provider: if assemble_params.provider.is_empty() {
            None
        } else {
            Some(assemble_params.provider)
        },
        icon: req.icon,
        color: req.color,
        status: db_row.status,
        tools: assemble_params.tools,
        mcp_servers: assemble_params.mcp_servers,
        skills: assemble_params.skills,
        knowledge_bases: assemble_params.knowledge_bases,
        temperature: assemble_params.temperature,
        max_tokens: assemble_params.max_tokens,
        stream: assemble_params.stream,
        reasoning_effort: assemble_params.reasoning_effort,
        max_turns: assemble_params.max_turns,
        created_at: db_row.created_at,
        updated_at: db_row.updated_at,
    };

    Ok((StatusCode::CREATED, Json(detail)))
}

/// `GET /api/agents/:id`
///
/// 返回 Agent 完整详情（数据来自 agent.md 文件）。
pub async fn get(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
) -> Result<Json<AgentDetail>, ApiError> {
    // ── 从 DB 获取 name ──────────────────────────────────────────────────
    let db_row = agents::find_index_by_id_and_user(&state.db, &agent_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent '{agent_id}' not found")))?;

    // ── 从 agent.md 读取完整配置 ─────────────────────────────────────────
    let ws = state.workspace_manager.get(&user_id)?;
    let md_path = ws.agent_manager().md_path(&db_row.name);
    let content = std::fs::read_to_string(&md_path).map_err(|e| {
        ApiError::Internal(format!(
            "failed to read agent.md for '{}': {e}",
            db_row.name
        ))
    })?;

    let (profile, body) = agent_config::parse_agent_md(&content)
        .map_err(|e| ApiError::Internal(format!("agent.md parse error: {e}")))?;

    // ── 自动同步 description（缓解 DB 缓存过期）─────────────────────────
    if profile.agent.description != db_row.description {
        let _ = agents::update(
            &state.db,
            &agent_id,
            &UpdateAgentParams {
                name: None,
                description: Some(profile.agent.description.clone()),
                icon: None,
                color: None,
            },
        )
        .await;
    }

    Ok(Json(agent_detail_from_profile(
        &agent_id, &db_row, &profile, &body,
    )))
}

/// `PATCH /api/agents/:id`
///
/// 更新 Agent 的部分字段。核心流程：读 agent.md → 深度合并 → 写回 → 更新 DB 索引。
pub async fn update(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
    Json(req): Json<UpdateAgentRequest>,
) -> Result<Json<AgentDetail>, ApiError> {
    // ── 确认 Agent 存在且属于当前用户 ─────────────────────────────────────
    let existing = agents::find_index_by_id_and_user(&state.db, &agent_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent '{agent_id}' not found")))?;

    // ── 检查名称重复（如果请求中包含新名称）─────────────────────────────
    let new_name = req
        .name
        .as_deref()
        .map(|s| s.trim())
        .unwrap_or(&existing.name);
    if !new_name.is_empty()
        && new_name != existing.name
        && agents::find_id_by_name_and_user(&state.db, new_name, &user_id)
            .await?
            .is_some()
    {
        return Err(ApiError::Conflict(format!(
            "agent with name '{new_name}' already exists"
        )));
    }

    // ── 读取现有 agent.md ─────────────────────────────────────────────────
    let ws = state.workspace_manager.get(&user_id)?;
    let old_path = ws.agent_manager().md_path(&existing.name);
    let old_content = std::fs::read_to_string(&old_path).map_err(|e| {
        ApiError::Internal(format!(
            "failed to read agent.md for '{}': {e}",
            existing.name
        ))
    })?;
    let (old_profile, old_body) = agent_config::parse_agent_md(&old_content)
        .map_err(|e| ApiError::Internal(format!("agent.md parse error: {e}")))?;

    // ── 深度合并 ──────────────────────────────────────────────────────────
    let merged = merge_agent_profile(&old_profile, &old_body, &req);
    let new_content = agent_config::assemble_agent_md(&merged);

    // ── 写 agent.md（若 name 变更则写入新目录）───────────────────────────
    ws.agent_manager()
        .save(new_name, &new_content)
        .map_err(|e| ApiError::Internal(format!("failed to write agent.md: {e}")))?;

    // 若 name 变更，删除旧目录
    if new_name != existing.name {
        let _ = ws.agent_manager().delete(&existing.name);
    }

    // ── 更新 DB 索引 ──────────────────────────────────────────────────────
    let updated = agents::update(
        &state.db,
        &agent_id,
        &UpdateAgentParams {
            name: if new_name != existing.name {
                Some(new_name.to_string())
            } else {
                None
            },
            description: Some(merged.description.clone()),
            icon: req.icon.clone(),
            color: req.color.clone(),
        },
    )
    .await?;

    if !updated {
        return Err(ApiError::NotFound(format!("agent '{agent_id}' not found")));
    }

    tracing::info!(
        user_id = %user_id,
        agent_id = %agent_id,
        old_name = %existing.name,
        new_name = %new_name,
        "Agent updated"
    );

    // ── 查询 DB 获取时间戳，直接从 merged 构造响应（避免重解析 YAML）────
    let updated_row = agents::find_index_by_id_and_user(&state.db, &agent_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::Internal("agent updated but not found".into()))?;

    let detail = AgentDetail {
        id: agent_id,
        name: merged.name,
        description: merged.description,
        system_prompt: merged.system_prompt,
        model: if merged.model.is_empty() {
            None
        } else {
            Some(merged.model)
        },
        provider: if merged.provider.is_empty() {
            None
        } else {
            Some(merged.provider)
        },
        icon: updated_row.icon,
        color: updated_row.color,
        status: updated_row.status,
        tools: merged.tools,
        mcp_servers: merged.mcp_servers,
        skills: merged.skills,
        knowledge_bases: merged.knowledge_bases,
        temperature: merged.temperature,
        max_tokens: merged.max_tokens,
        stream: merged.stream,
        reasoning_effort: merged.reasoning_effort,
        max_turns: merged.max_turns,
        created_at: updated_row.created_at,
        updated_at: updated_row.updated_at,
    };

    Ok(Json(detail))
}

/// `DELETE /api/agents/:id`
///
/// 删除 Agent 及其配置文件和 DB 索引。
pub async fn delete(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    // ── 确认 Agent 存在且属于当前用户 ─────────────────────────────────────
    let existing = agents::find_index_by_id_and_user(&state.db, &agent_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent '{agent_id}' not found")))?;

    // ── 删除 DB 记录 ──────────────────────────────────────────────────────
    agents::delete(&state.db, &agent_id).await?;

    // ── 使缓存失效 ────────────────────────────────────────────────────────
    state
        .workspace_manager
        .invalidate_agent(&user_id, &existing.name)?;

    // ── 删除 agent 文件目录 ───────────────────────────────────────────────
    let ws = state.workspace_manager.get(&user_id)?;
    if let Err(e) = ws.agent_manager().delete(&existing.name) {
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
