// ============================================================================
// Chat Handlers — 对话管理 + SSE 流式对话
// ============================================================================

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{KeepAlive, Sse};
use futures::stream::Stream;
use peco_core::agent::{AgentLooper, LooperConfig, LooperEvent};
use peco_core::persistence::SessionPersister;
use peco_core::session::Session;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::db::{agents, conversations, messages};
use crate::error::ApiError;
use crate::personal_assistant::build_ppa_components;
use crate::session_store::SqliteSessionPersister;
use crate::state::AppState;

use super::sse::{ChatSseEvent, UsageData, map_looper_event};

// ── Request / Response 类型 ─────────────────────────────────────────────────

/// 创建对话请求。
#[derive(Debug, Deserialize)]
pub struct CreateConversationRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// 对话列表项响应。
#[derive(Debug, Serialize)]
pub struct ConversationResponse {
    pub id: String,
    pub title: String,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 消息列表项响应。
#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub id: String,
    pub role: String,
    pub content: String,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub created_at: String,
}

/// SSE 流式查询参数。
#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    pub message: String,
}

/// 消息列表查询参数。
#[derive(Debug, Deserialize)]
pub struct MessagesQuery {
    #[serde(default = "default_offset")]
    pub offset: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_offset() -> i64 {
    0
}
fn default_limit() -> i64 {
    50
}

/// 简单成功响应。
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
}

// ── 子 Agent 事件关联类型 ───────────────────────────────────────────────────────

/// 子 Agent 调用信息，在 ToolCallStart 阶段写入，ToolResult 阶段读取。
///
/// `call_id` 是前端配对 `AgentCallStart` ↔ `AgentCallEnd` 的唯一标识。
#[derive(Debug, Clone)]
struct SubAgentInfo {
    call_id: String,
    agent_id: String,
    agent_name: String,
}

/// 从子 Agent tool result 中提取单个子 Agent 的输出。
///
/// - `delegate_sub_agent`：result 就是子 Agent 完整输出，直接返回
/// - `run_parallel_sub_agents`：result 是 JSON 数组，按 agent_name 匹配提取
fn extract_sub_agent_result(tool_result: &str, info: &SubAgentInfo, tool_name: &str) -> String {
    if tool_name == "delegate_sub_agent" {
        // 单 Agent 委托：result 即子 Agent 输出
        return tool_result.to_string();
    }

    // run_parallel_sub_agents：尝试从 JSON 数组中匹配 agent_name
    if let Ok(results) = serde_json::from_str::<Vec<serde_json::Value>>(tool_result) {
        for item in &results {
            if item["agent_name"].as_str() == Some(&info.agent_name) {
                // 优先返回 output，其次返回完整 JSON
                if let Some(output) = item["output"].as_str() {
                    return output.to_string();
                }
                if let Some(error) = item["error"].as_str() {
                    return format!("[error] {error}");
                }
                return item.to_string();
            }
        }
    }

    // 降级：无法解析时返回截断的原始结果
    let preview: String = tool_result.chars().take(200).collect();
    if preview.len() < tool_result.len() {
        format!("{preview}...")
    } else {
        preview
    }
}

// ── 辅助函数 ─────────────────────────────────────────────────────────────────

/// 确保用户拥有全能助手（omni-assistant）Agent。
///
/// 若不存在则自动创建，返回 agent_id。
async fn ensure_omni_agent(state: &Arc<AppState>, user_id: &str) -> Result<String, ApiError> {
    // 查找名为 "全能助手" 的 agent
    let existing = agents::find_id_by_name_and_user(&state.db, "全能助手", user_id).await?;
    if let Some(agent_id) = existing {
        return Ok(agent_id);
    }

    // 创建默认全能助手
    let agent_id = Uuid::new_v4().to_string();
    let assemble_params = peco_core::agent::agent_config::AssembleAgentMdParams {
        name: "全能助手".to_string(),
        description: "默认全能 AI 助手，可处理各类问题并与专业 Agent 协作".to_string(),
        provider: "deepseek".to_string(),
        model: "deepseek-v4-flash".to_string(),
        temperature: None,
        max_tokens: None,
        stream: None,
        reasoning_effort: None,
        tools: vec!["shell_exec".to_string(), "fetch".to_string()],
        mcp_servers: vec![],
        skills: vec![],
        max_turns: 20,
        system_prompt:
            "你是一个智能 AI 助手（Omni-Assistant），能够回答各种问题、使用工具完成任务。\
            当遇到专业领域问题时，你可以调用子 Agent 来获取更专业的帮助。\
            请始终使用中文回复用户。"
                .to_string(),
    };
    let content = peco_core::agent::agent_config::assemble_agent_md(&assemble_params);

    let ws = state.workspace_manager.get(user_id)?;

    // ── 先写 DB 索引（UNIQUE 约束防止并发重复创建）───────────────────────
    let db_params = agents::CreateAgentParams {
        id: agent_id.clone(),
        user_id: user_id.to_string(),
        name: "全能助手".to_string(),
        description: assemble_params.description.clone(),
        icon: "🌟".to_string(),
        color: "#6366f1".to_string(),
    };
    agents::insert(&state.db, &db_params).await?;

    // ── 后写 agent.md 文件 ─────────────────────────────────────────────────
    if let Err(e) = ws.save_agent("全能助手", &content) {
        // 回滚：删除已写入的 DB 索引
        let _ = agents::delete(&state.db, &agent_id).await;
        return Err(ApiError::Internal(format!(
            "failed to write omni agent.md: {e}"
        )));
    }

    tracing::info!(
        user_id = %user_id,
        agent_id = %agent_id,
        "Created omni-assistant agent for user"
    );

    Ok(agent_id)
}

