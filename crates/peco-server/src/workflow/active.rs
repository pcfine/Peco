// ============================================================================
// ActiveExecutions — 活跃 Workflow 执行注册表
// ============================================================================
//
// 事件转发架构（Phase 1 为 Phase 3 SSE 做准备）：
//
//   WorkflowHandle ──→ tokio task ──→ broadcast::Sender<WorkflowEvent>
//                                          │
//                     mpsc::Receiver         ├── Phase 3 SSE endpoint (subscribe)
//                     (control commands)     │
//
// 后台任务从 handle 消费事件，转发到 broadcast channel。
// Paused 状态时暂停广播，等待控制命令（approve/cancel）。
// 终端状态时清理注册表。

use std::collections::HashMap;
use std::sync::Mutex;

use peco_core::workflow::{ApprovalDecision, ApprovalResponse, WorkflowEvent, WorkflowHandle};
use tokio::sync::{broadcast, mpsc};

/// 控制命令。
enum ControlCommand {
    Cancel,
    Approve(ApprovalResponse),
}

/// 活跃句柄条目。
struct ActiveEntry {
    control_tx: mpsc::Sender<ControlCommand>,
    /// Broadcast sender for forwarding WorkflowEvents to subscribers (e.g., SSE endpoint).
    /// Phase 1: sender exists but no subscribers yet. Phase 3: SSE endpoint subscribes.
    event_tx: broadcast::Sender<WorkflowEvent>,
}

/// 活跃 Workflow 执行注册表。
static ACTIVE: std::sync::LazyLock<Mutex<HashMap<String, ActiveEntry>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// 取消指定执行。
pub async fn cancel_run(run_id: &str) {
    let entry = {
        let mut map = ACTIVE.lock().unwrap();
        map.remove(run_id)
    };
    if let Some(e) = entry {
        let _ = e.control_tx.send(ControlCommand::Cancel).await;
    }
}

/// 审批指定执行。
pub async fn approve_run(run_id: &str, decision: ApprovalDecision, note: Option<String>) {
    let entry = {
        let map = ACTIVE.lock().unwrap();
        map.get(run_id).map(|e| e.control_tx.clone())
    };
    if let Some(tx) = entry {
        let _ = tx
            .send(ControlCommand::Approve(ApprovalResponse { decision, note }))
            .await;
    }
}

/// 订阅指定执行的 WorkflowEvent 流。
///
/// 返回 broadcast Receiver，调用方可通过 `recv().await` 消费事件。
/// 若执行不存在或已完成，返回 None。
/// Phase 3 由 SSE endpoint 调用。
#[allow(dead_code)]
pub fn subscribe_events(run_id: &str) -> Option<broadcast::Receiver<WorkflowEvent>> {
    let map = ACTIVE.lock().unwrap();
    map.get(run_id).map(|entry| entry.event_tx.subscribe())
}

/// 注册一个运行中的 Workflow 执行，将 handle 所有权转移给后台 tokio 任务。
pub async fn insert_run(run_id: &str, mut handle: WorkflowHandle) {
    let (control_tx, mut control_rx) = mpsc::channel::<ControlCommand>(8);
    // 256 缓冲区 — 足以缓冲若干事件而不阻塞引擎
    let (event_tx, _event_rx) = broadcast::channel::<WorkflowEvent>(256);

    {
        let mut map = ACTIVE.lock().unwrap();
        map.insert(
            run_id.to_string(),
            ActiveEntry {
                control_tx: control_tx.clone(),
                event_tx: event_tx.clone(),
            },
        );
    }

    let rid = run_id.to_string();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                event = handle.recv_event() => {
                    let ev = match event {
                        None => break,
                        Some(ev) => ev,
                    };

                    // 转发事件到 broadcast（忽略无订阅者错误）
                    let _ = event_tx.send(ev.clone());

                    match &ev {
                        WorkflowEvent::Paused { .. } => {
                            // 等待审批或取消
                            match control_rx.recv().await {
                                Some(ControlCommand::Approve(response)) => {
                                    let _ = handle.approve(response).await;
                                }
                                Some(ControlCommand::Cancel) | None => {
                                    handle.cancel();
                                    break;
                                }
                            }
                        }
                        WorkflowEvent::Completed { .. }
                        | WorkflowEvent::Failed { .. }
                        | WorkflowEvent::Cancelled { .. } => {
                            break;
                        }
                        _ => {}
                    }
                }
                cmd = control_rx.recv() => {
                    match cmd {
                        Some(ControlCommand::Cancel) | None => {
                            handle.cancel();
                            break;
                        }
                        Some(ControlCommand::Approve(_)) => {
                            // 审批在非 Paused 状态下被忽略
                        }
                    }
                }
            }
        }

        // 清理注册表
        let mut map = ACTIVE.lock().unwrap();
        map.remove(&rid);
    });
}
