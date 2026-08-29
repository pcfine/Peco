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
use model_provider::InputItem;
use peco_core::agent::{AgentLooper, strip_summary_wrapper};
use peco_core::persistence::SessionPersister;
use peco_core::session::Session;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::auth::AuthUser;
use crate::chat::sse::{ChatSseEvent, UsageData, map_looper_event};
use crate::error::ApiError;
use crate::session_dto::group_input_items;
use crate::session_store::SqliteSessionPersister;
use crate::state::AppState;

use super::filter::PecoContextFilter;
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

/// 单条压缩记录（时间线条目）。
#[derive(Debug, Serialize)]
pub struct CompactionRecord {
    /// 发生时间（SQLite datetime 字符串，UTC）。
    pub at: String,
    pub evicted_turns: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
    /// 压缩后摘要正文字符数（观测摘要质量漂移的长度曲线）。
    pub summary_chars: usize,
}

/// 上下文指标（压缩与 Verbatim 预算的占用情况）。
#[derive(Debug, Serialize)]
pub struct ContextMetrics {
    /// 压缩触发口径：pinned 摘要 + 全部 committed 轮（含 tool 输出与 reasoning）
    /// 的估算 token。与 `compaction_trigger_tokens` 同口径可直接比较。
    pub estimated_total_tokens: usize,
    /// Verbatim 预算口径：历史轮中 viewable（User/Assistant 文本）条目的估算 token，
    /// 从最新轮往回整轮计入。与 `history_token_budget` 同口径可直接比较。
    pub estimated_view_tokens: usize,
    pub pinned_summary_tokens: usize,
    pub history_token_budget: usize,
    pub compaction_trigger_tokens: usize,
    /// 累计压缩次数。
    pub compaction_count: usize,
    /// 压缩时间线（时间正序）。
    pub compactions: Vec<CompactionRecord>,
}

