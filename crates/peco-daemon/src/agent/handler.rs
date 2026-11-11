// ============================================================================
// Agent Handlers — CRUD 操作
// ============================================================================
//
// 架构：agent.md 文件 = 唯一真相源，DB 仅保存轻量索引。

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use peco_core::agent::agent_config::{self, AgentProfile, AssembleAgentMdParams};
use peco_core::workspace::{WorkSpace, WorkspaceError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::LOCAL_USER_ID;
use crate::api_error::ApiError;
use crate::auth::LocalUser;
use crate::db::agents::{self, AgentRow, CreateAgentParams, UpdateAgentParams};
use crate::space::{fanout, index};
use crate::state::AppState;
use tracing::{info, warn};

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
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub max_iterations: Option<usize>,
    #[serde(default)]
    pub knowledge_bases: Option<Vec<String>>,
}

/// 更新 Agent 的请求体（全部字段可选）。
#[derive(Debug, Deserialize)]
pub struct UpdateAgentRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<Option<String>>, // Option<Option<T>>: None=不更新, Some(None)=清空
    pub provider: Option<Option<String>>,
    pub tools: Option<Vec<String>>,
    pub mcp_servers: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub temperature: Option<Option<f64>>,
    pub max_tokens: Option<Option<u32>>,
    pub stream: Option<Option<bool>>,
    pub reasoning_effort: Option<Option<String>>,
    pub max_iterations: Option<usize>,
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
    pub status: String,
    pub tools: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub skills: Vec<String>,
    pub knowledge_bases: Vec<String>,
    /// 是否为当前 workspace 的总管 Agent（名字由 peco-core 内部约定，不外露）
    pub is_master: bool,
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
    pub status: String,                   // DB only
    pub tools: Vec<String>,               // agent.md tools
    pub mcp_servers: Vec<String>,         // agent.md mcp
    pub skills: Vec<String>,              // agent.md skills
    pub knowledge_bases: Vec<String>,     // agent.md knowledge_bases
    pub temperature: Option<f64>,         // agent.md llm.temperature
    pub max_tokens: Option<u32>,          // agent.md llm.max_tokens
    pub stream: Option<bool>,             // agent.md llm.stream
    pub reasoning_effort: Option<String>, // agent.md llm.reasoning_effort
    pub max_iterations: usize,            // agent.md max_iterations
    /// 用户消息图片输入能力（provider 类型 + api 分派，未实例化）
    pub supports_images: bool,
    pub created_at: String, // DB
    pub updated_at: String, // DB
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
    supports_images: bool,
) -> AgentDetail {
    let llm = profile.llm.as_ref();
    AgentDetail {
        id: agent_id.to_string(),
        name: db_row.name.clone(),
        description: profile.agent.description.clone(),
        system_prompt: body.to_string(),
        model: llm.and_then(|l| l.model.clone()),
        provider: llm.and_then(|l| l.provider.clone()),
        status: db_row.status.clone(),
        tools: profile.tools.clone(),
        mcp_servers: profile.mcp.clone(),
        skills: profile.skills.clone(),
        knowledge_bases: profile.knowledge_bases.clone(),
        temperature: llm.and_then(|l| l.temperature),
        max_tokens: llm.and_then(|l| l.max_tokens),
        stream: llm.and_then(|l| l.stream),
        reasoning_effort: llm.and_then(|l| l.reasoning_effort.clone()),
        max_iterations: profile.max_iterations,
        supports_images,
        created_at: db_row.created_at.clone(),
        updated_at: db_row.updated_at.clone(),
    }
}

