// ============================================================================
// Peco Handlers — Axum HTTP 端点
// ============================================================================
//
// 提供：
//   - GET  /api/peco/stream?message=xxx   SSE 流式对话（无 message 时为纯附着模式）
//   - POST /api/peco/stream/query         向已注册的 run 排队消息（不新开连接）
//   - POST /api/peco/stream/cancel        取消进行中的任务
//   - GET  /api/peco/session               会话快照
//   - DELETE /api/peco/session              清除/重置会话
//   - GET  /api/peco/memory/audit           记忆删除审计（分页）
//   - POST /api/peco/memory/audit/:id/restore  按审计行回滚删除
//   - POST /api/peco/memory/consolidate     手动触发一轮记忆自动整理（202 受理）
//   - GET  /api/peco/memory/consolidation/state  查询最近一次整理结果
//   - GET  /api/peco/memory/consolidation/optin  查询自动整理 opt-in 开关
//   - PUT  /api/peco/memory/consolidation/optin  写入自动整理 opt-in 开关
//   - GET  /api/peco/memory/graph                记忆实体子图（节点 + 谓词边）
//   - GET  /api/peco/memory/documents            记忆文档列表（分页，可按 source 过滤）
//   - GET  /api/peco/memory/documents/{id}       记忆文档详情（全文 + 元数据）
//   - GET  /api/peco/memory/search               记忆内容检索（正文子串扫描）
//   - GET  /api/peco/memory/supersede/health     取代对账健康计数
//   - POST /api/peco/memory/supersede/reconcile  手动触发一次取代对账
//
// 任务生命周期与 SSE 连接解耦：runner 任务独占 LooperHandle 持续驱动，
// 桥接任务把 broadcast 事件流转发给每个 SSE 连接。连接断开只结束桥接，
// 轮次继续执行；重开页面通过 subscribe() 重新附着。轮边界（Idle）且无
// 订阅者时 runner 回收 looper，避免无人观看的停靠任务占用资源。

use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use futures::stream::Stream;
use model_provider::InputItem;
use peco_core::agent::{AgentLooper, LooperEvent, LooperHandle, OuterState, strip_summary_wrapper};
use peco_core::knowledge::{KnowledgeManager, KnowledgeModuleError};
use peco_core::persistence::SessionPersister;
use peco_core::session::{Session, SessionSnapshot};
use peco_core::tools::MemoryAuditEntry;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, broadcast, mpsc};
use tracing::{info, warn};

use crate::auth::AuthUser;
use crate::chat::sse::{ChatSseEvent, UsageData, map_looper_event};
use crate::error::ApiError;
use crate::peco::active::{
    CancelWaitResult, ControlCommand, EnqueueOutcome, RunGuard, RunRegistration,
};
use crate::session_dto::{TurnData, turns_to_dto};
use crate::session_store::{SqliteSessionPersister, hydrate_inflight_turn};
use crate::state::AppState;

use super::filter::PecoContextFilter;
use super::manager::PecoManager;
use super::memory::{degraded_count, last_converged_at, view};
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

/// 会话快照分页查询参数。
///
/// `turns` 缺省 = 全量（向后兼容旧客户端）；取值 clamp 到 `1..=200`。
/// `before` 缺省 = 尾部；语义为排他上界（只返回 `turn_index < before` 的轮）。
/// `turn_index` 保持全局位置号（非窗口内相对位置），前端据此做 turn 段级合并。
#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    #[serde(default)]
    pub turns: Option<usize>,
    #[serde(default)]
    pub before: Option<usize>,
}

/// `turns` 分页大小的合法上界。
const MAX_PAGE_TURNS: usize = 200;

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
    /// 是否有活跃 run（含停靠在 Idle 等下一句输入的 run）。
    pub is_running: bool,
    /// 是否有轮次在途 —— 前端据此决定是否附着并补占位气泡。
    ///
    /// 与 `is_running` 的区别只在停靠态：run 已注册但当前无轮次在跑时
    /// 该字段为 false，前端不应附着（否则会挂出一条永不填充的空占位）。
    pub turn_in_flight: bool,
    /// 在途轮的用户输入文本（无在途轮时缺省）。
    ///
    /// 快照只含已 committed 的轮，在途轮不在其中。整页刷新后前端内存全清，仅凭
    /// `turn_in_flight` 只能补一个空占位 —— 用户刚发出的 query 会消失到该轮落盘
    /// 为止。下发本字段让前端把它渲染回列表尾部。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inflight_user_input: Option<String>,
    /// 钉扎的历史摘要（compaction 产物）。无压缩历史时缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_summary: Option<String>,
    /// 上下文指标。会话不存在时缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_metrics: Option<ContextMetrics>,
    /// 当前快照总轮数（压缩后会变）。
    pub total_turns: usize,
    /// 本窗口之前是否还有更早的轮（翻页游标判定）。
    pub has_more: bool,
}