/// 将对话行转换为列表响应（含 agent_name 查询）。
async fn row_to_response(
    pool: &sqlx::SqlitePool,
    row: &conversations::ConversationRow,
) -> Result<ConversationResponse, ApiError> {
    let agent_name = match &row.agent_id {
        Some(aid) => agents::find_name_by_id(pool, aid).await?,
        None => None,
    };

    Ok(ConversationResponse {
        id: row.id.clone(),
        title: row.title.clone(),
        agent_id: row.agent_id.clone(),
        agent_name,
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    })
}

// ── Conversation Handlers ────────────────────────────────────────────────────

/// `GET /api/conversations`
///
/// 返回当前用户的对话列表。可选 `?agent_id=` 过滤。
pub async fn list_conversations(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ConversationListQuery>,
) -> Result<Json<Vec<ConversationResponse>>, ApiError> {
    let rows = conversations::list_by_user(&state.db, &user_id, params.agent_id.as_deref()).await?;

    let mut responses = Vec::with_capacity(rows.len());
    for row in &rows {
        responses.push(row_to_response(&state.db, row).await?);
    }

    Ok(Json(responses))
}

#[derive(Debug, Deserialize)]
pub struct ConversationListQuery {
    pub agent_id: Option<String>,
}

/// `POST /api/conversations`
///
/// 创建新对话。
pub async fn create_conversation(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateConversationRequest>,
) -> Result<(StatusCode, Json<ConversationResponse>), ApiError> {
    let title = req
        .title
        .unwrap_or_else(|| "新对话".to_string())
        .trim()
        .to_string();

    // 若指定 agent_id，验证归属；否则使用全能助手
    let agent_id = if let Some(ref aid) = req.agent_id {
        agents::find_index_by_id_and_user(&state.db, aid, &user_id)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("agent '{aid}' not found")))?;
        Some(aid.clone())
    } else {
        let omni_id = ensure_omni_agent(&state, &user_id).await?;
        Some(omni_id)
    };

    let conv_id = Uuid::new_v4().to_string();
    let params = conversations::CreateConversationParams {
        id: conv_id.clone(),
        user_id: user_id.clone(),
        agent_id: agent_id.clone(),
        title: title.clone(),
    };

    conversations::insert(&state.db, &params).await?;

    let row = conversations::find_by_id(&state.db, &conv_id)
        .await?
        .ok_or_else(|| ApiError::Internal("conversation created but not found".into()))?;

    let response = row_to_response(&state.db, &row).await?;

    tracing::info!(
        user_id = %user_id,
        conversation_id = %conv_id,
        agent_id = ?agent_id,
        "Conversation created"
    );

    Ok((StatusCode::CREATED, Json(response)))
}

