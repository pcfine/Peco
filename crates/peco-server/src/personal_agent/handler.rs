// ============================================================================
// Personal Agent Handlers — Axum HTTP 端点
// ============================================================================
//
// 提供：
//   - GET  /api/personal-agent/stream?message=xxx   SSE 流式对话
//   - GET  /api/personal-agent/session               会话快照
//   - DELETE /api/personal-agent/session               清除/重置会话
//
// 与 chat/handler.rs 的关键差异：
//   - Sub-agent 事件映射通过 WorkspaceManager::get_agent()（AgentLoader trait）
//     解析 agent 信息，而非 SQLite agents 表
//   - 无 conversation CRUD——固定 perpetual session
//   - 复用 chat::sse 的 ChatSseEvent / map_looper_event（通用，不依赖 DB）

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::sse::{KeepAlive, Sse};
use axum::routing::get;
use futures::stream::Stream;
use peco_core::agent::{AgentLooper, LooperConfig};
use peco_core::persistence::SessionPersister;
use peco_core::session::Session;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::auth::AuthUser;
use crate::chat::sse::{ChatSseEvent, UsageData, map_looper_event};
use crate::error::ApiError;
use crate::session_store::SqliteSessionPersister;
use crate::state::AppState;

use super::filter::PersonalAgentMessageFilter;
use super::manager::PersonalAgentManager;
use super::session::{SESSION_TITLE, private_session_id};

// ── Request / Response 类型 ─────────────────────────────────────────────────

/// SSE 流式查询参数。
#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    pub message: String,
}

/// 简单成功响应。
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// 工具调用简化格式。
#[derive(Debug, Serialize)]
pub struct ToolCallData {
    pub id: String,
    pub name: String,
    pub arguments: String,
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

/// 单轮对话数据。
#[derive(Debug, Serialize)]
pub struct TurnData {
    pub turn_index: usize,
    pub messages: Vec<MessageData>,
}

/// 会话快照响应。
#[derive(Debug, Serialize)]
pub struct SessionSnapshotResponse {
    pub conversation_id: String,
    pub turns: Vec<TurnData>,
    pub total_usage: UsageData,
}

// ── Handler: GET /api/personal-agent/stream ────────────────────────────────

/// SSE 流式对话。
///
/// 核心流程：
/// 1. 初始化 PersonalAgentManager（含幂等模板安装、Agent 加载）
/// 2. 加载/创建 Perpetual Session
/// 3. 创建 SSE channel + 后台 tokio 任务
/// 4. 构建 LooperConfig（无 PPA 组件）+ AgentLooper
/// 5. 事件循环：LooperEvent → ChatSseEvent → SSE
pub async fn stream_chat(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>>, ApiError> {
    let message = params.message.trim().to_string();
    if message.is_empty() {
        return Err(ApiError::BadRequest("message is required".into()));
    }

    // ── 1. 初始化 Manager ──────────────────────────────────────────────
    let manager = PersonalAgentManager::new(&state, &user_id).await?;

    let session_id = private_session_id(&user_id);
    let conv_id = session_id.clone();

    // ── 2. 加载或创建 Perpetual Session ──────────────────────────────
    let persister = SqliteSessionPersister::new(state.db.clone());
    let session: Box<Session> = match persister.load(&session_id).await {
        Ok(Some((snapshot, _meta))) => {
            tracing::info!(
                user_id = %user_id,
                turns = snapshot.committed_turns.len(),
                "Personal agent session restored"
            );
            let created_at = snapshot
                .committed_turns
                .first()
                .and_then(|t| t.first())
                .map(|m| m.timestamp_ms / 1000)
                .unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_else(|_| std::time::Duration::from_secs(0))
                        .as_secs()
                });
            Box::new(Session::from_snapshot(
                session_id.clone(),
                SESSION_TITLE.to_string(),
                created_at,
                snapshot,
            ))
        }
        _ => {
            tracing::info!(user_id = %user_id, "Creating new personal agent session");
            Box::new(Session::new(session_id.clone(), SESSION_TITLE.to_string()))
        }
    };