/// 上下文指标轻量响应（`/session/metrics`）。
#[derive(Debug, Serialize)]
pub struct SessionMetricsResponse {
    /// 上下文指标。会话不存在时为 null（字段恒在，不 skip）。
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
            match message
                .as_ref()
                // 先 subscribe 再 enqueue：持有一个 receiver 后 run 不可能被
                // 回收（try_reclaim 要求订阅者数为 0），消息必然送达。
                .map(|text| state.peco_runs.enqueue_query(&user_id, text.clone()))
            {
                // 纯附着，或消息已入队 → 桥接事件流
                None | Some(EnqueueOutcome::Enqueued) => {
                    return Ok(bridge_sse_response(Arc::clone(&state), user_id.clone(), rx));
                }
                Some(EnqueueOutcome::Backpressure) => {
                    return Err(ApiError::Conflict(
                        "peco control queue full（待处理消息积压）".into(),
                    ));
                }
                // 订阅到的是正在收尾的 run：等它退出后重试（新建或附着到新 run）
                Some(EnqueueOutcome::NoRun) => {
                    state
                        .peco_runs
                        .wait_until_absent(&user_id, RUN_EXIT_WAIT)
                        .await;
                    continue;
                }
            }
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
    let mut session: Box<Session> = match persister.load(&session_id).await {
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

    // 崩溃恢复：冻结上次进程留下的在途轮（无检查点时零开销）。
    hydrate_inflight_turn(&persister, &mut session).await;

    Ok(session)
}

/// 构建并启动一个 Peco run：PecoManager → Session → Looper → runner 任务。
///
/// 构建失败时 `registration` 在本函数帧内 drop → guard 清理注册表；
/// 成功时注册产物整体移入 runner 任务，随其退出而清理。
pub(crate) async fn spawn_peco_run(
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
/// - looper 事件 → 广播；`Shutdown` 结束；`OuterStateChange → Idle` 且无订阅者
///   → 回收退出
/// - 控制命令 → `Query` 排队消息 / `Cancel` 取消在途轮次（不 break，继续转发
///   收尾事件让附着端看到 error）
/// - 回收唤醒（桥接退出时 `request_reclaim`）→ looper 停靠且无订阅者 → 回收
///
/// **取消不再结束 run**：looper 收尾后停在 Idle，事件流继续，「无订阅者」成了回收的
/// 唯一触发器 —— 客户端断连后 `request_reclaim` 才回收，附着中的 run 一直留着等下一
/// 条 query。
///
/// `to=Idle` 的回收窗口在续接轮之间也存在（一轮 Done 后先发 `Idle`，下一轮才由
/// looper 发出 `Idle→RunningInnerLoop`）：无订阅者时会把本该续接的排队轮一并回收 ——
/// 用户已离开，不替他跑是合理取舍。
///
/// 退出即 drop handle：user_speaker 消亡使停靠的 looper 在 `Idle` 上经 `recv()` 返回
/// `None` 优雅终止；`_guard` drop 清理注册表。
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
                    match &ev {
                        LooperEvent::OuterStateChange { to, .. } => {
                            looper_idle = matches!(to, OuterState::Idle);
                            // 停靠态 = 无轮次在途；附着方据此判断要不要补占位气泡
                            runs.set_turn_in_flight(&user_id, !looper_idle);
                            // 收尾即清空在途 query —— 与 turn_in_flight 同寿命
                            if looper_idle {
                                runs.set_inflight_input(&user_id, None);
                            }
                        }
                        // 在途轮的用户输入：供快照端点在整页刷新后恢复本轮 query
                        LooperEvent::TurnStart { user_input, .. } => {
                            runs.set_inflight_input(&user_id, Some(user_input.clone()));
                        }
                        _ => {}
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
                Some(ControlCommand::Cancel) => {
                    if let Err(e) = handle.cancel().await {
                        warn!(user_id = %user_id, error = %e, "Failed to deliver cancel");
                    }
                }
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
/// 无活跃 run 时 404。取消 = **中止当前 ReAct 轮，looper 不退出**：在下个步进边界
/// （流式/工具 ≤200ms、batch = 整段生成）收尾成 `TurnComplete{Failed{Cancelled}}` →
/// SSE `error`，随后停在 Idle 等输入。**不再有 `Shutdown` / `done`** —— 连接保持
/// 打开，下一条 query 直接开启新一轮。
///
/// ⚠ 已知偏差：web 前端 `abortStream()` 在本请求后立即关闭 SSE 连接，run 随即被
/// runner 回收 —— 「取消后不退出 looper」在 web 主路径不成立，只对 CLI / 长连接有效
/// （详见 CLAUDE.md）。
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

/// `POST /api/peco/stream/query` 的请求体。
#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    pub message: String,
}

/// POST /api/peco/stream/query — 向已注册的 run 排队一条用户消息。
///
/// 客户端常驻一条 SSE 连接后，后续消息经本端点投递，不再新开连接
/// （新连接会让同一批 broadcast 事件被重复消费）。无 run 时 404，
/// 调用方应改走 `GET /stream?message=`（附着或新建，语义自洽）。
pub async fn query_stream(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    body: Result<Json<QueryRequest>, JsonRejection>,
) -> Result<Json<SuccessResponse>, ApiError> {
    // 显式归一：缺字段 / 类型不符 / 非法 JSON / Content-Type 缺失一律 400
    let Json(req) = body.map_err(|e| ApiError::BadRequest(format!("请求体不合法：{e}")))?;
    let text = req.message.trim().to_string();
    if text.is_empty() {
        return Err(ApiError::BadRequest("message is required".into()));
    }

    match state.peco_runs.enqueue_query(&user_id, text) {
        EnqueueOutcome::Enqueued => {
            info!(user_id = %user_id, "Peco query enqueued into active run");
            Ok(Json(SuccessResponse {
                success: true,
                message: None,
            }))
        }
        EnqueueOutcome::NoRun => Err(ApiError::NotFound("no active peco run".into())),
        EnqueueOutcome::Backpressure => Err(ApiError::Conflict(
            "peco control queue full（待处理消息积压）".into(),
        )),
    }
}

// ── Handler: GET /api/peco/session ──────────────────────────────────────

/// 计算会话上下文指标（`/session` 与 `/session/metrics` 共用）。
///
/// 预算阈值取默认配置 — GET /session 不构建 PecoManager（无模板安装等重
/// 副作用），阈值实际为常量，口径注释见 PecoConfig。
async fn compute_context_metrics(
    state: &AppState,
    user_id: &str,
    session_id: &str,
    snap: &SessionSnapshot,
) -> ContextMetrics {
    let peco_config = super::config::PecoConfig::default();
    let est = super::metrics::estimate_session_context(snap, peco_config.history_token_budget);
    let compactions =
        crate::db::compaction_log::list_by_conversation(&state.db, user_id, session_id)
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
    ContextMetrics {
        estimated_total_tokens: est.total_tokens,
        estimated_view_tokens: est.view_tokens,
        pinned_summary_tokens: est.pinned_tokens,
        history_token_budget: peco_config.history_token_budget,
        compaction_trigger_tokens: peco_config.compaction_trigger_tokens,
        compaction_count,
        compactions,
    }
}

/// 获取 Peco 永续会话快照（分 turn 分页）。
///
/// `?turns=<N>&before=<turn_index>` 只返回 `[before-N, before)` 的轮窗口，
/// `turn_index` 保持全局位置号；`turns` 缺省返回全量历史（向后兼容）。
pub async fn get_session_snapshot(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<SessionQuery>,
) -> Result<Json<SessionSnapshotResponse>, ApiError> {
    let session_id = private_session_id(&user_id);
    let persister = SqliteSessionPersister::new(state.db.clone());

    let snapshot_opt = persister
        .load(&session_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to load session: {e}")))?;

    let (turns, usage, pinned_summary, context_metrics, total_turns, has_more) = match snapshot_opt
    {
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

            // 窗口：[start, end)。end 收 `before` 或总轮数；n 收 `turns`
            // 或全量（显式值才 clamp 到 1..=200，缺省全量不受 clamp 影响）。
            let total = snap.committed_turns.len();
            let end = params.before.unwrap_or(total).min(total);
            let n = params
                .turns
                .map(|t| t.clamp(1, MAX_PAGE_TURNS))
                .unwrap_or(usize::MAX);
            let start = end.saturating_sub(n);
            let has_more = start > 0;
            // 切片在 enumerate 之后，turn_index 保持全局位置号。
            let turns = turns_to_dto(&snap.committed_turns, start..end);

            let usage = UsageData {
                input_tokens: snap.total_usage.input_tokens,
                output_tokens: snap.total_usage.output_tokens,
            };

            let context_metrics =
                compute_context_metrics(&state, &user_id, &session_id, &snap).await;

            (
                turns,
                usage,
                pinned_summary,
                Some(context_metrics),
                total,
                has_more,
            )
        }
        None => (
            Vec::new(),
            UsageData {
                input_tokens: 0,
                output_tokens: 0,
            },
            None,
            None,
            0,
            false,
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
        turn_in_flight: state.peco_runs.turn_in_flight(&user_id),
        inflight_user_input: state.peco_runs.inflight_input(&user_id),
        pinned_summary,
        context_metrics,
        total_turns,
        has_more,
    }))
}

// ── Handler: GET /api/peco/session/metrics ──────────────────────────────

/// 上下文指标轻量端点。
///
/// 只返回 `context_metrics`，不序列化 turn 历史 —— 供 ContextMetricsCard
/// 等只关心指标、不关心正文的调用方使用。会话不存在时返回 `{ context_metrics: null }`。
pub async fn get_session_metrics(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<SessionMetricsResponse>, ApiError> {
    let session_id = private_session_id(&user_id);
    let persister = SqliteSessionPersister::new(state.db.clone());

    let snapshot_opt = persister
        .load(&session_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to load session: {e}")))?;

    let context_metrics = match snapshot_opt {
        Some((snap, _meta)) => {
            Some(compute_context_metrics(&state, &user_id, &session_id, &snap).await)
        }
        None => None,
    };

    Ok(Json(SessionMetricsResponse { context_metrics }))
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
            // 兜底上限：取消后在途模型调用返回即收尾，正常不会更久。
            if runs
                .wait_until_absent(&uid, Duration::from_secs(2 * 60 * 60))
                .await
            {
                let persister = SqliteSessionPersister::new(db);
                if let Err(e) = persister.delete(&sid).await {
                    warn!(user_id = %uid, error = %e, "Follow-up snapshot delete after late run exit failed");
                }
                let _ = persister.delete_inflight(&sid).await;
            }
        });
    }