/// `DELETE /api/conversations/:id`
///
/// 删除对话及关联消息和会话快照。
pub async fn delete_conversation(
    AuthUser { user_id: _ }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(conv_id): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    // 查找并验证对话存在
    let conv = conversations::find_by_id(&state.db, &conv_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    // 删除关联消息
    messages::delete_by_conversation(&state.db, &conv_id).await?;

    // 删除会话快照
    let persister = SqliteSessionPersister::new(state.db.clone());
    let _ = persister.delete(&conv_id).await;

    // 删除对话记录
    conversations::delete(&state.db, &conv_id).await?;

    tracing::info!(
        user_id = %conv.user_id,
        conversation_id = %conv_id,
        "Conversation deleted"
    );

    Ok(Json(SuccessResponse { success: true }))
}

/// `GET /api/conversations/:id/messages`
///
/// 获取对话消息列表，支持分页。
pub async fn get_messages(
    AuthUser { user_id: _ }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(conv_id): Path<String>,
    Query(params): Query<MessagesQuery>,
) -> Result<Json<Vec<MessageResponse>>, ApiError> {
    // 验证对话存在
    conversations::find_by_id(&state.db, &conv_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    let rows =
        messages::list_by_conversation(&state.db, &conv_id, params.offset, params.limit).await?;

    let responses: Vec<MessageResponse> = rows
        .iter()
        .map(|r| MessageResponse {
            id: r.id.clone(),
            role: r.role.clone(),
            content: r.content.clone(),
            agent_id: r.agent_id.clone(),
            agent_name: r.agent_name.clone(),
            created_at: r.created_at.clone(),
        })
        .collect();

    Ok(Json(responses))
}

// ── SSE Stream Handler ───────────────────────────────────────────────────────

/// `GET /api/conversations/:id/stream?message=xxx`
///
/// SSE 流式对话。核心流程：
///
/// 1. 加载/验证对话和 Agent
/// 2. 加载或创建 Session（从 session_snapshots 恢复）
/// 3. 创建 SqliteSessionPersister 用于持久化
/// 4. 创建 AgentLooper → spawn → send_query
/// 5. 接收 LooperEvent → 映射为 ChatSseEvent → 通过 SSE channel 发送给客户端
/// 6. Looper 完成后发送 done 事件
pub async fn stream_chat(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(conv_id): Path<String>,
    Query(params): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>>, ApiError> {
    let message = params.message.trim().to_string();
    if message.is_empty() {
        return Err(ApiError::BadRequest("message is required".into()));
    }

    // ── 1. 加载对话 ──────────────────────────────────────────────────────
    let conv = conversations::find_by_id(&state.db, &conv_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    // ── 2. 确定 Agent ────────────────────────────────────────────────────
    let agent_id = match &conv.agent_id {
        Some(aid) => aid.clone(),
        None => {
            return Err(ApiError::Internal(
                "conversation has no agent assigned".into(),
            ));
        }
    };

    // agent 必须属于当前用户
    let agent_row = agents::find_index_by_id_and_user(&state.db, &agent_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::Forbidden("you do not have access to this agent".into()))?;

    let agent = state
        .workspace_manager
        .get_agent(&user_id, &agent_row.name)?;

    // ── 3. 加载或创建 Session ────────────────────────────────────────────
    let persister = SqliteSessionPersister::new(state.db.clone());
    let session: Box<Session> = match persister.load(&conv_id).await {
        Ok(Some((snapshot, _meta))) => {
            tracing::info!(
                conversation_id = %conv_id,
                turns = snapshot.committed_turns.len(),
                "Session restored from snapshot"
            );
            // 使用 snapshot 中的消息时间或当前时间作为 created_at
            let created_at = snapshot
                .committed_turns
                .first()
                .and_then(|t| t.first())
                .map(|m| m.timestamp_ms / 1000)
                .unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                });
            Box::new(Session::from_snapshot(
                conv_id.clone(),
                conv.title.clone(),
                created_at,
                snapshot,
            ))
        }
        _ => {
            tracing::info!(conversation_id = %conv_id, "Creating new session");
            Box::new(Session::new(conv_id.clone(), conv.title.clone()))
        }
    };

    // ── 4. 创建 SSE channel ──────────────────────────────────────────────
    let (sse_tx, sse_rx) = mpsc::channel::<Result<axum::response::sse::Event, Infallible>>(256);

    // ── 4.5. 构建 PPA 组件 ───────────────────────────────────────────────
    let ppa_ctx = build_ppa_components(&state, &user_id).await;

    let db_for_bg = state.db.clone();
    let conv_id_for_bg = conv_id.clone();
    let _conv_title = conv.title.clone();
    let message_for_bg = message.clone();
    let user_id_for_bg = user_id.clone();

    // ── 5. 后台执行 AgentLooper ──────────────────────────────────────────
    tokio::spawn(async move {
        let persister: Arc<dyn SessionPersister> =
            Arc::new(SqliteSessionPersister::new(db_for_bg.clone()));

        let config = LooperConfig {
            event_buffer: 256,
            per_turn_timeout: Some(Duration::from_secs(300)),
            total_timeout: Some(Duration::from_secs(1800)),
            persist_on_failure: true,
            dynamic_context: ppa_ctx.dynamic_context,
            hooks: ppa_ctx.hooks,
            ..LooperConfig::default()
        };

        let handle = AgentLooper::spawn(agent, session, config, persister.clone());

        // 发送用户消息
        if let Err(e) = handle.send_query(message_for_bg.clone()).await {
            let err_event = ChatSseEvent::Error {
                message: format!("Failed to send message: {e}"),
                conversation_id: conv_id_for_bg.clone(),
            };
            if let Ok(ev) = err_event.to_sse_event() {
                let _ = sse_tx.send(Ok(ev)).await;
            }
            return;
        }

        // 更新对话时间戳
        let _ = conversations::touch(&db_for_bg, &conv_id_for_bg).await;

        // ── 写入用户消息到 messages 表 ────────────────────────────────
        let _ = messages::insert(
            &db_for_bg,
            &Uuid::new_v4().to_string(),
            &conv_id_for_bg,
            "user",
            &message_for_bg,
            None,
            None,
        )
        .await;

        // ── 6. 事件循环：接收 LooperEvent → SSE ──────────────────────────
        let mut assistant_text = String::new();
        // 子 Agent 调用信息表：key = tool_call_id，value = 被调用的子 Agent 列表。
        // 在 ToolCallStart 阶段写入，在 ToolResult 阶段读取并删除。
        // 前端通过 AgentCallStart.call_id == AgentCallEnd.call_id 直接配对。
        let mut sub_agent_registry: HashMap<String, Vec<SubAgentInfo>> = HashMap::new();

        loop {
            match handle.recv_event().await {
                Some(LooperEvent::TextDelta { delta }) => {
                    assistant_text.push_str(&delta);
                    if let Some(sse_ev) =
                        map_looper_event(LooperEvent::TextDelta { delta }, &conv_id_for_bg)
                        && let Ok(ev) = sse_ev.to_sse_event()
                        && sse_tx.send(Ok(ev)).await.is_err()
                    {
                        break; // 客户端断开
                    }
                }

                // ── 子 Agent 调用：delegate_sub_agent ─────────────────────
                Some(LooperEvent::ToolCallStart {
                    ref id,
                    ref name,
                    ref arguments,
                }) if name == "delegate_sub_agent" => {
                    // 解析参数获取 agent_name 和 task
                    if let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) {
                        let agent_name = args["agent_name"].as_str().unwrap_or("unknown");
                        let task = args["prompt"].as_str().unwrap_or("");
                        let agent_id = agents::find_id_by_name_and_user(
                            &db_for_bg,
                            agent_name,
                            &user_id_for_bg,
                        )
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "unknown".to_string());

                        // call_id 直接使用 LLM 生成的 tool_call_id，前端可据此配对
                        let call_id = id.clone();

                        let sse_ev = ChatSseEvent::AgentCallStart {
                            call_id: call_id.clone(),
                            agent_id: agent_id.clone(),
                            agent_name: agent_name.to_string(),
                            task: task.to_string(),
                            conversation_id: conv_id_for_bg.clone(),
                        };
                        if let Ok(ev) = sse_ev.to_sse_event()
                            && sse_tx.send(Ok(ev)).await.is_err()
                        {
                            break;
                        }
                        sub_agent_registry.insert(
                            id.clone(),
                            vec![SubAgentInfo {
                                call_id: call_id.clone(),
                                agent_id,
                                agent_name: agent_name.to_string(),
                            }],
                        );
                    }
                    // 继续发送标准 tool_call_start SSE
                    if let Some(sse_ev) = map_looper_event(
                        LooperEvent::ToolCallStart {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                        &conv_id_for_bg,
                    ) && let Ok(ev) = sse_ev.to_sse_event()
                        && sse_tx.send(Ok(ev)).await.is_err()
                    {
                        break;
                    }
                }

                // ── 子 Agent 调用：run_parallel_sub_agents ────────────────
                Some(LooperEvent::ToolCallStart {
                    ref id,
                    ref name,
                    ref arguments,
                }) if name == "run_parallel_sub_agents" => {
                    if let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments)
                        && let Some(tasks) = args["tasks"].as_str()
                        && let Ok(task_list) = serde_json::from_str::<Vec<serde_json::Value>>(tasks)
                    {
                        let mut infos = Vec::new();
                        for (index, task) in task_list.iter().enumerate() {
                            let agent_name = task["agent_name"].as_str().unwrap_or("unknown");
                            let prompt = task["prompt"].as_str().unwrap_or("");
                            let agent_id = agents::find_id_by_name_and_user(
                                &db_for_bg,
                                agent_name,
                                &user_id_for_bg,
                            )
                            .await
                            .ok()
                            .flatten()
                            .unwrap_or_else(|| "unknown".to_string());

                            // 并行任务的 call_id = tool_call_id + 序号后缀，
                            // 确保每个子任务有唯一可配对的标识
                            let call_id = format!("{id}:{index}");

                            let sse_ev = ChatSseEvent::AgentCallStart {
                                call_id: call_id.clone(),
                                agent_id: agent_id.clone(),
                                agent_name: agent_name.to_string(),
                                task: prompt.to_string(),
                                conversation_id: conv_id_for_bg.clone(),
                            };
                            if let Ok(ev) = sse_ev.to_sse_event()
                                && sse_tx.send(Ok(ev)).await.is_err()
                            {
                                break;
                            }
                            infos.push(SubAgentInfo {
                                call_id,
                                agent_id,
                                agent_name: agent_name.to_string(),
                            });
                        }
                        sub_agent_registry.insert(id.clone(), infos);
                    }
                    // 继续发送标准 tool_call_start SSE
                    if let Some(sse_ev) = map_looper_event(
                        LooperEvent::ToolCallStart {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                        &conv_id_for_bg,
                    ) && let Ok(ev) = sse_ev.to_sse_event()
                        && sse_tx.send(Ok(ev)).await.is_err()
                    {
                        break;
                    }
                }

                // ── 子 Agent 调用结束 ──────────────────────────────────────
                Some(LooperEvent::ToolResult {
                    ref id,
                    ref name,
                    ref result,
                }) if name == "delegate_sub_agent" || name == "run_parallel_sub_agents" => {
                    // 从注册表中取出子 Agent 信息（O(1)），发送 AgentCallEnd 事件
                    if let Some(infos) = sub_agent_registry.remove(id.as_str()) {
                        for info in infos {
                            // 尝试从 tool result 中提取该子 Agent 的输出
                            let sub_result = extract_sub_agent_result(result, &info, name);
                            let sse_ev = ChatSseEvent::AgentCallEnd {
                                call_id: info.call_id,
                                agent_id: info.agent_id,
                                agent_name: info.agent_name,
                                result: sub_result,
                                conversation_id: conv_id_for_bg.clone(),
                            };
                            if let Ok(ev) = sse_ev.to_sse_event()
                                && sse_tx.send(Ok(ev)).await.is_err()
                            {
                                break;
                            }
                        }
                    }
                    // 继续发送标准 tool_result SSE
                    if let Some(sse_ev) = map_looper_event(
                        LooperEvent::ToolResult {
                            id: id.clone(),
                            name: name.clone(),
                            result: String::new(),
                        },
                        &conv_id_for_bg,
                    ) && let Ok(ev) = sse_ev.to_sse_event()
                        && sse_tx.send(Ok(ev)).await.is_err()
                    {
                        break;
                    }
                }

                Some(event @ LooperEvent::TurnComplete { .. }) => {
                    // 写入 assistant 概要消息到 messages 表
                    if !assistant_text.is_empty() {
                        let preview: String = assistant_text.chars().take(500).collect();
                        let _ = messages::insert(
                            &db_for_bg,
                            &Uuid::new_v4().to_string(),
                            &conv_id_for_bg,
                            "assistant",
                            &preview,
                            None,
                            None,
                        )
                        .await;
                        // 更新对话标题（使用前50字）
                        let short: String = assistant_text.chars().take(50).collect();
                        let new_title = if short.len() >= assistant_text.len() {
                            short
                        } else {
                            format!("{short}...")
                        };
                        if new_title.len() > 3 {
                            let _ = conversations::update_title(
                                &db_for_bg,
                                &conv_id_for_bg,
                                &new_title,
                            )
                            .await;
                        }
                    }
                    assistant_text.clear();

                    // ★ 将 TurnComplete 事件也转发给前端，
                    // 让前端可以在流结束前拿到完整的文本内容和 usage 信息。
                    if let Some(sse_ev) = map_looper_event(event, &conv_id_for_bg)
                        && let Ok(ev) = sse_ev.to_sse_event()
                        && sse_tx.send(Ok(ev)).await.is_err()
                    {
                        break;
                    }
                }

                Some(LooperEvent::Shutdown { total_usage, .. }) => {
                    // 发送 done 事件
                    let done_ev = ChatSseEvent::Done {
                        usage: UsageData::from(total_usage),
                        conversation_id: conv_id_for_bg.clone(),
                    };
                    if let Ok(ev) = done_ev.to_sse_event() {
                        let _ = sse_tx.send(Ok(ev)).await;
                    }
                    break;
                }

                Some(event) => {
                    if let Some(sse_ev) = map_looper_event(event, &conv_id_for_bg)
                        && let Ok(ev) = sse_ev.to_sse_event()
                        && sse_tx.send(Ok(ev)).await.is_err()
                    {
                        break;
                    }
                }

                None => {
                    // Channel closed
                    break;
                }
            }
        }

        // 确保 looper 被 drop 前完成清理
        drop(handle);
    });

    // ── 7. 返回 SSE 响应 ─────────────────────────────────────────────────
    let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

// ── Session Snapshot Handler ────────────────────────────────────────────────

/// Session 快照响应类型（含完整消息历史，用于前端恢复对话 UI）。
#[derive(Debug, Serialize)]
pub struct SessionSnapshotResponse {
    pub conversation_id: String,
    pub turns: Vec<TurnData>,
    pub total_usage: UsageData,
}

/// 单轮对话数据。
#[derive(Debug, Serialize)]
pub struct TurnData {
    pub turn_index: usize,
    pub messages: Vec<MessageData>,
}

/// 单条消息（前端友好格式，含 tool_calls / reasoning_content）。
#[derive(Debug, Serialize)]
pub struct MessageData {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallData>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub timestamp_ms: u64,
}

/// 工具调用简化格式。
#[derive(Debug, Serialize)]
pub struct ToolCallData {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// `GET /api/conversations/:id/session`
///
/// 返回对话的完整 Session 快照（含 tool calls、reasoning_content），
/// 供前端在刷新页面后重建聊天 UI。与 `GET /messages`（仅返回纯文本概要）
/// 不同，此端点返回完整结构化数据。
pub async fn get_session_snapshot(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(conv_id): Path<String>,
) -> Result<Json<SessionSnapshotResponse>, ApiError> {
    // 验证对话存在且属于当前用户
    let _conv = conversations::find_by_id_and_user(&state.db, &conv_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    // 从 session_snapshots 加载
    let persister = SqliteSessionPersister::new(state.db.clone());
    let snapshot_opt = persister
        .load(&conv_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to load session: {e}")))?;

    let (turns, usage): (Vec<TurnData>, UsageData) = match snapshot_opt {
        Some((snap, _meta)) => {
            let turns: Vec<TurnData> = snap
                .committed_turns
                .iter()
                .enumerate()
                .map(|(i, msgs): (usize, &Vec<_>)| TurnData {
                    turn_index: i,
                    messages: msgs
                        .iter()
                        .map(|am| {
                            let msg = &*am.message;
                            MessageData {
                                role: msg.role_name().to_string(),
                                content: msg.content().map(|s: &str| s.to_string()),
                                tool_calls: msg.tool_calls().map(|tcs: &Vec<_>| {
                                    tcs.iter()
                                        .map(|tc| ToolCallData {
                                            id: tc.id.clone(),
                                            name: tc.function.name.clone(),
                                            arguments: tc.function.arguments.clone(),
                                        })
                                        .collect()
                                }),
                                reasoning_content: msg
                                    .reasoning_content()
                                    .map(|s: &str| s.to_string()),
                                tool_call_id: msg.tool_call_id().map(|s: &str| s.to_string()),
                                timestamp_ms: am.timestamp_ms,
                            }
                        })
                        .collect(),
                })
                .collect();

            let usage = UsageData {
                input_tokens: snap.total_usage.input_tokens,
                output_tokens: snap.total_usage.output_tokens,
            };
            (turns, usage)
        }
        None => (
            Vec::new(),
            UsageData {
                input_tokens: 0,
                output_tokens: 0,
            },
        ),
    };

    tracing::debug!(
        conversation_id = %conv_id,
        turn_count = turns.len(),
        "Session snapshot returned"
    );

    Ok(Json(SessionSnapshotResponse {
        conversation_id: conv_id,
        turns,
        total_usage: usage,
    }))
}

// ============================================================================
// PPA 组件构建
// ============================================================================
