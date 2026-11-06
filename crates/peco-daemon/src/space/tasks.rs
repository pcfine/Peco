//! 跨空间任务的观察落点：`TaskObservationSink` 的 SQLite 实现。
//!
//! `DispatchWatchHook` 在派发成功后订阅目标空间的任务流，把注册 / 状态 /
//! 终态 / 失联翻译成本模块的四个回调。本模块只做两件事：写 `d_dispatches`
//! 台账，并发一条进程内广播事件（供后续 SSE 使用）。
//!
//! A+B 阶段不做唤醒：所有新行 `wake_state = 'skipped'`，避免未来的扫描器
//! 把「观察」误当成「待投递」。

use async_trait::async_trait;
use peco_core::agent::hooks::{DispatchObservation, TaskObservationSink};
use peco_core::machine::{ObsEvent, TaskSnapshot, UpdateBody};
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tracing::warn;

use crate::db::dispatches::{self, NewDispatch};
use crate::db::workspaces;

/// 终态摘要上限：只做 UI 的一行预览，不存整段输出。
const MAX_DIGEST_CHARS: usize = 512;
/// 广播缓冲：慢消费者只会收到 `Lagged`，不会阻塞 watcher。
const EVENT_BUFFER: usize = 256;

/// 台账变化通知（供后续 SSE 层消费）。
#[derive(Clone, Debug, Serialize)]
pub struct TaskEvent {
    /// `registered` / `state` / `terminal` / `orphaned`。
    pub kind: &'static str,
    pub task_id: String,
    /// 仅在 `registered` 时能直接确定；其余事件从台账查询。
    pub target_ws_id: Option<String>,
    pub state: String,
    pub dispatch_id: Option<String>,
}

/// `d_dispatches` 的写入端 + 进程内广播。
pub struct TaskObserver {
    pool: SqlitePool,
    events: broadcast::Sender<TaskEvent>,
}

impl TaskObserver {
    pub fn new(pool: SqlitePool) -> Self {
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        Self { pool, events }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.events.subscribe()
    }

    fn emit(&self, event: TaskEvent) {
        // 没有订阅者时 `send` 返回 Err：广播是可选能力，不影响落库。
        let _ = self.events.send(event);
    }
}

#[async_trait]
impl TaskObservationSink for TaskObserver {
    async fn on_registered(&self, obs: &DispatchObservation, snapshot: &TaskSnapshot) {
        let target_dir = obs.target_dir.to_string_lossy().into_owned();
        let row = match workspaces::by_work_dir(&self.pool, &target_dir).await {
            Ok(row) => row,
            Err(e) => {
                warn!(task_id = %obs.task_id, error = %e, "dispatch ledger lookup failed");
                return;
            }
        };
        let Some(row) = row else {
            warn!(
                task_id = %obs.task_id,
                target_dir,
                "dispatch target has no workspace row; ledger insert skipped"
            );
            return;
        };

        let state = snapshot.state.as_str();
        let id = format!("d_{}", obs.task_id);
        let inserted = dispatches::insert(
            &self.pool,
            &NewDispatch {
                id: &id,
                task_id: &obs.task_id.to_string(),
                target_ws_id: &row.id,
                target_work_dir: &row.work_dir,
                target_team_id: &row.team_id,
                origin_ws_id: &obs.origin.ws_id,
                agent_name: &obs.agent_name,
                origin_kind: &obs.origin_kind,
                state,
                // A+B 阶段不唤醒；写 skipped 而不是 pending。
                wake_state: "skipped",
            },
        )
        .await;
        match inserted {
            Ok(_) => self.emit(TaskEvent {
                kind: "registered",
                task_id: obs.task_id.to_string(),
                target_ws_id: Some(row.id),
                state: state.to_string(),
                dispatch_id: obs.dispatch_id.clone(),
            }),
            Err(e) => warn!(task_id = %obs.task_id, error = %e, "dispatch ledger insert failed"),
        }
    }

    async fn on_event(&self, obs: &DispatchObservation, event: &ObsEvent) {
        // 只关心状态变化；Progress/Output/Artifact 没有对应列，A+B 阶段不落库。
        let ObsEvent::Update(update) = event else {
            return;
        };
        let UpdateBody::Status { state, .. } = &update.body else {
            return;
        };
        let state_text = state.as_str();
        match dispatches::set_state(&self.pool, &obs.task_id.to_string(), state_text).await {
            Ok(_) => self.emit(TaskEvent {
                kind: "state",
                task_id: obs.task_id.to_string(),
                target_ws_id: None,
                state: state_text.to_string(),
                dispatch_id: obs.dispatch_id.clone(),
            }),
            Err(e) => {
                warn!(task_id = %obs.task_id, error = %e, "dispatch ledger state update failed")
            }
        }
    }

    async fn on_terminal(&self, obs: &DispatchObservation, snapshot: &TaskSnapshot) {
        let state = snapshot.state.as_str();
        let digest = result_digest(snapshot);
        match dispatches::finish(
            &self.pool,
            &obs.task_id.to_string(),
            state,
            digest.as_deref(),
        )
        .await
        {
            Ok(_) => self.emit(TaskEvent {
                kind: "terminal",
                task_id: obs.task_id.to_string(),
                target_ws_id: None,
                state: state.to_string(),
                dispatch_id: obs.dispatch_id.clone(),
            }),
            Err(e) => warn!(task_id = %obs.task_id, error = %e, "dispatch ledger finish failed"),
        }
    }

    async fn on_orphaned(&self, obs: &DispatchObservation, reason: &str) {
        match dispatches::orphan_by_task(&self.pool, &obs.task_id.to_string()).await {
            Ok(_) => {
                warn!(task_id = %obs.task_id, reason, "dispatch observation orphaned");
                self.emit(TaskEvent {
                    kind: "orphaned",
                    task_id: obs.task_id.to_string(),
                    target_ws_id: None,
                    state: "orphan".to_string(),
                    dispatch_id: obs.dispatch_id.clone(),
                });
            }
            Err(e) => {
                warn!(task_id = %obs.task_id, error = %e, "dispatch ledger orphan update failed")
            }
        }
    }
}

/// 终态输出摘要：只取文本载荷并截断到 [`MAX_DIGEST_CHARS`]，非文本为 `None`。
///
/// 启动对账补 `finish` 时也用它：两处各截一次会让「同一任务在两条路径上落库的
/// 摘要」长度不同。
pub(crate) fn result_digest(snapshot: &TaskSnapshot) -> Option<String> {
    let output = snapshot.outcome.as_ref()?.output.as_ref()?;
    let serde_json::Value::String(text) = &output.payload else {
        return None;
    };
    Some(text.chars().take(MAX_DIGEST_CHARS).collect())
}
