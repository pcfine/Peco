// ============================================================================
// Chat Handlers — Agent 作用域下的对话管理 + SSE 流式对话
// ============================================================================
//
// 端点路径：/api/chat/:agentId/conversations

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
use crate::session_store::SqliteSessionPersister;
use crate::state::AppState;

use super::conversation::auto_archive_oldest_if_needed;
use super::sse::{
    ChatSseEvent, SubAgentInfo, UsageData, extract_sub_agent_result, map_looper_event,
    parse_sub_agent_infos,
};

// ── Request / Response 类型 ─────────────────────────────────────────────────

/// 创建对话请求。
#[derive(Debug, Deserialize)]
pub struct CreateConversationRequest {
    #[serde(default)]
    pub title: Option<String>,
}

/// 更新对话请求。
#[derive(Debug, Deserialize)]
pub struct UpdateConversationRequest {
    pub title: Option<String>,
    /// 设为 true 以归档对话
    pub archive: Option<bool>,
    /// 设为 true 以恢复已归档对话
    pub unarchive: Option<bool>,
}

/// 对话列表项响应。
#[derive(Debug, Serialize)]
pub struct ConversationResponse {
    pub id: String,
    pub title: String,
    pub agent_name: String,
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 对话列表查询参数。
#[derive(Debug, Deserialize)]
pub struct ConversationListQuery {
    /// 归档状态过滤："active"（默认）, "archived", "all"
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "active".to_string()
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

// ── 辅助函数 ─────────────────────────────────────────────────────────────────

pub(crate) fn row_to_response(row: &conversations::ConversationRow) -> ConversationResponse {
    ConversationResponse {
        id: row.id.clone(),
        title: row.title.clone(),
        agent_name: row.agent_name.clone(),
        archived: row.archived_at.is_some(),
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    }
}

// ── Conversation Handlers ────────────────────────────────────────────────────

/// `GET /api/chat/:agentId/conversations`
///
/// 返回某 Agent 下当前用户的对话列表。支持 `?status=active|archived|all`。
pub async fn list_conversations(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(agent_name): Path<String>,
    Query(params): Query<ConversationListQuery>,
) -> Result<Json<Vec<ConversationResponse>>, ApiError> {
    let include_archived = matches!(params.status.as_str(), "all" | "archived");

    let rows =
        conversations::list_by_user_and_agent(&state.db, &user_id, &agent_name, include_archived)
            .await?;

    let mut responses: Vec<ConversationResponse> = rows.iter().map(row_to_response).collect();

    // 如果只查询 archived，过滤掉活跃的
    if params.status == "archived" {
        responses.retain(|r| r.archived);
    } else if params.status == "active" {
        responses.retain(|r| !r.archived);
    }

    Ok(Json(responses))
}

/// `POST /api/chat/:agentId/conversations`
///
/// 创建新对话。自动检查上限，超限时自动归档最旧对话。
pub async fn create_conversation(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(agent_name): Path<String>,
    Json(req): Json<CreateConversationRequest>,
) -> Result<(StatusCode, Json<ConversationResponse>), ApiError> {
    let title = req
        .title
        .unwrap_or_else(|| "新对话".to_string())
        .trim()
        .to_string();

    // ── 上限检查：超限自动归档最旧对话 ────────────────────────────────
    let archived = auto_archive_oldest_if_needed(&state.db, &user_id, &agent_name).await?;
    if archived > 0 {
        tracing::info!(
            user_id = %user_id,
            agent_name = %agent_name,
            archived,
            "Auto-archived old conversations before creating new one"
        );
    }

    // ── 创建对话 ────────────────────────────────────────────────────────
    let conv_id = Uuid::new_v4().to_string();
    let params = conversations::CreateConversationParams {
        id: conv_id.clone(),
        user_id: user_id.clone(),
        agent_id: None,
        agent_name: agent_name.clone(),
        title: title.clone(),
    };

    conversations::insert(&state.db, &params).await?;

    let row = conversations::find_by_id(&state.db, &conv_id)
        .await?
        .ok_or_else(|| ApiError::Internal("conversation created but not found".into()))?;

    let response = row_to_response(&row);

    tracing::info!(
        user_id = %user_id,
        conversation_id = %conv_id,
        agent_name = %agent_name,
        "Conversation created"
    );

    Ok((StatusCode::CREATED, Json(response)))
}

/// `PATCH /api/chat/:agentId/conversations/:id`
///
/// 更新对话：重命名、归档、恢复。
pub async fn update_conversation(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
    Json(req): Json<UpdateConversationRequest>,
) -> Result<Json<ConversationResponse>, ApiError> {
    let _conv = conversations::find_by_id_and_user(&state.db, &conv_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    if let Some(ref title) = req.title {
        conversations::update_title(&state.db, &conv_id, title.trim()).await?;
    }

    if req.archive.unwrap_or(false) {
        conversations::archive(&state.db, &conv_id).await?;
    }

    if req.unarchive.unwrap_or(false) {
        conversations::unarchive(&state.db, &conv_id).await?;
    }

    let updated = conversations::find_by_id(&state.db, &conv_id)
        .await?
        .ok_or_else(|| ApiError::Internal("conversation updated but not found".into()))?;

    Ok(Json(row_to_response(&updated)))
}

/// `DELETE /api/chat/:agentId/conversations/:id`
///
/// 永久删除对话及关联消息和会话快照。
pub async fn delete_conversation(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let conv = conversations::find_by_id_and_user(&state.db, &conv_id, &user_id)
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
        "Conversation permanently deleted"
    );

    Ok(Json(SuccessResponse { success: true }))
}

/// `GET /api/chat/:agentId/conversations/:id/messages`
///
/// 获取对话消息列表，支持分页。
pub async fn get_messages(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
    Query(params): Query<MessagesQuery>,
) -> Result<Json<Vec<MessageResponse>>, ApiError> {
    conversations::find_by_id_and_user(&state.db, &conv_id, &user_id)
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

/// `GET /api/chat/:agentId/conversations/:id/stream?message=`
///
/// SSE 流式对话。
pub async fn stream_chat(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
    Query(params): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>>, ApiError> {
    let message = params.message.trim().to_string();
    if message.is_empty() {
        return Err(ApiError::BadRequest("message is required".into()));
    }

    // ── 1. 加载对话 ──────────────────────────────────────────────────────
    let conv = conversations::find_by_id_and_user(&state.db, &conv_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    // ── 2. 加载 Agent（从 agent_name 字段，非 agent_id）─────────────────
    let agent = state
        .workspace_manager
        .get_agent(&user_id, &conv.agent_name)?;

    // ── 3. 加载或创建 Session ────────────────────────────────────────────
    let persister = SqliteSessionPersister::new(state.db.clone());
    let session: Box<Session> = match persister.load(&conv_id).await {
        Ok(Some((snapshot, _meta))) => {
            tracing::info!(
                conversation_id = %conv_id,
                turns = snapshot.committed_turns.len(),
                "Session restored from snapshot"
            );
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

    let db_for_bg = state.db.clone();
    let conv_id_for_bg = conv_id.clone();
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
            ..LooperConfig::default()
        };

        let handle = AgentLooper::spawn(agent, session, config, persister.clone());

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

        let _ = conversations::touch(&db_for_bg, &conv_id_for_bg).await;

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

        let mut assistant_text = String::new();
        let mut sub_agent_registry: HashMap<String, Vec<SubAgentInfo>> = HashMap::new();

        let resolve_agent_id = |agent_name: &str| -> String {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(agents::find_id_by_name_and_user(
                    &db_for_bg,
                    agent_name,
                    &user_id_for_bg,
                ))
            })
            .ok()
            .flatten()
            .unwrap_or_else(|| "unknown".to_string())
        };

        loop {
            match handle.recv_event().await {
                Some(LooperEvent::TextDelta { delta }) => {
                    assistant_text.push_str(&delta);
                    if let Some(sse_ev) =
                        map_looper_event(LooperEvent::TextDelta { delta }, &conv_id_for_bg)
                        && let Ok(ev) = sse_ev.to_sse_event()
                        && sse_tx.send(Ok(ev)).await.is_err()
                    {
                        break;
                    }
                }

                // ── 子 Agent 调用 ────────────────────────────────────────
                Some(LooperEvent::ToolCallStart {
                    ref id,
                    ref name,
                    ref arguments,
                }) if name == "delegate_sub_agent" || name == "run_parallel_sub_agents" => {
                    let infos = parse_sub_agent_infos(id, name, arguments, resolve_agent_id);

                    for info in &infos {
                        let task = if name == "delegate_sub_agent" {
                            serde_json::from_str::<serde_json::Value>(arguments)
                                .ok()
                                .and_then(|v| v["prompt"].as_str().map(|s| s.to_string()))
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };

                        let sse_ev = ChatSseEvent::AgentCallStart {
                            call_id: info.call_id.clone(),
                            agent_id: info.agent_id.clone(),
                            agent_name: info.agent_name.clone(),
                            task,
                            conversation_id: conv_id_for_bg.clone(),
                        };
                        if let Ok(ev) = sse_ev.to_sse_event()
                            && sse_tx.send(Ok(ev)).await.is_err()
                        {
                            break;
                        }
                    }
                    sub_agent_registry.insert(id.clone(), infos);

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

                // ── 子 Agent 调用结束 ────────────────────────────────────
                Some(LooperEvent::ToolResult {
                    ref id,
                    ref name,
                    ref result,
                }) if name == "delegate_sub_agent" || name == "run_parallel_sub_agents" => {
                    if let Some(infos) = sub_agent_registry.remove(id.as_str()) {
                        for info in infos {
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

                    if let Some(sse_ev) = map_looper_event(event, &conv_id_for_bg)
                        && let Ok(ev) = sse_ev.to_sse_event()
                        && sse_tx.send(Ok(ev)).await.is_err()
                    {
                        break;
                    }
                }

                Some(LooperEvent::Shutdown { total_usage, .. }) => {
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

                None => break,
            }
        }

        drop(handle);
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

// ── Session Snapshot Handler ────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SessionSnapshotResponse {
    pub conversation_id: String,
    pub turns: Vec<TurnData>,
    pub total_usage: UsageData,
}

#[derive(Debug, Serialize)]
pub struct TurnData {
    pub turn_index: usize,
    pub messages: Vec<MessageData>,
}

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

#[derive(Debug, Serialize)]
pub struct ToolCallData {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// `GET /api/chat/:agentId/conversations/:id/session`
pub async fn get_session_snapshot(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
) -> Result<Json<SessionSnapshotResponse>, ApiError> {
    let _conv = conversations::find_by_id_and_user(&state.db, &conv_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    let persister = SqliteSessionPersister::new(state.db.clone());
    let snapshot_opt = persister
        .load(&conv_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to load session: {e}")))?;

    let (turns, usage) = match snapshot_opt {
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

    Ok(Json(SessionSnapshotResponse {
        conversation_id: conv_id,
        turns,
        total_usage: usage,
    }))
}

// ── Export Handler ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default = "default_export_format")]
    pub format: String,
}

fn default_export_format() -> String {
    "json".to_string()
}

/// `GET /api/chat/:agentId/conversations/:id/export?format=json|markdown`
pub async fn export_conversation(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
    Query(params): Query<ExportQuery>,
) -> Result<axum::response::Response, ApiError> {
    let _conv = conversations::find_by_id_and_user(&state.db, &conv_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    let persister = SqliteSessionPersister::new(state.db.clone());
    let snapshot_opt = persister
        .load(&conv_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to load session: {e}")))?;

    match params.format.as_str() {
        "markdown" => {
            let md = snapshot_to_markdown(&snapshot_opt, &conv_id);
            Ok(axum::response::Response::builder()
                .header("Content-Type", "text/markdown; charset=utf-8")
                .header(
                    "Content-Disposition",
                    format!("attachment; filename=\"conversation-{conv_id}.md\""),
                )
                .body(axum::body::Body::from(md))
                .unwrap())
        }
        _ => {
            let json = serde_json::to_string_pretty(&snapshot_opt).unwrap_or_default();
            Ok(axum::response::Response::builder()
                .header("Content-Type", "application/json; charset=utf-8")
                .header(
                    "Content-Disposition",
                    format!("attachment; filename=\"conversation-{conv_id}.json\""),
                )
                .body(axum::body::Body::from(json))
                .unwrap())
        }
    }
}

pub(crate) fn snapshot_to_markdown(
    snapshot_opt: &Option<(peco_core::session::SessionSnapshot, peco_core::session::SessionMeta)>,
    conv_id: &str,
) -> String {
    let mut md = format!("# 对话记录 — {conv_id}\n\n");
    if let Some((snap, _meta)) = snapshot_opt {
        for turn in &snap.committed_turns {
            for am in turn {
                let msg = &*am.message;
                match msg.role_name() {
                    "user" => {
                        if let Some(content) = msg.content() {
                            md.push_str(&format!("\n## 用户\n{content}\n"));
                        }
                    }
                    "assistant" => {
                        if let Some(reasoning) = msg.reasoning_content() {
                            md.push_str(&format!(
                                "\n> 推理：{reasoning}\n"
                            ));
                        }
                        if let Some(tool_calls) = msg.tool_calls() {
                            for tc in tool_calls {
                                md.push_str(&format!(
                                    "\n> 🔧 {}: `{}`\n",
                                    tc.function.name,
                                    tc.function.arguments.chars().take(200).collect::<String>()
                                ));
                            }
                        }
                        if let Some(content) = msg.content() {
                            md.push_str(&format!("\n## 助手\n{content}\n"));
                        }
                    }
                    "tool" => {
                        if let Some(content) = msg.content() {
                            let preview: String = content.chars().take(300).collect();
                            md.push_str(&format!("\n> 工具输出：\n> ```\n> {preview}\n> ```\n"));
                        }
                    }
                    _ => {}
                }
            }
            md.push_str("\n---\n");
        }
    }
    md
}
