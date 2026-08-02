// ============================================================================
// Peco Handlers — Axum HTTP 端点
// ============================================================================
//
// 提供：
//   - GET  /api/peco/stream?message=xxx   SSE 流式对话
//   - GET  /api/peco/session               会话快照
//   - DELETE /api/peco/session              清除/重置会话

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::sse::{KeepAlive, Sse};
use axum::routing::get;
use futures::stream::Stream;
use peco_core::agent::AgentLooper;
use peco_core::persistence::SessionPersister;
use peco_core::session::Session;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::auth::AuthUser;
use crate::chat::sse::{ChatSseEvent, UsageData, map_looper_event};
use crate::error::ApiError;
use crate::session_store::SqliteSessionPersister;
use crate::state::AppState;

use super::filter::PecoMessageFilter;
use super::manager::PecoManager;
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

/// 导出格式查询参数。
#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default = "default_export_format")]
    pub format: String,
}

fn default_export_format() -> String {
    "json".to_string()
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

// ── WatcherGuard ──────────────────────────────────────────────────────────

/// Drop 时自动释放 FileWatcher 引用计数，防止 panic 导致泄漏。
///
/// SSE spawned task 的 RAII 守卫 — 即使 task 在 event loop 之外 panic，
/// FileWatcher 也会在 unwinding 时通过 Drop 正确释放。
struct WatcherGuard {
    app_state: Option<Arc<AppState>>,
    user_id: String,
}

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        if let Some(state) = self.app_state.take() {
            state.workspace_manager.release_watcher(&self.user_id);
        }
    }
}

// ── Handler: GET /api/peco/stream ────────────────────────────────────────

/// SSE 流式对话。
///
/// 核心流程：
/// 1. 初始化 PecoManager（含幂等模板安装、Agent 加载）
/// 2. 加载/创建 Perpetual Session
/// 3. 构建 LooperConfig（从 PecoConfig）+ AgentLooper
/// 4. 事件循环：LooperEvent → ChatSseEvent → SSE
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
    let manager = PecoManager::new(&state, &user_id).await?;

    // 启动 FileWatcher（SSE 连接建立）
    state.workspace_manager.acquire_watcher(&user_id, &state.db);

    let session_id = private_session_id(&user_id);
    let app_state = Arc::clone(&state);
    let conv_id = session_id.clone();

    // ── 2. 加载或创建 Perpetual Session ──────────────────────────────
    let persister = SqliteSessionPersister::new(state.db.clone());
    let session: Box<Session> = match persister.load(&session_id).await {
        Ok(Some((snapshot, _meta))) => {
            tracing::info!(
                user_id = %user_id,
                turns = snapshot.committed_turns.len(),
                "Peco session restored"
            );
            let created_at = snapshot
                .committed_turns
                .first()
                .and_then(|t| t.first())
                .map(|m| m.timestamp_ms / 1000)
                .unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_else(|_| Duration::from_secs(0))
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
            tracing::info!(user_id = %user_id, "Creating new Peco session");
            Box::new(Session::new(session_id.clone(), SESSION_TITLE.to_string()))
        }
    };

    // ── 3. 创建 SSE channel ─────────────────────────────────────────────
    let (sse_tx, sse_rx) = mpsc::channel::<Result<axum::response::sse::Event, Infallible>>(256);

    // ── 4. 克隆值给后台任务 ────────────────────────────────────────────
    let agent = Arc::clone(manager.agent());
    let config = manager.config().clone();
    let conv_id_bg = conv_id.clone();
    let message_bg = message.clone();
    let user_id_bg = user_id.clone();

    tokio::spawn(async move {
        // RAII guard：无论 task 如何退出（正常/panic），都释放 FileWatcher
        let _watcher_guard = WatcherGuard {
            app_state: Some(app_state.clone()),
            user_id: user_id_bg,
        };
        let persister: Arc<dyn SessionPersister> =
            Arc::new(SqliteSessionPersister::new(app_state.db.clone()));

        // 构建 LooperConfig（从 PecoConfig + MessageFilter）
        let looper_config = config.to_looper_config(Arc::new(PecoMessageFilter::new(
            config.max_history_messages,
        )));

        let handle = AgentLooper::spawn(agent, session, looper_config, persister.clone());

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

        // ── 5. 事件循环：LooperEvent → SSE ──────────────────────────────
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

    // ── 6. 返回 SSE 响应 ─────────────────────────────────────────────────
    let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

// ── Handler: GET /api/peco/session ──────────────────────────────────────

/// 获取 Peco 永续会话快照。
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
        "Peco session snapshot returned"
    );

    Ok(Json(SessionSnapshotResponse {
        conversation_id: session_id,
        turns,
        total_usage: usage,
    }))
}

// ── Handler: DELETE /api/peco/session ───────────────────────────────────

/// 清除 Peco 永续会话（重置对话）。
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
        .map_err(|e| ApiError::Internal(format!("failed to clear Peco session: {e}")))?;

    tracing::info!(
        user_id = %user_id,
        session_id = %session_id,
        "Peco session cleared"
    );

    Ok(Json(SuccessResponse {
        success: true,
        message: Some("Session cleared".to_string()),
    }))
}

// ── Router ─────────────────────────────────────────────────────────────────

/// `GET /api/peco/session/export?format=json|markdown`
pub async fn export_session(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ExportQuery>,
) -> Result<axum::response::Response, ApiError> {
    let session_id = private_session_id(&user_id);
    let persister = SqliteSessionPersister::new(state.db.clone());
    let snapshot_opt = persister
        .load(&session_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to load session: {e}")))?;

    match params.format.as_str() {
        "markdown" => {
            let md = crate::chat::handler::snapshot_to_markdown(&snapshot_opt, &session_id);
            Ok(axum::response::Response::builder()
                .header("Content-Type", "text/markdown; charset=utf-8")
                .header(
                    "Content-Disposition",
                    format!("attachment; filename=\"peco-session-{session_id}.md\""),
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
                    format!("attachment; filename=\"peco-session-{session_id}.json\""),
                )
                .body(axum::body::Body::from(json))
                .unwrap())
        }
    }
}

/// 构建 Peco 路由。
///
/// 注册到 `/api/peco`：
/// - `GET /stream` — SSE 流式对话
/// - `GET /session` — 获取会话快照
/// - `DELETE /session` — 清除会话
/// - `GET /session/export` — 导出会话
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/stream", get(stream_chat))
        .route("/session", get(get_session_snapshot).delete(clear_session))
        .route("/session/export", get(export_session))
}
