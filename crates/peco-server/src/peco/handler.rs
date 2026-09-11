// ============================================================================
// Peco Handlers — Axum HTTP 端点
// ============================================================================
//
// 提供：
//   - GET  /api/peco/stream?message=xxx   SSE 流式对话（无 message 时为纯附着模式）
//   - POST /api/peco/stream/cancel        取消进行中的任务
//   - GET  /api/peco/session               会话快照
//   - DELETE /api/peco/session              清除/重置会话
//   - GET  /api/peco/memory/audit           记忆删除审计（分页）
//   - POST /api/peco/memory/audit/:id/restore  按审计行回滚删除
//
// 任务生命周期与 SSE 连接解耦：runner 任务独占 LooperHandle 持续驱动，
// 桥接任务把 broadcast 事件流转发给每个 SSE 连接。连接断开只结束桥接，
// 轮次继续执行；重开页面通过 subscribe() 重新附着。轮边界（Idle）且无
// 订阅者时 runner 回收 looper，避免无人观看的停靠任务占用资源。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::sse::{KeepAlive, Sse};
use axum::routing::{get, post};
use futures::stream::Stream;
use model_provider::InputItem;
use peco_core::agent::{AgentLooper, LooperEvent, LooperHandle, OuterState, strip_summary_wrapper};
use peco_core::knowledge::KnowledgeModuleError;
use peco_core::persistence::SessionPersister;
use peco_core::session::Session;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, broadcast, mpsc};
use tracing::{info, warn};

use crate::auth::AuthUser;
use crate::chat::sse::{ChatSseEvent, UsageData, map_looper_event};
use crate::error::ApiError;
use crate::peco::active::{CancelWaitResult, ControlCommand, RunGuard, RunRegistration};
use crate::session_dto::group_input_items;
use crate::session_store::SqliteSessionPersister;
use crate::state::AppState;

use super::filter::PecoContextFilter;
use super::manager::PecoManager;
use super::session::{SESSION_TITLE, private_session_id};

/// 附着时等待将死 run 退出的上限（回收只发生在停靠态，退出是毫秒级）。
const RUN_EXIT_WAIT: Duration = Duration::from_secs(2);

/// 清除会话时等待 run 收尾的上限；超时转后台兜底补删。
const CLEAR_RUN_WAIT: Duration = Duration::from_secs(10);

// ── Request / Response 类型 ─────────────────────────────────────────────────

/// SSE 流式查询参数。
///
/// `message` 缺省时为纯附着模式：接上当前用户进行中的任务流
/// （重开页面的场景）；无进行中任务时返回 400。
#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    pub message: Option<String>,
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
    /// 用户消息携带的图片部件 URL（含 data URI），无图时不序列化。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
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
    /// 是否有进行中的任务（前端据此重新附着到任务流）。
    pub is_running: bool,
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
/// 桥接任务的 RAII 守卫 — 即使 task 在 event loop 之外 panic，
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
/// 语义四象限（按是否有活跃 run / 是否携带 message）：
/// - 有 run + 有 message：附着到 run 并把消息排队（looper Active 时入 pending 队列）
/// - 有 run + 无 message：纯附着（重开页面接上进行中的任务）
/// - 无 run + 有 message：抢注注册表 → 构建（PecoManager/Session/Looper）→ 启动 runner
/// - 无 run + 无 message：400
///
/// 抢注失败（并发请求抢先注册）或附着到将死 run（enqueue 失败）时重试，
/// 最终收敛到「附着到健康 run」或「自己新建 run」。
pub async fn stream_chat(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>>, ApiError> {
    let message = params
        .message
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());

    for _ in 0..3 {
        // ── 尝试附着到已有 run ─────────────────────────────────────────
        if let Some(rx) = state.peco_runs.subscribe(&user_id) {
            let mut delivered = true;
            if let Some(text) = &message {
                // 先 subscribe 再 enqueue：持有一个 receiver 后 run 不可能被
                // 回收（try_reclaim 要求订阅者数为 0），消息必然送达。
                delivered = state.peco_runs.enqueue_query(&user_id, text.clone());
            }
            if delivered {
                return Ok(bridge_sse_response(Arc::clone(&state), user_id.clone(), rx));
            }
            // 订阅到的是正在收尾的 run：等它退出后重试（新建或附着到新 run）
            state
                .peco_runs
                .wait_until_absent(&user_id, RUN_EXIT_WAIT)
                .await;
            continue;
        }

        // ── 无 run：新建需要 message ───────────────────────────────────
        let Some(text) = message.clone() else {
            return Err(ApiError::BadRequest(
                "message is required（当前无进行中的任务可附着）".into(),
            ));
        };

        // 原子抢注先于耗时的构建：构建期间其他连接可附着到本 run
        let Some(registration) = state.peco_runs.try_register(&user_id) else {
            continue; // 并发请求抢先注册 → 下一轮循环附着到它
        };

        // 抢注成功后立即自订阅（保证 looper 启动期的第一手事件不丢失）
        let rx = state
            .peco_runs
            .subscribe(&user_id)
            .ok_or_else(|| ApiError::Internal("peco run registry inconsistent".into()))?;

        // 注册产物所有权移交 spawn_peco_run → runner 任务；其内部任一步
        // 失败时 registration 在该函数帧内 drop → guard 清理注册表。
        spawn_peco_run(&state, &user_id, text, registration).await?;
        return Ok(bridge_sse_response(Arc::clone(&state), user_id.clone(), rx));
    }

    Err(ApiError::Internal(
        "peco stream attach/new-run retry exhausted".into(),
    ))
}