/// 从 CreateAgentRequest 构造 AssembleAgentMdParams。
fn assemble_params_from_request(req: &CreateAgentRequest) -> AssembleAgentMdParams {
    AssembleAgentMdParams {
        name: req.name.trim().to_string(),
        description: req.description.trim().to_string(),
        // 全新 agent 无模板版本
        template_version: None,
        provider: req.provider.clone().unwrap_or_else(|| "deepseek".into()),
        model: req
            .model
            .clone()
            .unwrap_or_else(|| "deepseek-v4-flash".into()),
        temperature: req.temperature,
        max_tokens: req.max_tokens,
        stream: req.stream,
        reasoning_effort: req.reasoning_effort.clone(),
        web_search: None,
        tools: req.tools.clone(),
        mcp_servers: req.mcp_servers.clone(),
        skills: req.skills.clone(),
        knowledge_bases: req.knowledge_bases.clone().unwrap_or_default(),
        max_iterations: req
            .max_iterations
            .unwrap_or(agent_config::default_max_iterations()),
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
        // 请求体不暴露该字段，原样透传；丢失则下次重启被模板覆盖
        template_version: old_profile.template_version,
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
        // 请求体未暴露该字段：编辑时保留旧值，避免静默丢失
        web_search: old_llm.and_then(|l| l.web_search),
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
        max_iterations: req.max_iterations.unwrap_or(old_profile.max_iterations),
        system_prompt: req
            .system_prompt
            .clone()
            .unwrap_or_else(|| old_body.to_string()),
    }
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `GET /api/agents`
///
/// 列出请求空间里的 Agent（含 model/provider/tools 便于列表页展示，
/// 不含 system_prompt 等大字段）。
///
/// 行来自 DB 索引，`tools` / `model` 等细节现读 agent.md：索引里没有这些字段，
/// 而列表页需要它们。
pub async fn list(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<AgentListItem>>, ApiError> {
    let ws = state.spaces.workspace(&ws_id).await?;
    let team_id = state.spaces.team_id(&ws_id).await?;

    // 索引可能落后于磁盘（模板安装、手工拷贝、外部编辑器新建）：列表是能发现
    // 这些 agent 的唯一入口，先补行再查，否则它们永远不出现在列表里。
    index::ensure_agents_indexed(&state.pool, &ws, &team_id).await?;

    let rows = agents::list_index_by_team(&state.pool, &team_id).await?;

    let mut items: Vec<AgentListItem> = Vec::with_capacity(rows.len());
    for r in &rows {
        // 索引行只在「磁盘有、库里有」时才代表一个真实 agent。目录已消失的行
        // 直接不返回：详情页要读 agent.md，交出去只会换一个 500。
        if !ws.agent_manager().md_path(&r.name).is_file() {
            tracing::debug!(
                ws_id = %ws_id,
                agent = %r.name,
                "agent.md is gone; hiding the stale index row"
            );
            continue;
        }
        // 尝试从 agent.md 读取轻量字段（失败则回落到 DB 缓存与空默认值）。
        // description 优先用文件：卡片名称由它推导，外部编辑器 / save_agent
        // 改过描述后索引行可能还是旧的。
        let light = read_agent_md_light(ws.agent_manager(), &r.name);
        items.push(AgentListItem {
            id: r.id.clone(),
            name: r.name.clone(),
            description: light.description.unwrap_or_else(|| r.description.clone()),
            model: light.model,
            provider: light.provider,
            status: r.status.clone(),
            tools: light.tools,
            mcp_servers: light.mcp_servers,
            skills: light.skills,
            knowledge_bases: light.knowledge_bases,
            is_master: ws.is_master_agent(&r.name),
            created_at: r.created_at.clone(),
        });
    }
    info!(ws_id = %ws_id, count = items.len(), "Agents listed");
    Ok(Json(items))
}

/// 列表页要用的 agent.md 轻量字段；读取失败时全部为空，不阻塞列表渲染。
#[derive(Debug, Default)]
struct AgentMdLight {
    description: Option<String>,
    tools: Vec<String>,
    mcp_servers: Vec<String>,
    skills: Vec<String>,
    knowledge_bases: Vec<String>,
    model: Option<String>,
    provider: Option<String>,
}

fn read_agent_md_light(agent_manager: &peco_core::agent::AgentManager, name: &str) -> AgentMdLight {
    let path = agent_manager.md_path(name);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return AgentMdLight::default(),
    };
    let (profile, _body) = match peco_core::agent::agent_config::parse_agent_md(&content) {
        Ok(p) => p,
        Err(_) => return AgentMdLight::default(),
    };
    AgentMdLight {
        description: Some(profile.agent.description),
        tools: profile.tools,
        mcp_servers: profile.mcp,
        skills: profile.skills,
        knowledge_bases: profile.knowledge_bases,
        model: profile.llm.as_ref().and_then(|l| l.model.clone()),
        provider: profile.llm.as_ref().and_then(|l| l.provider.clone()),
    }
}

/// `master` 的裸写法：前端与脚本常这么指总管 Agent，语义与 core 的保留名相同。
///
/// core 把保留名常量留在 crate 内部，对外只经 [`WorkSpace::is_master_agent`]
/// 暴露；裸别名是 daemon 自己的入口策略，所以字面量留在这里。
const MASTER_ALIAS: &str = "master";

/// 新建与改名的共同闸门：总管名不可被占用。
///
/// 一旦被占，`get_master_agent` 会指到另一个 Agent，整个空间的派发目标就变了。
fn ensure_writable_name(ws: &WorkSpace, name: &str) -> Result<(), ApiError> {
    if ws.is_master_agent(name) || name == MASTER_ALIAS {
        return Err(ApiError::coded(
            StatusCode::BAD_REQUEST,
            "reserved_agent",
            format!("'{name}' is reserved: each space has exactly one master agent"),
        ));
    }
    Ok(())
}

/// 删除被 core 拒绝时的响应：目标是保留名属于调用方选错了对象，不是服务器故障。
fn deletable_error(name: &str, error: WorkspaceError) -> ApiError {
    match error {
        WorkspaceError::ReservedAgent(msg) => {
            ApiError::coded(StatusCode::BAD_REQUEST, "reserved_agent", msg)
        }
        other => ApiError::Internal(format!("cannot delete agent '{name}': {other}")),
    }
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
///   "tools": ["shell", "fetch"]
/// }
/// ```
pub async fn create(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Json(req): Json<CreateAgentRequest>,
) -> Result<(StatusCode, Json<AgentDetail>), ApiError> {
    // ── 输入验证 ──────────────────────────────────────────────────────────
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("agent name is required".into()));
    }

    // 同名冲突只在同一团队内成立：另一团队的同名 agent 是另一份资源。
    let team_id = state.spaces.team_id(&ws_id).await?;
    if agents::find_id_by_name_and_team(&state.pool, name, &team_id)
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

    let ws = state.spaces.workspace(&ws_id).await?;
    ensure_writable_name(&ws, name)?;
    let agent_id = Uuid::new_v4().to_string();

    // ── 先写 DB 索引（利用 UNIQUE 约束原子性防止并发竞态）──────────────
    let db_params = CreateAgentParams {
        id: agent_id.clone(),
        team_id: team_id.clone(),
        name: name.to_string(),
        description: req.description.trim().to_string(),
    };
    agents::insert(&state.pool, &db_params).await?;

    // ── 后写 agent.md 文件（真相源）─────────────────────────────────────
    if let Err(e) = ws.agent_manager().save(name, &content) {
        // 回滚：删除已写入的 DB 索引记录
        let _ = agents::delete(&state.pool, &agent_id, &team_id).await;
        return Err(ApiError::Internal(format!("failed to write agent.md: {e}")));
    }

    info!(
        ws_id = %ws_id,
        agent_id = %agent_id,
        agent_name = %name,
        "Agent created"
    );

    // ── 读取 DB 获取时间戳，直接从 assemble_params 构造响应 ─────────────
    let db_row = agents::find_index_by_id_and_team(&state.pool, &agent_id, &team_id)
        .await?
        .unwrap_or_else(|| AgentRow {
            id: agent_id.clone(),
            user_id: LOCAL_USER_ID.to_string(),
            team_id: team_id.clone(),
            name: name.to_string(),
            description: String::new(),
            status: "idle".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        });

    let supports_images = peco_core::agent::supports_image_input_for(
        &ws.config(),
        if assemble_params.provider.is_empty() {
            None
        } else {
            Some(assemble_params.provider.as_str())
        },
    );
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
        status: db_row.status,
        tools: assemble_params.tools,
        mcp_servers: assemble_params.mcp_servers,
        skills: assemble_params.skills,
        knowledge_bases: assemble_params.knowledge_bases,
        temperature: assemble_params.temperature,
        max_tokens: assemble_params.max_tokens,
        stream: assemble_params.stream,
        reasoning_effort: assemble_params.reasoning_effort,
        max_iterations: assemble_params.max_iterations,
        supports_images,
        created_at: db_row.created_at,
        updated_at: db_row.updated_at,
    };

    // 同 team 的其它已打开空间有自己的 AgentManager：新名字只进了发起方的
    // Tier-1，不扇出它们看不到这个 agent。
    fanout::refresh_space_team(&state, &ws_id).await;

    Ok((StatusCode::CREATED, Json(detail)))
}

