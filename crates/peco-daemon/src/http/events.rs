//! `/api/events` —— 任务台账变化的 SSE 流。
//!
//! 事件源是 `TaskObserver` 的进程内广播（观察 hook 落库成功后发一条）。载荷
//! 取**落库后的台账行**而不是广播里的那几个字段：广播只是「哪一行的哪一刻变了」，
//! 前端要的是行本身，两处各拼一份 JSON 会让字段名与取值慢慢漂移。
//!
//! 不做重放：不为每个订阅者留历史，前端重连后自己重取一次。不做鉴权：单用户
//! daemon 与本机回环监听。客户端断开时 axum 丢弃这个流的 future，接收端随之
//! 释放 —— 本模块不 spawn 任何后台任务，因此没有需要回收的观察者。

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::Stream;
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::broadcast::error::RecvError;
use tracing::warn;

use crate::db::dispatches::{self, DispatchRow};
use crate::state::AppState;

/// 心跳间隔：反代与浏览器都会掐掉长时间静默的连接，15s 留出足够余量。
const HEARTBEAT: Duration = Duration::from_secs(15);

/// 一条台账事件的载荷；字段与 `/api/tasks` 的视图同名，前端一套类型通吃。
#[derive(Debug, Serialize)]
pub struct TaskEventPayload {
    /// 任务所在的（目标）空间。
    pub ws_id: String,
    pub task_id: String,
    pub task_state: String,
    pub wake_state: String,
    pub wake_skip_reason: Option<String>,
    pub origin_kind: String,
    pub created_at: String,
    pub terminal_at: Option<String>,
}

impl From<DispatchRow> for TaskEventPayload {
    fn from(row: DispatchRow) -> Self {
        Self {
            ws_id: row.target_ws_id,
            task_id: row.task_id,
            task_state: row.task_state,
            wake_state: row.wake_state,
            wake_skip_reason: row.wake_skip_reason,
            origin_kind: row.origin_kind,
            created_at: row.created_at,
            terminal_at: row.terminal_at,
        }
    }
}

pub async fn stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = state.tasks.subscribe();
    let stream = futures::stream::unfold(
        (receiver, state.pool.clone()),
        |(mut receiver, pool)| async move {
            loop {
                let task_id = match receiver.recv().await {
                    Ok(event) => event.task_id,
                    // 慢消费者丢事件：台账是权威，前端按需重取即可，不补发。
                    Err(RecvError::Lagged(skipped)) => {
                        warn!(skipped, "sse subscriber lagged behind the ledger events");
                        continue;
                    }
                    // 发送端随 `TaskObserver` 一起活着；只有进程收尾才会关。
                    Err(RecvError::Closed) => return None,
                };
                if let Some(payload) = ledger_payload(&pool, &task_id).await {
                    return Some((Ok(to_event(&payload)), (receiver, pool)));
                }
            }
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::new().interval(HEARTBEAT).text("ping"))
}

/// 广播里的 `task_id` → 台账行。行不存在（注册时目标空间没有索引行导致没落库）
/// 就不发这条事件：发一条没有台账依据的状态只会让前端无从核对。
async fn ledger_payload(pool: &SqlitePool, task_id: &str) -> Option<TaskEventPayload> {
    match dispatches::get_by_task_id(pool, task_id).await {
        Ok(row) => row.map(TaskEventPayload::from),
        Err(e) => {
            warn!(task_id, error = %e, "cannot read the ledger row for an sse event");
            None
        }
    }
}

fn to_event(payload: &TaskEventPayload) -> Event {
    match Event::default().event("task").json_data(payload) {
        Ok(event) => event,
        // 载荷全是字符串字段，序列化失败只可能是上游 serde 出了问题：
        // 一条注释仍能让连接活着，比 panic 掉整个流好。
        Err(e) => {
            warn!(error = %e, "task event payload is not serializable");
            Event::default().comment("unserializable task event")
        }
    }
}
