// ============================================================================
// Workflow SSE Stream Handler — 实时事件流端点
// ============================================================================
//
// `GET /api/workflows/executions/{run_id}/stream`
//
// 双层 channel 架构（参照 chat/handler.rs:stream_chat）：
//   broadcast (引擎 → 多订阅者) → mpsc (单订阅者 → SSE 响应)
// SSE 连接断开时不影响 broadcast 其他订阅者。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use peco_core::workflow::WorkflowEvent;
use tokio::sync::mpsc;

use crate::auth::AuthUser;
use crate::db;
use crate::error::ApiError;
use crate::state::AppState;
use tokio::sync::broadcast;

use super::WorkflowEventSource;
use super::sse::{self, WorkflowSseEvent};

pub async fn stream_execution(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    // ── 1. 验证执行记录归属 ──────────────────────────────────────────────
    let _row = db::workflow_executions::find_by_id_and_user(&state.db, &run_id, &user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("get execution: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("execution '{run_id}' not found")))?;

    // ── 2. 创建 SSE mpsc channel ─────────────────────────────────────────
    let (sse_tx, sse_rx) = mpsc::channel::<Result<Event, Infallible>>(256);

    // ── 3. 尝试订阅 broadcast（通过 trait，而非全局静态函数）─────────────
    if let Some(mut broadcast_rx) = state.subscribe_events(&run_id) {
        // 活跃执行 — 后台任务将 broadcast 事件转发到 SSE mpsc
        let rid = run_id.clone();
        tokio::spawn(async move {
            loop {
                match broadcast_rx.recv().await {
                    Ok(ev) => {
                        // 映射为 SSE 格式。StepDelta/StepRetrying 返回 None，跳过
                        let sse_ev = match sse::map_event(&ev) {
                            Some(e) => e,
                            None => continue,
                        };

                        let is_terminal = matches!(
                            ev,
                            WorkflowEvent::Completed { .. }
                                | WorkflowEvent::Failed { .. }
                                | WorkflowEvent::Cancelled { .. }
                        );

                        if let Ok(data) = sse_ev.to_sse_event()
                            && sse_tx.send(Ok(data)).await.is_err()
                        {
                            break; // SSE 客户端断开
                        }

                        if is_terminal {
                            // 终端事件后发送 Done，通知前端流结束
                            let done = WorkflowSseEvent::Done { run_id: rid };
                            if let Ok(data) = done.to_sse_event() {
                                let _ = sse_tx.send(Ok(data)).await;
                            }
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // 消费端落后 — 发送 error 让前端提示用户刷新
                        tracing::warn!(
                            run_id = %rid,
                            lagged = n,
                            "SSE subscriber lagged, notifying client"
                        );
                        let error_data = serde_json::json!({
                            "type": "error",
                            "runId": rid,
                            "message": format!(
                                "Event stream lagged by {n} messages, please refresh"
                            )
                        });
                        let _ = sse_tx
                            .send(Ok(Event::default().data(error_data.to_string())))
                            .await;
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    } else {
        // 非活跃执行 — 从 DB 快照构造终端事件后发送 Done，关闭流
        let rid = run_id.clone();
        let uid = user_id.clone();
        let db_for_bg = state.db.clone();
        tokio::spawn(async move {
            let terminal_event = if let Ok(Some(row)) =
                db::workflow_executions::find_by_id_and_user(&db_for_bg, &rid, &uid).await
            {
                match row.status.as_str() {
                    "completed" => Some(WorkflowSseEvent::Completed {
                        run_id: rid.clone(),
                        total_duration_ms: row.total_duration_ms.unwrap_or(0) as u64,
                        steps_completed: row.steps_completed as usize,
                        steps_failed: row.steps_failed as usize,
                        steps_skipped: row.steps_skipped as usize,
                    }),
                    "failed" => Some(WorkflowSseEvent::Failed {
                        run_id: rid.clone(),
                        error: row.error.unwrap_or_else(|| "Unknown error".into()),
                        failed_at_step: None,
                        total_duration_ms: row.total_duration_ms.unwrap_or(0) as u64,
                    }),
                    "cancelled" => Some(WorkflowSseEvent::Cancelled {
                        run_id: rid.clone(),
                    }),
                    _ => {
                        // running/paused — 不应出现（active 注册表已清理）
                        tracing::warn!(
                            run_id = %rid,
                            status = %row.status,
                            "Non-active execution has unexpected status"
                        );
                        None
                    }
                }
            } else {
                None
            };

            if let Some(ev) = terminal_event
                && let Ok(data) = ev.to_sse_event()
            {
                let _ = sse_tx.send(Ok(data)).await;
            }

            let done = WorkflowSseEvent::Done { run_id: rid };
            if let Ok(data) = done.to_sse_event() {
                let _ = sse_tx.send(Ok(data)).await;
            }
        });
    }

    // ── 4. 返回 SSE 流 ──────────────────────────────────────────────────
    let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}