/// `GET /api/agents/:id`
///
/// 返回 Agent 完整详情（数据来自 agent.md 文件）。
pub async fn get(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Result<Json<AgentDetail>, ApiError> {
    // ── 从 DB 获取 name（限定本团队：别的团队的 id 在这里就是不存在）─────
    let team_id = state.spaces.team_id(&ws_id).await?;
    let db_row = agents::find_index_by_id_and_team(&state.pool, &agent_id, &team_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent '{agent_id}' not found")))?;

    // ── 从 agent.md 读取完整配置 ─────────────────────────────────────────
    let ws = state.spaces.workspace(&ws_id).await?;
    let md_path = ws.agent_manager().md_path(&db_row.name);
    let content = std::fs::read_to_string(&md_path).map_err(|e| {
        ApiError::Internal(format!(
            "failed to read agent.md for '{}': {e}",
            db_row.name
        ))
    })?;

    let (profile, body) = agent_config::parse_agent_md(&content)
        .map_err(|e| ApiError::Internal(format!("agent.md parse error: {e}")))?;

    let supports_images = peco_core::agent::supports_image_input_for(
        &ws.config(),
        profile.llm.as_ref().and_then(|l| l.provider.as_deref()),
    );
    let detail = agent_detail_from_profile(&agent_id, &db_row, &profile, &body, supports_images);
    info!(ws_id = %ws_id, agent_id = %agent_id, name = %db_row.name, "Agent fetched");
    Ok(Json(detail))
}

/// `PATCH /api/agents/:id`
///
/// 更新 Agent 的部分字段。核心流程：读 agent.md → 深度合并 → 写回 → 更新 DB 索引。
pub async fn update(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Json(req): Json<UpdateAgentRequest>,
) -> Result<Json<AgentDetail>, ApiError> {
    // ── 确认 Agent 存在（限定本团队：跨团队的 id 不是可改对象）─────────────
    let team_id = state.spaces.team_id(&ws_id).await?;
    let existing = agents::find_index_by_id_and_team(&state.pool, &agent_id, &team_id)
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
        && agents::find_id_by_name_and_team(&state.pool, new_name, &team_id)
            .await?
            .is_some()
    {
        return Err(ApiError::Conflict(format!(
            "agent with name '{new_name}' already exists"
        )));
    }

    // ── 读取现有 agent.md ─────────────────────────────────────────────────
    let ws = state.spaces.workspace(&ws_id).await?;
    if new_name != existing.name {
        // 改名 = 写新目录 + 删旧目录；旧名不可删时改名同样不成立，
        // 否则会留下「新名有文件、旧名还挂着总管」的两份身份。
        if let Err(e) = ws.agent_manager().ensure_deletable(&existing.name) {
            return Err(deletable_error(&existing.name, e));
        }
        ensure_writable_name(&ws, new_name)?;
    }
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
        &state.pool,
        &agent_id,
        &team_id,
        &UpdateAgentParams {
            name: if new_name != existing.name {
                Some(new_name.to_string())
            } else {
                None
            },
            description: Some(merged.description.clone()),
        },
    )
    .await?;

    if !updated {
        return Err(ApiError::NotFound(format!("agent '{agent_id}' not found")));
    }

    info!(
        ws_id = %ws_id,
        agent_id = %agent_id,
        old_name = %existing.name,
        new_name = %new_name,
        "Agent updated"
    );

    // ── 查询 DB 获取时间戳，直接从 merged 构造响应（避免重解析 YAML）────
    let updated_row = agents::find_index_by_id_and_team(&state.pool, &agent_id, &team_id)
        .await?
        .ok_or_else(|| ApiError::Internal("agent updated but not found".into()))?;

    let supports_images = peco_core::agent::supports_image_input_for(
        &ws.config(),
        if merged.provider.is_empty() {
            None
        } else {
            Some(merged.provider.as_str())
        },
    );
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
        status: updated_row.status,
        tools: merged.tools,
        mcp_servers: merged.mcp_servers,
        skills: merged.skills,
        knowledge_bases: merged.knowledge_bases,
        temperature: merged.temperature,
        max_tokens: merged.max_tokens,
        stream: merged.stream,
        reasoning_effort: merged.reasoning_effort,
        max_iterations: merged.max_iterations,
        supports_images,
        created_at: updated_row.created_at,
        updated_at: updated_row.updated_at,
    };

    // 改名会同时动两个名字（新名、旧名），扇出按「磁盘名 ∪ 库内名」整体失效，
    // 不需要在这里传名字。
    fanout::refresh_space_team(&state, &ws_id).await;

    Ok(Json(detail))
}