/// 会话快照响应。
#[derive(Debug, Serialize)]
pub struct SessionSnapshotResponse {
    pub conversation_id: String,
    pub turns: Vec<TurnData>,
    pub total_usage: UsageData,
    /// 钉扎的历史摘要（compaction 产物）。无压缩历史时缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_summary: Option<String>,
    /// 上下文指标。会话不存在时缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_metrics: Option<ContextMetrics>,
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
        Err(e) => {
            tracing::warn!(
                user_id = %user_id,
                error = %e,
                "Peco 会话快照加载失败，创建新会话（历史会话丢失）"
            );
            Box::new(Session::new(session_id.clone(), SESSION_TITLE.to_string()))
        }
        Ok(None) => {
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

        // 构建 LooperConfig（从 PecoConfig + 统一上下文过滤器）
        let looper_config = config.to_looper_config(Arc::new(PecoContextFilter::new(
            config.history_token_budget,
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

    let (turns, usage, pinned_summary, context_metrics) = match snapshot_opt {
        Some((snap, _meta)) => {
            let pinned_summary: Option<String> =
                snap.pinned_summary
                    .as_ref()
                    .and_then(|am| match am.message.as_ref() {
                        // 剥离定界标签 — 下发给前端的是纯正文（归档分隔条 hover 展示用）
                        InputItem::Message { content, .. } => {
                            Some(strip_summary_wrapper(content).to_string())
                        }
                        _ => None,
                    });
            let turns: Vec<TurnData> = snap
                .committed_turns
                .iter()
                .enumerate()
                .map(|(i, msgs): (usize, &Vec<_>)| TurnData {
                    turn_index: i,
                    messages: {
                        let items: Vec<InputItem> =
                            msgs.iter().map(|am| (*am.message).clone()).collect();
                        let timestamps: Vec<u64> = msgs.iter().map(|am| am.timestamp_ms).collect();
                        group_input_items(&items, &timestamps)
                            .into_iter()
                            .map(|msg| MessageData {
                                role: msg.role.to_string(),
                                content: msg.content,
                                tool_calls: if msg.tool_calls.is_empty() {
                                    None
                                } else {
                                    Some(
                                        msg.tool_calls
                                            .into_iter()
                                            .map(|tc| ToolCallData {
                                                id: tc.id,
                                                name: tc.function.name,
                                                arguments: tc.function.arguments,
                                            })
                                            .collect(),
                                    )
                                },
                                reasoning_content: msg.reasoning_content,
                                tool_call_id: msg.tool_call_id,
                                timestamp_ms: msg.timestamp_ms,
                            })
                            .collect()
                    },
                })
                .collect();

            let usage = UsageData {
                input_tokens: snap.total_usage.input_tokens,
                output_tokens: snap.total_usage.output_tokens,
            };

            // ── 上下文指标 ──────────────────────────────────────────────
            // 预算阈值取默认配置 — GET /session 不构建 PecoManager（无模板
            // 安装等重副作用），阈值实际为常量，口径注释见 PecoConfig。
            let peco_config = super::config::PecoConfig::default();
            let est =
                super::metrics::estimate_session_context(&snap, peco_config.history_token_budget);
            let compactions =
                crate::db::compaction_log::list_by_conversation(&state.db, &user_id, &session_id)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|row| CompactionRecord {
                        at: row.created_at,
                        evicted_turns: row.evicted_turns as usize,
                        tokens_before: row.tokens_before as usize,
                        tokens_after: row.tokens_after as usize,
                        summary_chars: row.summary_chars as usize,
                    })
                    .collect::<Vec<_>>();
            let compaction_count = compactions.len();
            let context_metrics = ContextMetrics {
                estimated_total_tokens: est.total_tokens,
                estimated_view_tokens: est.view_tokens,
                pinned_summary_tokens: est.pinned_tokens,
                history_token_budget: peco_config.history_token_budget,
                compaction_trigger_tokens: peco_config.compaction_trigger_tokens,
                compaction_count,
                compactions,
            };

            (turns, usage, pinned_summary, Some(context_metrics))
        }
        None => (
            Vec::new(),
            UsageData {
                input_tokens: 0,
                output_tokens: 0,
            },
            None,
            None,
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
        pinned_summary,
        context_metrics,
    }))
}

// ── Handler: DELETE /api/peco/session ───────────────────────────────────

/// `DELETE /api/peco/session?archive=true|false` 的查询参数。
#[derive(Debug, Deserialize)]
pub struct ClearQuery {
    /// 归档式清空（默认 true）：清空前先将会话全文导出存入
    /// `peco_session_archives` 表，避免误删即永久丢失。
    /// 显式传 `false` 跳过归档（隐私场景硬删除）。
    #[serde(default = "default_archive")]
    pub archive: bool,
}

fn default_archive() -> bool {
    true
}

/// 清除 Peco 永续会话（重置对话）。
///
/// 归档式（默认）：先将会话全文（含 pinned 摘要与用量元数据）导出为
/// Markdown 存入 `peco_session_archives` 表，再删除快照 — 归档失败时
/// 中止删除，快照保持不动，保证信息不丢失。
/// 快照损坏 / 旧格式无法 load 时跳过归档但仍执行删除，保留用户重置能力。
/// 压缩日志（`peco_compaction_log`）随会话一并清理。
/// 下次对话将创建全新的 Session。
pub async fn clear_session(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ClearQuery>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let session_id = private_session_id(&user_id);
    let persister = SqliteSessionPersister::new(state.db.clone());

    // ── 1. 清理压缩日志（先于快照删除 — 失败时快照未动，可安全重试）──────
    // conversation_id 清空重置后复用，日志必须随会话生命周期回收，
    // 否则新会话的指标被旧会话污染。
    crate::db::compaction_log::delete_by_conversation(&state.db, &user_id, &session_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to clear compaction log: {e}")))?;

    // ── 2. 加载快照（尽力而为）────────────────────────────────────────
    // 损坏 / 旧格式快照不阻断清空 — 跳过归档、删除照常执行，保留用户重置能力
    //（archive=false 的隐私硬删除尤其不能因 load 失败而失效）。
    let mut load_failed = false;
    let snapshot_opt = match persister.load(&session_id).await {
        Ok(snapshot) => snapshot,
        Err(e) => {
            tracing::warn!(
                user_id = %user_id,
                session_id = %session_id,
                error = %e,
                "Peco session snapshot load failed; skipping archive but clearing anyway"
            );
            load_failed = true;
            None
        }
    };

    // ── 3. 归档（默认开启；失败则中止删除）────────────────────────────
    if params.archive && !load_failed {
        let snapshot = match snapshot_opt {
            Some(ref snap) => snap,
            None => {
                tracing::info!(
                    user_id = %user_id,
                    session_id = %session_id,
                    "Peco session clear: nothing to archive or clear"
                );
                return Ok(Json(SuccessResponse {
                    success: true,
                    message: Some("Session already empty".to_string()),
                }));
            }
        };

        let md = crate::chat::handler::archive_markdown(
            &snapshot_opt,
            &session_id,
            &chrono::Utc::now().to_rfc3339(),
        );

        crate::db::session_archive::insert(
            &state.db,
            &uuid::Uuid::new_v4().to_string(),
            &user_id,
            &session_id,
            snapshot.0.committed_turns.len(),
            snapshot.0.total_usage.input_tokens as u64,
            snapshot.0.total_usage.output_tokens as u64,
            &md,
        )
        .await
        .map_err(|e| ApiError::Internal(format!("failed to archive session before clear: {e}")))?;
    }

    // ── 2. 删除快照 ─────────────────────────────────────────────────────
    persister
        .delete(&session_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to clear Peco session: {e}")))?;

    tracing::info!(
        user_id = %user_id,
        session_id = %session_id,
        archived = params.archive,
        "Peco session cleared"
    );

    Ok(Json(SuccessResponse {
        success: true,
        message: Some(if params.archive {
            "Session archived and cleared".to_string()
        } else {
            "Session cleared".to_string()
        }),
    }))
}

// ── Handler: GET /api/peco/archives ─────────────────────────────────────

/// 归档列表项。
#[derive(Debug, Serialize)]
pub struct SessionArchiveItem {
    pub id: String,
    pub conversation_id: String,
    pub turn_count: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub created_at: String,
}

/// 列出当前用户的会话归档。
pub async fn list_archives(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<SessionArchiveItem>>, ApiError> {
    let rows = crate::db::session_archive::list_by_user(&state.db, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to list archives: {e}")))?;

    Ok(Json(
        rows.into_iter()
            .map(|r| SessionArchiveItem {
                id: r.id,
                conversation_id: r.conversation_id,
                turn_count: r.turn_count as usize,
                total_input_tokens: r.total_input_tokens as u64,
                total_output_tokens: r.total_output_tokens as u64,
                created_at: r.created_at,
            })
            .collect(),
    ))
}

/// 下载一条归档（Markdown）。限定所属用户 — 防越权读取。
pub async fn download_archive(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    axum::extract::Path(archive_id): axum::extract::Path<String>,
) -> Result<axum::response::Response, ApiError> {
    let row = crate::db::session_archive::get(&state.db, &user_id, &archive_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to load archive: {e}")))?
        .ok_or_else(|| ApiError::NotFound("archive not found".into()))?;

    Ok(axum::response::Response::builder()
        .header("Content-Type", "text/markdown; charset=utf-8")
        .header(
            "Content-Disposition",
            format!(
                "attachment; filename=\"peco-archive-{}.md\"",
                row.created_at
            ),
        )
        .body(axum::body::Body::from(row.content_md))
        .unwrap())
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
/// - `DELETE /session` — 清除会话（默认先归档，`?archive=false` 硬删除）
/// - `GET /session/export` — 导出会话
/// - `GET /archives` — 归档列表
/// - `GET /archives/:id` — 下载归档
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/stream", get(stream_chat))
        .route("/session", get(get_session_snapshot).delete(clear_session))
        .route("/session/export", get(export_session))
        .route("/archives", get(list_archives))
        .route("/archives/{id}", get(download_archive))
}