    let persister = SqliteSessionPersister::new(state.db.clone());

    // ── 1. 清理在途轮检查点 ─────────────────────────────────────────────
    // 检查点不是历史（从不归档），必须随快照一并清除 —— peco 的 session_id
    // 由 user_id 派生、清空后复用，残留行会被水化进新会话。
    // 放在最前：后面有「会话本就为空」的提前返回，不能漏。
    if let Err(e) = persister.delete_inflight(&session_id).await {
        warn!(session_id = %session_id, error = %e, "Failed to clear inflight turn checkpoint");
    }

    // ── 1b. 清理压缩日志（先于快照删除 — 失败时快照未动，可安全重试）─────
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
    /// 按删除原因过滤（历史 tab：`?reason=superseded`）。缺省 / 空串不过滤。
    #[serde(default)]
    pub reason: Option<String>,
}

fn default_audit_limit() -> i64 {
    20
}

/// 单条记忆删除审计记录（含被删原文 — 仅供本人查阅）。
///
/// `topic_key` / `successor_doc_id` 仅 `reason='superseded'` 行有值；
/// `successor_title` / `retention_days_remaining` 需查 KB / 取当前时刻，
/// 由 handler 在 `From` 之后逐条后填（仅 superseded 行）。
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
    /// 取代槽键（仅 superseded 行）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic_key: Option<String>,
    /// 后继 doc id（仅 superseded 行）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub successor_doc_id: Option<String>,
    /// 后继标题 — §11 两路解析（① 存活活条标题 ② 审计面最新 done 行标题）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub successor_title: Option<String>,
    /// superseded 保留期剩余天数（30d 档，下限 0）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_days_remaining: Option<i64>,
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
            topic_key: r.topic_key,
            successor_doc_id: r.successor_doc_id,
            successor_title: None,
            retention_days_remaining: None,
        }
    }
}

/// 分页列出当前用户的记忆删除审计（deleted_at 倒序，可按 `reason` 过滤）。
///
/// superseded 行额外后填后继标题（两路解析）与剩余保留天数 —
/// 两者需查 KB / 取 now，不能在 `From` 里同步算。workspace 打不开 /
/// KB 读失败只降级跳过对应展示字段，不阻断列表（I4 非致命）。
pub async fn list_memory_audit(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<AuditQuery>,
) -> Result<Json<Vec<MemoryAuditItem>>, ApiError> {
    let reason = params.reason.as_deref().filter(|r| !r.is_empty());
    let rows = crate::db::memory_audit::list_by_user_reason(
        &state.db,
        &user_id,
        reason,
        params.limit.clamp(1, 100),
        params.offset.max(0),
    )
    .await
    .map_err(|e| ApiError::Internal(format!("failed to list memory audit: {e}")))?;

    let memory = super::config::PecoConfig::default().memory;
    // 后继标题解析①（活条）要读 KB —— 只在确有带后继的 superseded 行时开 workspace
    let needs_successor_lookup = rows
        .iter()
        .any(|r| r.reason == "superseded" && r.successor_doc_id.is_some());
    let ws = if needs_successor_lookup {
        match state
            .workspace_manager
            .get_synced(&user_id, &state.db)
            .await
        {
            Ok(ws) => Some(ws),
            Err(e) => {
                warn!(error = %e, "Audit list: workspace unavailable, successor titles degrade");
                None
            }
        }
    } else {
        None
    };
    let km: Option<&KnowledgeManager> = ws.as_ref().map(|w| &**w.knowledge_manager());

    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let mut item = MemoryAuditItem::from(row);
        if item.reason == "superseded" {
            item.retention_days_remaining =
                retention_days_remaining(&item.deleted_at, memory.superseded_retention_days);
            if let Some(successor) = item.successor_doc_id.as_deref() {
                item.successor_title =
                    resolve_successor_title(&state.db, &user_id, km, &item.kb_name, successor)
                        .await;
            }
        }
        items.push(item);
    }

    Ok(Json(items))
}

/// §11 后继标题两路解析：① 后继仍存活 → 活条标题；② 已再次退役 → 该
/// doc_id 在审计面最新 `status='done'` 行的标题（先滤 done，防 cancelled
/// 重复行顶替 — design nit10）。展示字段：任一路失败降级，不阻断列表。
async fn resolve_successor_title(
    db: &sqlx::SqlitePool,
    user_id: &str,
    km: Option<&KnowledgeManager>,
    kb_name: &str,
    successor_doc_id: &str,
) -> Option<String> {
    if let Some(km) = km {
        match km.get_document(kb_name, successor_doc_id).await {
            Ok(Some(doc)) => return Some(doc.title),
            Ok(None) => {}
            Err(e) => warn!(
                error = %e,
                doc_id = successor_doc_id,
                "Successor live lookup failed, falling back to audit title"
            ),
        }
    }
    match crate::db::memory_audit::latest_done_row_title(db, user_id, kb_name, successor_doc_id)
        .await
    {
        Ok(title) => title,
        Err(e) => {
            warn!(error = %e, doc_id = successor_doc_id, "Successor audit title lookup failed");
            None
        }
    }
}

/// superseded 保留期剩余天数 = 档期 − 已过天数，下限 0（§11 / §6.6）。
/// `deleted_at` 解析失败返回 None（不冒充 0）。
fn retention_days_remaining(deleted_at: &str, retention_days: u64) -> Option<i64> {
    let deleted = chrono::DateTime::parse_from_rfc3339(deleted_at).ok()?;
    let elapsed = (chrono::Utc::now() - deleted.with_timezone(&chrono::Utc)).num_days();
    Some((retention_days as i64 - elapsed).max(0))
}

/// 回滚响应。
#[derive(Debug, Serialize)]
pub struct RestoreMemoryResponse {
    pub success: bool,
    pub doc_id: String,
    pub restored_at: String,
}