/// `DELETE /api/agents/:id`
///
/// 删除 Agent 及其配置文件和 DB 索引。
pub async fn delete(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    // ── 确认 Agent 存在（限定本团队：跨团队的 id 不是可删对象）─────────────
    let team_id = state.spaces.team_id(&ws_id).await?;
    let existing = agents::find_index_by_id_and_team(&state.pool, &agent_id, &team_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent '{agent_id}' not found")))?;

    // ── 删除前置：先问 core 这个名字能不能删 ──────────────────────────────
    // 顺序不能反：先删 DB 行再被 core 拒绝，会留下「行没了、文件还在」的半完成态。
    let ws = state.spaces.workspace(&ws_id).await?;
    if let Err(e) = ws.agent_manager().ensure_deletable(&existing.name) {
        return Err(deletable_error(&existing.name, e));
    }

    // ── 删除 DB 记录 ──────────────────────────────────────────────────────
    agents::delete(&state.pool, &agent_id, &team_id).await?;

    // ── 使缓存失效 ────────────────────────────────────────────────────────
    ws.agent_manager().invalidate(&existing.name);

    // ── 删除 agent 文件目录 ───────────────────────────────────────────────
    if let Err(e) = ws.agent_manager().delete(&existing.name) {
        warn!(error = %e, agent = %existing.name, "Failed to delete agent files");
    }

    // 扇出在文件已删除之后：失效集合里包含「rescan 之前」的 Tier-1 名字，
    // 所以刚被删掉的这个 agent 也能从其它空间的缓存里清掉。
    fanout::refresh_space_team(&state, &ws_id).await;

    info!(
        ws_id = %ws_id,
        agent_id = %agent_id,
        agent_name = %existing.name,
        "Agent deleted"
    );

    Ok(Json(SuccessResponse { success: true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个「由模板衍生」的 agent.md：带 template_version。
    const TEMPLATE_DERIVED_MD: &str = r#"---
agent:
  name: "sample-agent"
  description: "Peco"
template_version: 7
llm:
  provider: "deepseek"
  model: "deepseek-v4-flash"
tools:
  - "shell"
knowledge_bases: ["@private_memory"]
---

body
"#;
    fn empty_update_request() -> UpdateAgentRequest {
        UpdateAgentRequest {
            name: None,
            description: None,
            system_prompt: None,
            model: None,
            provider: None,
            tools: None,
            mcp_servers: None,
            skills: None,
            temperature: None,
            max_tokens: None,
            stream: None,
            reasoning_effort: None,
            max_iterations: None,
            knowledge_bases: None,
        }
    }

    /// 回归（本缺陷根因）：UI 保存（PATCH）必须保留 `template_version`，否则下次重启被模板覆盖。
    #[test]
    fn update_preserves_template_version() {
        let (old_profile, old_body) = agent_config::parse_agent_md(TEMPLATE_DERIVED_MD).unwrap();
        assert_eq!(old_profile.template_version, Some(7));

        let mut req = empty_update_request();
        req.knowledge_bases = Some(vec![
            "@private_memory".into(),
            "work_records".into(),
            "daily_jokes".into(),
        ]);

        let merged = merge_agent_profile(&old_profile, &old_body, &req);
        assert_eq!(merged.template_version, Some(7));

        let md = agent_config::assemble_agent_md(&merged);
        let (reparsed, _) = agent_config::parse_agent_md(&md).unwrap();
        assert_eq!(
            reparsed.template_version,
            Some(7),
            "round-trip 后 template_version 必须保持，否则重启会被模板覆盖"
        );
        assert_eq!(
            reparsed.knowledge_bases,
            vec!["@private_memory", "work_records", "daily_jokes"]
        );
    }

    /// 无模板版本的文件（用户手写）保存后不应凭空获得版本号。
    #[test]
    fn update_of_handwritten_agent_keeps_no_version() {
        let md = "---\nagent:\n  name: \"handwritten\"\n  description: \"d\"\nllm:\n  provider: \"deepseek\"\n---\n\nbody\n";
        let (old_profile, old_body) = agent_config::parse_agent_md(md).unwrap();
        assert_eq!(old_profile.template_version, None);

        let mut req = empty_update_request();
        req.description = Some("new desc".into());

        let merged = merge_agent_profile(&old_profile, &old_body, &req);
        assert_eq!(merged.template_version, None);

        let out = agent_config::assemble_agent_md(&merged);
        assert!(!out.contains("template_version"), "手写文件不应获得版本号");
    }
}