    // ── 3. 创建 SSE channel ─────────────────────────────────────────
    let (sse_tx, sse_rx) = mpsc::channel::<Result<axum::response::sse::Event, Infallible>>(256);

    // ── 4. 克隆值给后台任务 ────────────────────────────────────────
    let agent = Arc::clone(manager.agent());
    let conv_id_bg = conv_id.clone();
    let message_bg = message.clone();
    let db_bg = state.db.clone();

    tokio::spawn(async move {
        let persister: Arc<dyn SessionPersister> =
            Arc::new(SqliteSessionPersister::new(db_bg.clone()));

        // 构建 LooperConfig（无 PPA 组件）
        let config = LooperConfig {
            event_buffer: 256,
            per_turn_timeout: Some(Duration::from_secs(300)),
            total_timeout: Some(Duration::from_secs(1800)),
            persist_on_failure: true,
            dynamic_context: None,
            hooks: Vec::new(),
            message_filter: Some(Arc::new(PersonalAgentMessageFilter::new(10))),
            ..LooperConfig::default()
        };

        let handle = AgentLooper::spawn(agent, session, config, persister.clone());

        // 发送用户消息
        if let Err(e) = handle.send_query(message_bg.clone()).await {
            let err_event = ChatSseEvent::Error {
                message: format!("Failed to send message: {e}"),
                conversation_id: conv_id_bg.clone(),
            };
            if let Ok(ev) = err_event.to_sse_event() {
                let _ = sse_tx.send(Ok(ev)).await;
            }
            return;
        }

        // 事件循环：LooperEvent → SSE
        loop {
            match handle.recv_event().await {
                Some(peco_core::agent::LooperEvent::Shutdown { total_usage, .. }) => {
                    let done_ev = ChatSseEvent::Done {
                        usage: UsageData::from(total_usage),
                        conversation_id: conv_id_bg.clone(),
                    };
                    if let Ok(ev) = done_ev.to_sse_event() {
                        let _ = sse_tx.send(Ok(ev)).await;
                    }
                    break;
                }

                Some(event) => {
                    if let Some(sse_ev) = map_looper_event(event, &conv_id_bg)
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

    // ── 5. 返回 SSE 响应 ─────────────────────────────────────────
    let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

// ── Handler: GET /api/personal-agent/session ──────────────────────────────

/// 获取个人助理会话快照。
///
/// 返回完整的 turn 历史（含 tool calls、reasoning_content），
/// 供前端刷新页面后重建聊天 UI。
pub async fn get_session_snapshot(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<SessionSnapshotResponse>, ApiError> {
    let session_id = private_session_id(&user_id);
    let persister = SqliteSessionPersister::new(state.db.clone());

    let snapshot_opt = persister
        .load(&session_id)
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

    tracing::debug!(
        user_id = %user_id,
        session_id = %session_id,
        turn_count = turns.len(),
        "Personal agent session snapshot returned"
    );

    Ok(Json(SessionSnapshotResponse {
        conversation_id: session_id,
        turns,
        total_usage: usage,
    }))
}

// ── Handler: DELETE /api/personal-agent/session ───────────────────────────

/// 清除个人助理会话（重置对话）。
///
/// 删除 session_snapshots 表中的持久化记录。
/// 下次对话将创建全新的 Session。
pub async fn clear_session(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let session_id = private_session_id(&user_id);
    let persister = SqliteSessionPersister::new(state.db.clone());

    persister
        .delete(&session_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to clear personal agent session: {e}")))?;

    tracing::info!(
        user_id = %user_id,
        session_id = %session_id,
        "Personal agent session cleared"
    );

    Ok(Json(SuccessResponse {
        success: true,
        message: Some("Session cleared".to_string()),
    }))
}

// ── Router ─────────────────────────────────────────────────────────────────

/// 构建个人助理路由。
///
/// 注册到 `/api/personal-agent`：
/// - `GET /stream` — SSE 流式对话
/// - `GET /session` — 获取会话快照
/// - `DELETE /session` — 清除会话
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/stream", get(stream_chat))
        .route("/session", get(get_session_snapshot).delete(clear_session))
}