/// 按审计行回滚一条记忆删除，成功后回填 restored_at / restored_doc_id：
/// - `source == "graph_fact"`（图事实删除）→ 解析边快照，`read_fact` 存在性门后
///   重放 `add_facts`（事实仍在则跳过，不产生并行边）；实体级联行与损坏快照 409 拒绝。
///   分支原样保留，**不做内容哈希断言**（§7.4 —— graph_fact 的 doc_id 不是内容哈希）。
/// - 其余（文档删除）→ 三阶段回滚契约（§7）：① 全量校验零副作用 →
///   ② 沿 successor 链定位存活叶子（对环免疫 + 自身守门 + 跨槽 409）→
///   ③ 重放自身 → 退役叶子 → 回填。
///
/// 取代行只重放不退役会让旧条与当前叶子两活（违反 I1）—— 叶子退役失败时
/// 两活留存且 `restored_at` 仍为 NULL，**可再次发起 restore 收敛**
/// （restore 路径无 intent，不经对账 —— §7.3 明文契约）。
///
/// 他人审计行与不存在的行一律 404 — 不泄露记录的存在性。
pub async fn restore_memory_audit(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<RestoreMemoryResponse>, ApiError> {
    // ── 阶段1（共享）：行存在 / 归属 / 状态 / 未回滚 —— 全部零副作用 ──
    let row = crate::db::memory_audit::get(&state.db, id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to load memory audit: {e}")))?
        .filter(|r| r.user_id == user_id)
        .ok_or_else(|| ApiError::NotFound(format!("memory audit record #{id} not found")))?;

    // 仅 done 且未回滚的行可回滚
    // （pending 是未决的删除流程；cancelled 表示删除未发生，无需回滚）
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

    // ── 分流：graph_fact 原样走图路径（§7.4）；文本行走 §7 三阶段契约 ──
    // 图行不得进 add_text 路径 —— 把快照 JSON 当文档灌进 KB 是污染（假失败 + 垃圾文档）。
    let is_graph_fact = row.source == "graph_fact";
    let restored_doc_id = if is_graph_fact {
        let (subject, predicate, object, weight) =
            parse_graph_fact_snapshot(id, &row.doc_id, &row.content)?;

        let ws = state
            .workspace_manager
            .get_synced(&user_id, &state.db)
            .await?;
        // 存在性门：事实已在（并发重建 / 人工重放 / 删↔回滚循环）→ 跳过重放，
        // 每次 restore 对图的净效果 ∈ {0, 1 条边}，杜绝并行边累积。
        let existing = ws
            .knowledge_manager()
            .read_fact(&row.kb_name, &subject, &predicate, &object)
            .await
            .map_err(|e| match &e {
                KnowledgeModuleError::NotFound(name) => {
                    ApiError::NotFound(format!("知识库 '{name}' 不存在，无法回滚审计行 #{id}"))
                }
                other => ApiError::Internal(format!("failed to read fact: {other}")),
            })?;
        if existing.is_empty() {
            ws.knowledge_manager()
                .add_facts_to_kb(
                    &row.kb_name,
                    &[knowledge_base::Fact::new(
                        subject, predicate, object, weight,
                    )],
                    true,
                )
                .await
                .map_err(|e| match &e {
                    KnowledgeModuleError::NotFound(name) => {
                        ApiError::NotFound(format!("知识库 '{name}' 不存在，无法回滚审计行 #{id}"))
                    }
                    other => ApiError::Internal(format!("failed to replay add_facts: {other}")),
                })?;
        }
        // doc_id 由 parse_graph_fact_snapshot 保证与快照三元组一致（fact:xxx），直接复原
        row.doc_id.clone()
    } else {
        restore_text_row(&state, &user_id, &row).await?
    };

    // ── 回填 restored_at / restored_doc_id（CAS：仅 done 且未回滚）──
    let restored_at = chrono::Utc::now().to_rfc3339();
    let updated =
        crate::db::memory_audit::mark_restored(&state.db, id, &restored_at, &restored_doc_id)
            .await
            .map_err(|e| ApiError::Internal(format!("failed to mark audit restored: {e}")))?;
    if updated == 0 {
        return Err(ApiError::Conflict(format!(
            "审计行 #{id} 状态已变化，本次回滚未记录"
        )));
    }

    info!(
        user_id = %user_id,
        audit_id = id,
        doc_id = %restored_doc_id,
        kb = %row.kb_name,
        graph = is_graph_fact,
        "Memory deletion restored from audit"
    );

    Ok(Json(RestoreMemoryResponse {
        success: true,
        doc_id: restored_doc_id,
        restored_at,
    }))
}

/// 文本行的 §7 三阶段回滚，返回重放后的 doc_id。
///
/// 行存在 / 归属 / 状态 / 未回滚已在调用方校验；本函数负责 KB 校验、
/// 叶子定位与写入序（replay → retire → 由调用方回填）。
async fn restore_text_row(
    state: &Arc<AppState>,
    user_id: &str,
    row: &crate::db::memory_audit::MemoryAuditRow,
) -> Result<String, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(user_id, &state.db)
        .await?;
    let km: &KnowledgeManager = ws.knowledge_manager();

    // ── 阶段1 续：KB 存在（读探测零副作用；文档在/不在均合法）──
    match km.get_document(&row.kb_name, &row.doc_id).await {
        Ok(_) => {}
        Err(KnowledgeModuleError::NotFound(name)) => {
            return Err(ApiError::NotFound(format!(
                "知识库 '{name}' 不存在，无法回滚审计行 #{}",
                row.id
            )));
        }
        Err(e) => {
            return Err(ApiError::Internal(format!(
                "failed to verify knowledge base: {e}"
            )));
        }
    }

    // ── 阶段2：定位叶子（§7.1，可能 ABORT，仍零副作用）──
    let max_hops = super::config::PecoConfig::default()
        .memory
        .restore_walk_max_hops;
    let leaf = locate_restore_leaf(&state.db, km, row, max_hops).await?;

    // ── 阶段3：replay(R) —— 失败即中止，叶子未动（拒绝零副作用）──
    let doc = km
        .add_text_to_kb(&row.kb_name, &row.title, &row.content, &row.source)
        .await
        .map_err(|e| match &e {
            KnowledgeModuleError::NotFound(name) => ApiError::NotFound(format!(
                "知识库 '{name}' 不存在，无法回滚审计行 #{}",
                row.id
            )),
            other => ApiError::Internal(format!("failed to replay add_text: {other}")),
        })?;
    if doc.id != row.doc_id {
        return Err(ApiError::Conflict(format!(
            "回滚后的文档 id '{}' 与审计行 doc_id '{}' 不一致（内容应逐字节一致）",
            doc.id, row.doc_id
        )));
    }
    let restored_doc_id = doc.id;

    // 退役当前叶子（链上还有存活后继时）：successor 指回 R（§7.3 nit7）。
    // 失败 → 两活、R.restored_at 仍 NULL —— 返回 5xx，可再次发起 restore 收敛
    // （§7.3：不经对账，不计入有界收敛）。
    if let Some(leaf_doc) = leaf {
        let entry = MemoryAuditEntry {
            user_id: user_id.to_string(),
            kb_name: row.kb_name.clone(),
            doc_id: leaf_doc.id.clone(),
            title: leaf_doc.title,
            content: leaf_doc.content,
            source: leaf_doc.source_path,
            reason: "superseded".to_string(),
            deleted_by: format!("restore:{}", row.id),
            deleted_at: chrono::Utc::now().to_rfc3339(),
            topic_key: row.topic_key.clone(),
            successor_doc_id: Some(row.doc_id.clone()),
        };
        if let Err(e) = super::memory::retire::delete_with_audit(&state.db, km, &entry).await {
            return Err(ApiError::Internal(format!(
                "回滚后继退役失败（两活态，restored_at 未回填，可再次回滚收敛）: {e}"
            )));
        }
    }

    Ok(restored_doc_id)
}

/// §7.1 叶子定位：沿 successor 链找第一个存活者（即当前版本），对环免疫。
///
/// - `visited` 单调增长 + 首存活即断 + `max_hops` 上限 → 必终止（A↔B 环可终止）；
/// - 每跳经 §7.2 唯一选行取 successor，`topic_compatible` 不符 → 409 slot
///   mismatch（此时零副作用）；
/// - 链回到 R 自身且自身存活 → 自身守门返回 `None`（只重放不退役，不得
///   zero-alive —— V4 Blocker1）；
/// - `get_document` 的非 NotFound `Err` 是后端故障 ≠「已删」，中止上抛（M1）。
async fn locate_restore_leaf(
    db: &sqlx::SqlitePool,
    km: &KnowledgeManager,
    row: &crate::db::memory_audit::MemoryAuditRow,
    max_hops: usize,
) -> Result<Option<knowledge_base::Document>, ApiError> {
    let mut node = row.successor_doc_id.clone();
    let mut visited: HashSet<String> = HashSet::new();
    while let Some(cur) = node {
        if visited.len() >= max_hops {
            warn!(
                audit_id = row.id,
                chain_len = visited.len(),
                max_hops,
                "Restore successor walk hit hop limit"
            );
            break;
        }
        if !visited.insert(cur.clone()) {
            // 回到已访问节点 —— 环，visited 单调保证终止
            break;
        }
        match km.get_document(&row.kb_name, &cur).await {
            // 自身守门：链回到 R 且 R 已复活 → 只重放、不退役（不得 zero-alive）
            Ok(Some(doc)) if doc.id == row.doc_id => return Ok(None),
            Ok(Some(doc)) => return Ok(Some(doc)),
            Ok(None) => {}
            Err(KnowledgeModuleError::NotFound(name)) => {
                return Err(ApiError::NotFound(format!(
                    "知识库 '{name}' 不存在，无法回滚审计行 #{}",
                    row.id
                )));
            }
            Err(e) => {
                return Err(ApiError::Internal(format!(
                    "failed to walk successor chain: {e}"
                )));
            }
        }
        // §7.2 唯一选行：同 doc_id 的 done + 未回滚 superseded 行取最新一条
        let Some(next) =
            crate::db::memory_audit::latest_superseded_row(db, &row.user_id, &row.kb_name, &cur)
                .await
                .map_err(|e| ApiError::Internal(format!("failed to pick successor row: {e}")))?
        else {
            break;
        };
        if !topic_compatible(row.topic_key.as_deref(), next.topic_key.as_deref()) {
            return Err(ApiError::Conflict(format!(
                "slot mismatch：审计行 #{} 与后继链上的审计行 #{} 不在同一事实槽，回滚中止（零副作用）",
                row.id, next.id
            )));
        }
        node = next.successor_doc_id;
    }
    Ok(None)
}

/// §7.4 topic 一致性守卫：回滚方 topic 不可判（None）放行，否则必须与跳点相等。
fn topic_compatible(r_topic: Option<&str>, hop_topic: Option<&str>) -> bool {
    r_topic.is_none() || r_topic == hop_topic
}

/// 解析 `source == "graph_fact"` 审计行的边快照，返回可重放的三元组与 weight。
///
/// 全部校验先于任何 KB 写入 —— 拒绝路径零副作用（409），审计原文原样保留可人工重放。
/// 写方 `fact_snapshot_json` 恒产出所需字段（读到空边的删除会先报错、根本不写行），
/// 因此严格校验不会误伤合法行；doc_id 一致性是廉价的防篡改断言。
fn parse_graph_fact_snapshot(
    id: i64,
    doc_id: &str,
    content: &str,
) -> Result<(String, String, String, f32), ApiError> {
    // 1. 实体级联行只拒绝不重放：快照的边只存端点哈希 id、无对端名称，
    //    重放需要 id→name 解析（改 peco-core）或改写快照格式（历史行不兼容）。
    if doc_id.starts_with("entity:") {
        return Err(ApiError::Conflict(format!(
            "审计行 #{id} 是图实体级联删除，暂不支持自动回滚（快照仅存对端实体 id，无法解析名称）；审计原文保留，可人工重放"
        )));
    }

    let v: serde_json::Value = serde_json::from_str(content).map_err(|_| {
        ApiError::Conflict(format!(
            "审计行 #{id} 的 graph_fact 快照损坏：content 不是合法 JSON"
        ))
    })?;
    let obj = v.as_object().ok_or_else(|| {
        ApiError::Conflict(format!(
            "审计行 #{id} 的 graph_fact 快照损坏：content 不是 JSON object"
        ))
    })?;

    // 2-3. 三元组字段齐全
    let get_str = |k: &str| -> Result<String, ApiError> {
        obj.get(k)
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                ApiError::Conflict(format!(
                    "审计行 #{id} 的 graph_fact 快照损坏：缺字符串字段 '{k}'"
                ))
            })
    };
    let subject = get_str("subject")?;
    let predicate = get_str("predicate")?;
    let object = get_str("object")?;

    // 4. 可重放的 weight（快照恒含非空 edges[]，首条边的 weight 即重放置信度）
    let weight = obj
        .get("edges")
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("weight"))
        .and_then(|w| w.as_f64())
        .ok_or_else(|| {
            ApiError::Conflict(format!(
                "审计行 #{id} 的 graph_fact 快照损坏：缺可重放的 edges[0].weight"
            ))
        })? as f32;

    // 5. doc_id 与三元组一致（Fact::compute_id 与删除工具写入时同源）
    let expected = knowledge_base::Fact::compute_id(&subject, &predicate, &object);
    if expected != doc_id {
        return Err(ApiError::Conflict(format!(
            "审计行 #{id} 快照与 doc_id 不一致（快照三元组算得 '{expected}'，行内为 '{doc_id}'）"
        )));
    }

    Ok((subject, predicate, object, weight))
}