/// 加载或创建 Peco 永续 Session（快照损坏时降级为新会话）。
async fn load_or_new_session(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<Box<Session>, ApiError> {
    let session_id = private_session_id(user_id);
    let persister = SqliteSessionPersister::new(state.db.clone());
    let session: Box<Session> = match persister.load(&session_id).await {
        Ok(Some((snapshot, _meta))) => {
            info!(
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
            warn!(
                user_id = %user_id,
                error = %e,
                "Failed to load Peco session snapshot, creating a new session (history lost)"
            );
            Box::new(Session::new(session_id.clone(), SESSION_TITLE.to_string()))
        }
        Ok(None) => {
            info!(user_id = %user_id, "Creating new Peco session");
            Box::new(Session::new(session_id.clone(), SESSION_TITLE.to_string()))
        }
    };
    Ok(session)
}

/// 构建并启动一个 Peco run：PecoManager → Session → Looper → runner 任务。
///
/// 构建失败时 `registration` 在本函数帧内 drop → guard 清理注册表；
/// 成功时注册产物整体移入 runner 任务，随其退出而清理。
async fn spawn_peco_run(
    state: &Arc<AppState>,
    user_id: &str,
    message: String,
    registration: RunRegistration,
) -> Result<(), ApiError> {
    // ── 1. PecoManager（重活：模板幂等安装 + @assistant 加载 + 记忆/压缩装配）──
    let manager = PecoManager::new(state, user_id).await?;

    // ── 2. 永续 Session ──────────────────────────────────────────────────
    let session = load_or_new_session(state, user_id).await?;

    // ── 3. Looper ────────────────────────────────────────────────────────
    let agent = Arc::clone(manager.agent());
    let config = manager.config().clone();
    let looper_config = config.to_looper_config(Arc::new(PecoContextFilter::new(
        config.history_token_budget,
    )));
    let persister: Arc<dyn SessionPersister> =
        Arc::new(SqliteSessionPersister::new(state.db.clone()));
    let handle = AgentLooper::spawn(agent, session, looper_config, persister);

    // ── 4. runner 任务（注册产物移入，guard 随其退出 drop）───────────────
    let RunRegistration {
        control_rx,
        event_tx,
        reclaim_notify,
        _guard,
    } = registration;
    let runs = Arc::clone(&state.peco_runs);
    let uid = user_id.to_string();

    tokio::spawn(runner_loop(
        handle,
        control_rx,
        event_tx,
        reclaim_notify,
        _guard,
        runs,
        uid,
        message,
    ));
    Ok(())
}

/// runner 任务：独占 LooperHandle，驱动 looper 并向订阅者广播事件。
///
/// 三路 select：
/// - looper 事件 → 广播；`Shutdown` 结束；`OuterStateChange → Idle`（轮边界
///   且无排队输入）且无订阅者 → 回收退出
/// - 控制命令 → `Query` 排队消息 / `Cancel` 取消在途轮次（不 break，继续
///   转发收尾事件让附着端看到 error + done）
/// - 回收唤醒（桥接退出时 `request_reclaim`）→ looper 停靠且无订阅者 → 回收
///
/// 退出即 drop handle：user_speaker 消亡使停靠的 looper 经
/// `input_closed + Idle` 优雅终止；`_guard` drop 清理注册表。
#[allow(clippy::too_many_arguments)]
async fn runner_loop(
    handle: LooperHandle,
    mut control_rx: mpsc::Receiver<ControlCommand>,
    event_tx: broadcast::Sender<LooperEvent>,
    reclaim_notify: Arc<Notify>,
    _guard: RunGuard,
    runs: Arc<crate::peco::active::PecoActiveRuns>,
    user_id: String,
    initial_message: String,
) {
    // 初始消息由 runner 发送：此处 looper 必为 Idle，直通开启新一轮。
    if let Err(e) = handle.send_query(initial_message).await {
        warn!(user_id = %user_id, error = %e, "Failed to send initial query; runner exiting");
        return;
    }

    // 首个 OuterStateChange 事件之前保守不回收（looper 启动期不属于停靠态）
    let mut looper_idle = false;

    loop {
        tokio::select! {
            event = handle.recv_event() => match event {
                Some(ev) => {
                    if let LooperEvent::OuterStateChange { to, .. } = &ev {
                        looper_idle = matches!(to, OuterState::Idle);
                    }
                    // 无订阅者时广播失败是常态（Err 忽略）
                    let _ = event_tx.send(ev.clone());
                    if matches!(ev, LooperEvent::Shutdown { .. }) {
                        break;
                    }
                    if looper_idle && runs.try_reclaim_if_unsubscribed(&user_id) {
                        info!(user_id = %user_id, "No active connection at turn boundary, recycling docked looper");
                        break;
                    }
                }
                None => break,
            },
            cmd = control_rx.recv() => match cmd {
                Some(ControlCommand::Query(text)) => {
                    if let Err(e) = handle.send_query(text).await {
                        warn!(user_id = %user_id, error = %e, "Failed to enqueue user query");
                    }
                }
                Some(ControlCommand::Cancel) => handle.cancel(),
                None => break,
            },
            _ = reclaim_notify.notified() => {
                if looper_idle && runs.try_reclaim_if_unsubscribed(&user_id) {
                    info!(user_id = %user_id, "All subscribers disconnected and looper docked, recycling looper");
                    break;
                }
            }
        }
    }
}

/// 将 run 的 broadcast 事件流桥接为一个 SSE 响应（每个连接一个桥接任务）。
///
/// 连接断开只结束桥接任务并唤醒 runner 复查回收，运行中的轮次不受影响；
/// 重开页面重新调用本端点即可重新附着。
fn bridge_sse_response(
    state: Arc<AppState>,
    user_id: String,
    mut rx: broadcast::Receiver<LooperEvent>,
) -> Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>> {
    let (sse_tx, sse_rx) = mpsc::channel::<Result<axum::response::sse::Event, Infallible>>(256);
    let conv_id = private_session_id(&user_id);
    let runs = Arc::clone(&state.peco_runs);
    let app_state = Arc::clone(&state);
    let uid = user_id;

    // FileWatcher 引用计数按连接管理（acquire 此处，release 由 guard 兜底）
    state.workspace_manager.acquire_watcher(&uid, &state.db);

    tokio::spawn(async move {
        // RAII：连接结束（正常/断开/panic）时释放 FileWatcher 引用计数
        let _watcher_guard = WatcherGuard {
            app_state: Some(app_state),
            user_id: uid.clone(),
        };
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Some(sse_ev) = map_looper_event(event, &conv_id)
                        && let Ok(ev) = sse_ev.to_sse_event()
                        && sse_tx.send(Ok(ev)).await.is_err()
                    {
                        warn!(
                            conversation_id = %conv_id,
                            "SSE client disconnected; run keeps executing in background"
                        );
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(conversation_id = %conv_id, skipped = n, "SSE subscribers lagging, dropping events");
                    let err_ev = ChatSseEvent::Error {
                        message: format!("事件消费过慢，{n} 条更新被跳过"),
                        conversation_id: conv_id.clone(),
                    };
                    if let Ok(ev) = err_ev.to_sse_event() {
                        let _ = sse_tx.send(Ok(ev)).await;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        // 桥接结束：唤醒 runner 复查回收（looper 已停靠且无其他订阅者时停止）
        runs.request_reclaim(&uid);
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// POST /api/peco/stream/cancel — 取消当前用户进行中的任务。
///
/// 无活跃 run 时 404。取消后 looper 在下个检查点收尾
/// （`TurnComplete{Failed}` → `Shutdown`），附着中的客户端会看到
/// error + done 事件；注册表条目随 runner 退出自动清理。
pub async fn cancel_stream(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<SuccessResponse>, ApiError> {
    if !state.peco_runs.cancel(&user_id) {
        return Err(ApiError::NotFound("no active peco run".into()));
    }
    info!(user_id = %user_id, "Peco run cancellation requested");
    Ok(Json(SuccessResponse {
        success: true,
        message: Some("cancellation requested".to_string()),
    }))
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
                            Some(strip_summary_wrapper(&content.text_view()).to_string())
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
                                images: msg.images,
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
        is_running: state.peco_runs.is_running(&user_id),
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

    // ── 0. 取消活跃 run 并尽量等其退出 ──────────────────────────────────
    // 否则 runner 在快照删除后仍会按轮边界落盘，覆盖刚清空的会话。
    // 模型调用在途时取消要等下个检查点，可能远超上限 — 超时转后台兜底补删。
    if state
        .peco_runs
        .cancel_and_wait(&user_id, CLEAR_RUN_WAIT)
        .await
        == CancelWaitResult::TimedOut
    {
        info!(
            user_id = %user_id,
            session_id = %session_id,
            "Peco run still finishing after cancel; scheduling follow-up snapshot delete"
        );
        let runs = Arc::clone(&state.peco_runs);
        let db = state.db.clone();
        let uid = user_id.clone();
        let sid = session_id.clone();
        tokio::spawn(async move {
            // 上限对齐 looper 的 per_turn_timeout（7200s）：取消后在途模型
            // 调用返回即收尾，不会更久。
            if runs
                .wait_until_absent(&uid, Duration::from_secs(2 * 60 * 60))
                .await
                && let Err(e) = SqliteSessionPersister::new(db).delete(&sid).await
            {
                warn!(user_id = %uid, error = %e, "Follow-up snapshot delete after late run exit failed");
            }
        });
    }

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

// ── Handler: GET /api/peco/memory/audit + POST /memory/audit/{id}/restore ──

/// 记忆审计查询分页参数。
#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    /// 页大小（默认 20，上限 100）。
    #[serde(default = "default_audit_limit")]
    pub limit: i64,
    /// 偏移量。
    #[serde(default)]
    pub offset: i64,
}

fn default_audit_limit() -> i64 {
    20
}

/// 单条记忆删除审计记录（含被删原文 — 仅供本人查阅）。
#[derive(Debug, Serialize)]
pub struct MemoryAuditItem {
    pub id: i64,
    pub kb_name: String,
    pub doc_id: String,
    pub title: String,
    pub content: String,
    pub source: String,
    pub reason: String,
    pub deleted_by: String,
    pub status: String,
    pub deleted_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restored_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restored_doc_id: Option<String>,
}

impl From<crate::db::memory_audit::MemoryAuditRow> for MemoryAuditItem {
    fn from(r: crate::db::memory_audit::MemoryAuditRow) -> Self {
        Self {
            id: r.id,
            kb_name: r.kb_name,
            doc_id: r.doc_id,
            title: r.title,
            content: r.content,
            source: r.source,
            reason: r.reason,
            deleted_by: r.deleted_by,
            status: r.status,
            deleted_at: r.deleted_at,
            restored_at: r.restored_at,
            restored_doc_id: r.restored_doc_id,
        }
    }
}

/// 分页列出当前用户的记忆删除审计（deleted_at 倒序）。
pub async fn list_memory_audit(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<AuditQuery>,
) -> Result<Json<Vec<MemoryAuditItem>>, ApiError> {
    let rows = crate::db::memory_audit::list_by_user(
        &state.db,
        &user_id,
        params.limit.clamp(1, 100),
        params.offset.max(0),
    )
    .await
    .map_err(|e| ApiError::Internal(format!("failed to list memory audit: {e}")))?;

    Ok(Json(rows.into_iter().map(MemoryAuditItem::from).collect()))
}

/// 回滚响应。
#[derive(Debug, Serialize)]
pub struct RestoreMemoryResponse {
    pub success: bool,
    pub doc_id: String,
    pub restored_at: String,
}

/// 按审计行回滚一条记忆删除：重放 `add_text`（doc_id 为内容哈希前缀，幂等复原），
/// 成功后回填 restored_at / restored_doc_id。
///
/// 他人审计行与不存在的行一律 404 — 不泄露记录的存在性。
pub async fn restore_memory_audit(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<RestoreMemoryResponse>, ApiError> {
    // 1. 归属校验：仅本人审计行可见可回滚
    let row = crate::db::memory_audit::get(&state.db, id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to load memory audit: {e}")))?
        .filter(|r| r.user_id == user_id)
        .ok_or_else(|| ApiError::NotFound(format!("memory audit record #{id} not found")))?;

    // 2. 状态校验：仅 done 且未回滚的行可回滚
    //   （pending 是未决的删除流程；cancelled 表示删除未发生，无需回滚）
    if row.status != "done" {
        return Err(ApiError::Conflict(format!(
            "审计行 #{id} 状态为 '{}'，仅 done 记录可回滚",
            row.status
        )));
    }
    if row.restored_at.is_some() {
        return Err(ApiError::Conflict(format!(
            "审计行 #{id} 已回滚，不可重复回滚"
        )));
    }

    // 3. 重放写入（内容哈希幂等 → doc_id 复原）
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let doc = ws
        .knowledge_manager()
        .add_text_to_kb(&row.kb_name, &row.title, &row.content, &row.source)
        .await
        .map_err(|e| match &e {
            KnowledgeModuleError::NotFound(name) => {
                ApiError::NotFound(format!("知识库 '{name}' 不存在，无法回滚审计行 #{id}"))
            }
            other => ApiError::Internal(format!("failed to replay add_text: {other}")),
        })?;

    if doc.id != row.doc_id {
        return Err(ApiError::Conflict(format!(
            "回滚后的文档 id '{}' 与审计行 doc_id '{}' 不一致（内容应逐字节一致）",
            doc.id, row.doc_id
        )));
    }

    // 4. 回填 restored_at / restored_doc_id
    let restored_at = chrono::Utc::now().to_rfc3339();
    let updated = crate::db::memory_audit::mark_restored(&state.db, id, &restored_at, &doc.id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to mark audit restored: {e}")))?;
    if updated == 0 {
        return Err(ApiError::Conflict(format!(
            "审计行 #{id} 状态已变化，本次回滚未记录"
        )));
    }

    tracing::info!(
        user_id = %user_id,
        audit_id = id,
        doc_id = %doc.id,
        kb = %row.kb_name,
        "Memory deletion restored from audit"
    );

    Ok(Json(RestoreMemoryResponse {
        success: true,
        doc_id: doc.id,
        restored_at,
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
/// - `GET /stream` — SSE 流式对话（无 message 时为纯附着模式）
/// - `POST /stream/cancel` — 取消进行中的任务
/// - `GET /session` — 获取会话快照
/// - `DELETE /session` — 清除会话（默认先归档，`?archive=false` 硬删除）
/// - `GET /session/export` — 导出会话
/// - `GET /archives` — 归档列表
/// - `GET /archives/:id` — 下载归档
/// - `GET /memory/audit` — 记忆删除审计（分页，仅本人）
/// - `POST /memory/audit/:id/restore` — 按审计行回滚删除
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/stream", get(stream_chat))
        .route("/stream/cancel", post(cancel_stream))
        .route("/session", get(get_session_snapshot).delete(clear_session))
        .route("/session/export", get(export_session))
        .route("/archives", get(list_archives))
        .route("/archives/{id}", get(download_archive))
        .route("/memory/audit", get(list_memory_audit))
        .route("/memory/audit/{id}/restore", post(restore_memory_audit))
}