// ── Handler: POST /api/peco/memory/consolidate ─────────────────────────────

/// 手动触发整理的响应。
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ConsolidateResponse {
    /// 整理未开启（`memory.consolidation.enabled = false`）。
    Disabled { enabled: bool, message: String },
    /// 已受理，后台整理中。
    Accepted { status: String, user_id: String },
}

/// 手动触发一轮记忆自动整理（不经 agent 通道，直接调 ConsolidationWorker）。
///
/// 异步受理：一轮整理是分钟级（batch 200 + ≤20 次 Flash 调用），同步等待
/// 会占住连接并撞上游超时 —— 返回 202 后由后台任务跑完，进度与结果经
/// `GET /memory/consolidation/state` 观测。触发方式之一 — cron 触发与
/// 空闲判定见 `super::memory::cron`。
pub async fn consolidate_now(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Response, ApiError> {
    let memory = super::config::PecoConfig::default().memory;
    if !state.consolidation_enabled {
        return Ok(Json(ConsolidateResponse::Disabled {
            enabled: false,
            message: "自动整理未开启（memory.consolidation.enabled = false）".into(),
        })
        .into_response());
    }

    // 用户主动发起即同意，不读 opt-in 开关（与 cron 通道语义分离）
    let state_bg = Arc::clone(&state);
    let user_id_bg = user_id.clone();
    tokio::spawn(async move {
        match super::memory::build_worker(&state_bg, &user_id_bg, &memory).await {
            Ok(worker) => match worker.run_once(&user_id_bg).await {
                Ok(stats) => info!(
                    user_id = %user_id_bg,
                    scanned = stats.scanned,
                    merged = stats.merged,
                    dedup_deleted = stats.dedup_deleted,
                    ttl_deleted = stats.ttl_deleted,
                    llm_calls = stats.llm_calls,
                    "Manual consolidation round finished"
                ),
                Err(e) => warn!(
                    user_id = %user_id_bg,
                    error = %e,
                    "Manual consolidation round failed"
                ),
            },
            Err(e) => warn!(
                user_id = %user_id_bg,
                error = %e,
                "Failed to assemble consolidation worker for manual trigger"
            ),
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(ConsolidateResponse::Accepted {
            status: "accepted".into(),
            user_id,
        }),
    )
        .into_response())
}

// ── Handler: GET /api/peco/memory/consolidation/state ──────────────────────

/// 自动整理观测响应：最近一次运行的时刻与统计。
#[derive(Debug, Serialize)]
pub struct ConsolidationStateResponse {
    /// 最近一次整理的完成时刻（RFC 3339）；从未整理过为 null。
    pub last_run_at: Option<String>,
    /// 最近一次整理的统计（落库 JSON）；从未整理过为 null。
    pub last_run_stats: Option<serde_json::Value>,
}

/// 查询当前用户的自动整理运行状态（前端观测口）。
///
/// 不受总开关门控 —— 关闭后仍需能看到关闭前的最后一次运行结果。
pub async fn get_consolidation_state(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<ConsolidationStateResponse>, ApiError> {
    let row = crate::db::memory_consolidation_state::get_state(&state.db, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to read consolidation state: {e}")))?;

    let (last_run_at, last_run_stats) = match row {
        Some(row) => (
            row.last_run_at,
            row.last_run_stats.map(|raw| {
                serde_json::from_str::<serde_json::Value>(&raw)
                    .unwrap_or(serde_json::Value::String(raw))
            }),
        ),
        None => (None, None),
    };

    Ok(Json(ConsolidationStateResponse {
        last_run_at,
        last_run_stats,
    }))
}

// ── Handler: GET/PUT /api/peco/memory/consolidation/optin ──────────────────

/// opt-in 开关的查询/写入响应。
#[derive(Debug, Serialize)]
pub struct OptinResponse {
    /// 当前用户是否已 opt-in 自动整理（无行 = false，fail-closed）。
    pub enabled: bool,
}

/// opt-in 写入请求体。
#[derive(Debug, Deserialize)]
pub struct SetOptinRequest {
    pub enabled: bool,
}

/// 查询当前用户的自动整理 opt-in 开关。
pub async fn get_memory_optin(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OptinResponse>, ApiError> {
    let enabled = crate::db::memory_consolidation_optin::is_opted_in(&state.db, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to read consolidation opt-in: {e}")))?;

    Ok(Json(OptinResponse { enabled }))
}

/// 写入当前用户的自动整理 opt-in 开关。
///
/// 存意愿不即时生效：服务器总开关（`memory.consolidation.enabled`）关闭时
/// 同样可写，避免放开灰度时前端再补交互；手动触发端点不读此开关
/// （用户主动发起即同意，两通道语义分离）。
pub async fn set_memory_optin(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    body: Result<Json<SetOptinRequest>, JsonRejection>,
) -> Result<Json<OptinResponse>, ApiError> {
    // 显式归一：缺字段 / 类型不符 / 非法 JSON / Content-Type 缺失一律 400
    let Json(req) = body.map_err(|e| ApiError::BadRequest(format!("请求体不合法：{e}")))?;

    crate::db::memory_consolidation_optin::set_enabled(&state.db, &user_id, req.enabled)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to write consolidation opt-in: {e}")))?;

    tracing::info!(user_id = %user_id, enabled = req.enabled, "Memory consolidation opt-in updated");

    Ok(Json(OptinResponse {
        enabled: req.enabled,
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

// ── Handler: GET /api/peco/memory/graph + GET /api/peco/memory/documents ──

/// 私人记忆库名 —— 与 `MemoryConfig.kb_name` 的生产默认值一致（`peco/memory/config.rs`）。
///
/// 就地定义而非提升为 `peco-core` 公共常量：为单个字符串引入跨 crate 依赖，收益
/// （省一处字面量）小于成本（design §3.3）。
const PRIVATE_MEMORY_KB: &str = "@private_memory";

/// 图谱端点查询参数。
#[derive(Debug, Deserialize)]
pub struct GraphQuery {
    /// 实体节点上限（默认 200，clamp 1..=1000）。
    #[serde(default = "default_node_limit")]
    pub node_limit: i64,
    /// 边上限（默认 500，clamp 1..=2000）。
    #[serde(default = "default_edge_limit")]
    pub edge_limit: i64,
}

fn default_node_limit() -> i64 {
    200
}

fn default_edge_limit() -> i64 {
    500
}

/// 文档列表端点查询参数。
#[derive(Debug, Deserialize)]
pub struct MemoryDocumentQuery {
    /// 偏移量（默认 0，负数按 0）。
    #[serde(default)]
    pub offset: i64,
    /// 页大小（默认 20，clamp 1..=100）。
    #[serde(default = "default_memory_doc_limit")]
    pub limit: i64,
    /// v2 新增：按 `source_path` **精确**过滤；缺省/空白 = 不过滤。
    pub source: Option<String>,
}

fn default_memory_doc_limit() -> i64 {
    20
}

/// 内容检索端点查询参数（E4）。
///
/// `q` 为 `Option<String>`：缺省能成功反序列化为 `None`，由 handler 主动判空返回
/// `400 BadRequest`（JSON），而非让 axum `Query` 反序列化失败的**纯文本 400**。
#[derive(Debug, Deserialize)]
pub struct MemorySearchQuery {
    /// 检索词（对**正文**子串、大小写不敏感）；缺失/空白 → 400。
    pub q: Option<String>,
    /// 返回条数上限（默认 20，clamp 1..=50）。
    ///
    /// **必须带 `#[serde(default = ..)]`**：否则 `?q=x` 省略 `limit` 会走 axum
    /// `QueryRejection` → 纯文本 400，拿不到默认值。
    #[serde(default = "default_memory_search_limit")]
    pub limit: i64,
    /// 按 `source_path` **精确**过滤；缺省/空白 = 不过滤（语义同 E2）。
    pub source: Option<String>,
}

fn default_memory_search_limit() -> i64 {
    20
}

/// 检索条数 clamp 到 `1..=50`（比 E2 的 100 更小：每条命中含一段 snippet，限响应体积）。
fn clamp_search_limit(limit: i64) -> usize {
    limit.clamp(1, 50) as usize
}

/// 把 KB 定位失败映射到 [`ApiError`]：`NotFound` → 404，其余 → 500。
///
/// 无 `From<KnowledgeModuleError> for ApiError`，逐处 `.map_err`（design §3.3）。
/// `op` 是 500 details 的操作前缀（`"failed to load memory graph"` /
/// `"failed to load memory documents"`）。
fn map_memory_kb_error(op: &str, e: KnowledgeModuleError) -> ApiError {
    match e {
        KnowledgeModuleError::NotFound(name) => {
            ApiError::NotFound(format!("知识库 '{name}' 不存在"))
        }
        other => ApiError::Internal(format!("{op}: {other}")),
    }
}

/// `GET /api/peco/memory/graph` —— 当前用户 `@private_memory` 的实体子图。
///
/// **隔离缺口（design §3.4）**：HelixDB 单一命名空间 + `Entity` 无 owner 字段 ⇒
/// 多用户部署下会返回该实例内所有用户写入的 Entity / 边；本轮接受现状、不加过滤。
pub async fn get_memory_graph(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<GraphQuery>,
) -> Result<Json<view::MemoryGraphResponse>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let (nodes, edges) = ws
        .knowledge_manager()
        .list_entity_graph(PRIVATE_MEMORY_KB)
        .await
        .map_err(|e| map_memory_kb_error("failed to load memory graph", e))?;

    Ok(Json(view::build_graph(
        nodes,
        edges,
        params.node_limit.clamp(1, 1000) as usize,
        params.edge_limit.clamp(1, 2000) as usize,
    )))
}

/// `GET /api/peco/memory/documents` —— 当前用户 `@private_memory` 的文档列表（分页）。
///
/// **隔离缺口（design §3.4）**：同 `get_memory_graph` —— `Document` 不写 `kb_id`，
/// 多用户部署下会返回该实例内所有 KB 的文档；本轮接受现状、不加过滤。
pub async fn list_memory_documents(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<MemoryDocumentQuery>,
) -> Result<Json<view::MemoryDocumentPage>, ApiError> {
    let offset = params.offset.max(0) as usize;
    let limit = params.limit.clamp(1, 100) as usize;
    // 空白 source 视作不过滤（防御性，与 agent 工具层「空前缀匹配全部」一致）。
    let source = params
        .source
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    // `limit+1` 探测 `has_more`：多取一条，由 build_document_page 截断。
    // 用 `list_memory_documents`（而非 agent 工具复用的 `list_documents`）——
    // 后者会把 `open_kb` 的**全部**失败收敛为 `NotFound`，本次修复让 E2 与 E1
    // 一致：只有 KB 真的不存在才 404，HelixDB 不可达等 → 500（design §3.3）。
    let rows = ws
        .knowledge_manager()
        .list_memory_documents(PRIVATE_MEMORY_KB, offset, limit + 1, source)
        .await
        .map_err(|e| map_memory_kb_error("failed to load memory documents", e))?;

    Ok(Json(view::build_document_page(rows, offset, limit)))
}

/// `GET /api/peco/memory/documents/{id}` —— 文档详情（E3）。
///
/// 复用既有 `KnowledgeManager::get_document`（core/kb 层零新增能力）。文档不存在
/// （`Ok(None)`）→ 404「文档不存在或已被删除」。
///
/// **隔离缺口（design §3.4）**：同 E2 —— 多用户部署下会返回该 HelixDB 实例内
/// 所有 KB 的文档；本轮接受现状、不加过滤。
pub async fn get_memory_document(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(doc_id): Path<String>,
) -> Result<Json<view::MemoryDocumentDetail>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let doc = ws
        .knowledge_manager()
        .get_document(PRIVATE_MEMORY_KB, &doc_id)
        .await
        .map_err(|e| map_memory_kb_error("failed to load memory document", e))?
        .ok_or_else(|| ApiError::NotFound("文档不存在或已被删除".into()))?;

    Ok(Json(view::build_document_detail(doc)))
}

/// `GET /api/peco/memory/search` —— 记忆内容检索（E4，正文子串扫描）。
///
/// 一次只读查询取回至多 `SCAN_LIMIT + 1` 条文档（含正文），Rust 侧做大小写不敏感
/// 子串匹配并生成 snippet。命中数超 `SCAN_LIMIT` ⇒ 500 + `warn!`（**不静默截断**）。
/// 检索语义（匹配/片段/排序）在 `view` 纯函数，core/kb 层只提供「取回全部文档」原语。
///
/// **隔离缺口（design §3.4）**：同 E1/E2 —— 不加 per-user / per-KB 过滤。
pub async fn search_memory_documents(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<MemorySearchQuery>,
) -> Result<Json<view::MemorySearchResponse>, ApiError> {
    let q = params.q.as_deref().map(str::trim).unwrap_or("");
    if q.is_empty() {
        return Err(ApiError::BadRequest("查询参数 q 不能为空".into()));
    }
    let limit = clamp_search_limit(params.limit);
    let source = params
        .source
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    // `SCAN_LIMIT + 1` 探测超限（同 E2 的 `limit+1` 范式）。
    let rows = ws
        .knowledge_manager()
        .list_memory_documents_with_content(PRIVATE_MEMORY_KB, view::SCAN_LIMIT + 1)
        .await
        .map_err(|e| map_memory_kb_error("failed to search memory documents", e))?;

    if rows.len() > view::SCAN_LIMIT {
        warn!(
            found = rows.len(),
            scan_limit = view::SCAN_LIMIT,
            "memory document scan exceeded SCAN_LIMIT"
        );
        return Err(ApiError::Internal(format!(
            "memory document scan exceeded SCAN_LIMIT={}",
            view::SCAN_LIMIT
        )));
    }

    Ok(Json(view::build_search_response(view::search_documents(
        rows, q, source, limit,
    ))))
}

/// `GET /api/peco/memory/supersede/health` —— 取代对账健康计数（design §11）。
///
/// `{pending, processing, failed}` 按用户从 intent 表聚合（done 是终态历史，
/// 不入响应）；`degraded` / `last_converged_at` 是进程级内存态 —— 前者为
/// ① `write_intent` 失败退化为 append 的累计次数（重启清零、不按用户隔离），
/// 后者为最近一次对账完整收口的时刻（SQL 级错误的轮次不更新）。
pub async fn supersede_health(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<SupersedeHealthResponse>, ApiError> {
    supersede_health_response(&state, &user_id).await
}

/// 组装 §11 健康响应 —— `GET /memory/supersede/health` 与手动对账端点共用。
async fn supersede_health_response(
    state: &AppState,
    user_id: &str,
) -> Result<Json<SupersedeHealthResponse>, ApiError> {
    let health = crate::db::memory_supersede::health_counts(&state.db, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to read supersede health: {e}")))?;

    Ok(Json(SupersedeHealthResponse {
        pending: health.pending,
        processing: health.processing,
        failed: health.failed,
        degraded: degraded_count(),
        last_converged_at: last_converged_at(),
    }))
}

/// `POST /api/peco/memory/supersede/reconcile` —— 手动触发一次对账（§6.5 ③）。
///
/// 与每轮 hook 收尾、进程启动同一条 `peco::memory::reconcile` 入口，幂等；
/// **不新增开关、不改变默认行为**（门恒闭）。同步跑完后返回与
/// `GET /memory/supersede/health` 同形的健康计数。
pub async fn supersede_reconcile(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<SupersedeHealthResponse>, ApiError> {
    let memory = super::config::PecoConfig::default().memory;
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    super::memory::reconcile(&state.db, ws.knowledge_manager(), &memory, &user_id).await;

    info!(user_id = %user_id, "Manual supersede reconcile finished");
    supersede_health_response(&state, &user_id).await
}

/// 取代对账健康响应（design §11）。
#[derive(Debug, Serialize)]
pub struct SupersedeHealthResponse {
    /// 待处理意图数。
    pub pending: i64,
    /// 处理中（已领取）意图数。
    pub processing: i64,
    /// 超重试上界已放弃的意图数。
    pub failed: i64,
    /// ① intent 写失败退化为 append 的次数（进程级，重启清零）。
    pub degraded: i64,
    /// 最近一次对账完整收口时刻（RFC 3339；从未收口则为 null）。
    pub last_converged_at: Option<String>,
}

/// 构建 Peco 路由。
///
/// 注册到 `/api/peco`：
/// - `GET /stream` — SSE 流式对话（无 message 时为纯附着模式）
/// - `POST /stream/query` — 向已注册的 run 排队一条消息（不新开连接）
/// - `POST /stream/cancel` — 取消进行中的任务
/// - `GET /session` — 获取会话快照（`?turns=&before=` 分 turn 分页）
/// - `GET /session/metrics` — 上下文指标轻量端点
/// - `DELETE /session` — 清除会话（默认先归档，`?archive=false` 硬删除）
/// - `GET /session/export` — 导出会话
/// - `GET /archives` — 归档列表
/// - `GET /archives/:id` — 下载归档
/// - `GET /memory/audit` — 记忆删除审计（分页，仅本人）
/// - `POST /memory/audit/:id/restore` — 按审计行回滚删除
/// - `POST /memory/consolidate` — 手动触发一轮自动整理（202 受理，后台执行）
/// - `GET /memory/consolidation/state` — 查询最近一次整理结果
/// - `GET /memory/consolidation/optin` — 查询自动整理 opt-in 开关
/// - `PUT /memory/consolidation/optin` — 写入自动整理 opt-in 开关
/// - `GET /memory/graph` — 记忆实体子图（节点 + 谓词边）
/// - `GET /memory/documents` — 记忆文档列表（分页，可按 `source` 过滤）
/// - `GET /memory/documents/{id}` — 记忆文档详情（全文 + 元数据）
/// - `GET /memory/search` — 记忆内容检索（正文子串扫描，`?q=&limit[&source]`）
/// - `GET /memory/supersede/health` — 取代对账健康计数（design §11）
/// - `POST /memory/supersede/reconcile` — 手动触发一次取代对账（§6.5 ③）
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/stream", get(stream_chat))
        .route("/stream/query", post(query_stream))
        .route("/stream/cancel", post(cancel_stream))
        .route("/session", get(get_session_snapshot).delete(clear_session))
        .route("/session/metrics", get(get_session_metrics))
        .route("/session/export", get(export_session))
        .route("/archives", get(list_archives))
        .route("/archives/{id}", get(download_archive))
        .route("/memory/audit", get(list_memory_audit))
        .route("/memory/audit/{id}/restore", post(restore_memory_audit))
        .route("/memory/consolidate", post(consolidate_now))
        .route("/memory/consolidation/state", get(get_consolidation_state))
        .route("/memory/consolidation/optin", get(get_memory_optin))
        .route("/memory/consolidation/optin", put(set_memory_optin))
        .route("/memory/graph", get(get_memory_graph))
        .route("/memory/documents", get(list_memory_documents))
        .route("/memory/documents/{id}", get(get_memory_document))
        .route("/memory/search", get(search_memory_documents))
        .route("/memory/supersede/health", get(supersede_health))
        .route("/memory/supersede/reconcile", post(supersede_reconcile))
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::StatusCode;

    async fn status_and_json(resp: Response) -> (StatusCode, serde_json::Value) {
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    /// E1 404：KB 不存在 ⇒ `NotFound` ⇒ 404 `{"error":"not_found", ...}`。
    #[tokio::test]
    async fn memory_graph_maps_kb_not_found_to_404() {
        let err = map_memory_kb_error(
            "failed to load memory graph",
            KnowledgeModuleError::NotFound(PRIVATE_MEMORY_KB.into()),
        );
        let (status, body) = status_and_json(err.into_response()).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");
        assert_eq!(body["details"], "知识库 '@private_memory' 不存在");
    }

    /// E1 500：其余 `KnowledgeModuleError` ⇒ `Internal` ⇒ 500 `{"error":"internal", ...}`。
    #[tokio::test]
    async fn memory_graph_maps_other_errors_to_500() {
        let err = map_memory_kb_error(
            "failed to load memory graph",
            KnowledgeModuleError::NotInitialized,
        );
        let (status, body) = status_and_json(err.into_response()).await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "internal");
        assert!(
            body["details"]
                .as_str()
                .unwrap()
                .starts_with("failed to load memory graph"),
            "500 details 应带操作前缀：{}",
            body["details"]
        );
    }

    /// E2 404 / 500：同一映射函数，`documents` 操作前缀不同。
    #[tokio::test]
    async fn memory_documents_maps_errors() {
        let not_found = map_memory_kb_error(
            "failed to load memory documents",
            KnowledgeModuleError::NotFound(PRIVATE_MEMORY_KB.into()),
        );
        let (status, body) = status_and_json(not_found.into_response()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");

        let internal = map_memory_kb_error(
            "failed to load memory documents",
            KnowledgeModuleError::NotInitialized,
        );
        let (status, body) = status_and_json(internal.into_response()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "internal");
        assert!(
            body["details"]
                .as_str()
                .unwrap()
                .starts_with("failed to load memory documents")
        );
    }

    /// 401：`AuthUser` 缺失/无效时抛的就是 `ApiError::Unauthorized` ⇒
    /// 401 `{"error":"unauthorized"}`（与 handler 的 extractor 同源）。
    #[tokio::test]
    async fn auth_failure_maps_to_401() {
        let err = ApiError::Unauthorized("missing authorization header".into());
        let (status, body) = status_and_json(err.into_response()).await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "unauthorized");
    }

    /// E4 `q` 空判定 → 400 `bad_request`（JSON，非 axum 纯文本）。
    #[tokio::test]
    async fn search_empty_q_maps_to_bad_request() {
        let err = ApiError::BadRequest("查询参数 q 不能为空".into());
        let (status, body) = status_and_json(err.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "bad_request");
        assert_eq!(body["details"], "查询参数 q 不能为空");
    }

    /// E4 handler 错误映射：`search` 操作前缀 + NotFound→404 / 其余→500。
    #[tokio::test]
    async fn memory_search_maps_errors() {
        let not_found = map_memory_kb_error(
            "failed to search memory documents",
            KnowledgeModuleError::NotFound(PRIVATE_MEMORY_KB.into()),
        );
        let (status, body) = status_and_json(not_found.into_response()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");

        let internal = map_memory_kb_error(
            "failed to search memory documents",
            KnowledgeModuleError::NotInitialized,
        );
        let (status, body) = status_and_json(internal.into_response()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "internal");
        assert!(
            body["details"]
                .as_str()
                .unwrap()
                .starts_with("failed to search memory documents")
        );
    }

    /// E4 clamp：区间 `1..=50`（999→50、0→1、负数→1、20→20）。
    #[test]
    fn search_limit_is_clamped_to_1_50() {
        assert_eq!(clamp_search_limit(999), 50);
        assert_eq!(clamp_search_limit(0), 1);
        assert_eq!(clamp_search_limit(-5), 1);
        assert_eq!(clamp_search_limit(20), 20);
        assert_eq!(clamp_search_limit(50), 50);
        assert_eq!(clamp_search_limit(51), 50);
    }

    /// E4 `MemorySearchQuery` 反序列化：省略 `limit` 仍得默认 20
    /// （缺 `#[serde(default = ..)]` 时反序列化会失败）。
    #[test]
    fn memory_search_query_defaults_limit() {
        use axum::http::Uri;

        let uri: Uri = "/memory/search?q=x".parse().unwrap();
        let q = Query::<MemorySearchQuery>::try_from_uri(&uri)
            .expect("省略 limit 必须能反序列化（默认 20）");
        assert_eq!(q.q.as_deref(), Some("x"));
        assert_eq!(q.limit, 20);
        assert_eq!(q.source, None);

        let uri: Uri = "/memory/search?q=x&limit=999&source=ppa_profile"
            .parse()
            .unwrap();
        let q = Query::<MemorySearchQuery>::try_from_uri(&uri).unwrap();
        assert_eq!(q.limit, 999, "解析值原样给出，再由 clamp_search_limit 收敛");
        assert_eq!(q.source.as_deref(), Some("ppa_profile"));
    }

    /// E2 `MemoryDocumentQuery`：`source` 可选（缺省 → None）。
    #[test]
    fn memory_document_query_source_is_optional() {
        use axum::http::Uri;

        let uri: Uri = "/memory/documents?offset=20&limit=10".parse().unwrap();
        let q = Query::<MemoryDocumentQuery>::try_from_uri(&uri).unwrap();
        assert_eq!(q.offset, 20);
        assert_eq!(q.limit, 10);
        assert_eq!(q.source, None);

        let uri: Uri = "/memory/documents?source=ppa_profile".parse().unwrap();
        let q = Query::<MemoryDocumentQuery>::try_from_uri(&uri).unwrap();
        assert_eq!(q.source.as_deref(), Some("ppa_profile"));
    }
}
