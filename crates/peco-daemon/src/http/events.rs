//! `/api/events` —— 任务台账变化的 SSE 流。
//!
//! 事件源是 `TaskObserver` 的进程内广播（观察 hook 落库成功后发一条）。载荷
//! 取**落库后的台账行**而不是广播里的那几个字段：广播只是「哪一行的哪一刻变了」，
//! 前端要的是行本身，两处各拼一份 JSON 会让字段名与取值慢慢漂移。
//!
//! 例外是 `conversation_id` 非空的事件（唤醒轮结束）：它不来自台账，用广播
//! 自带的字段拼一条载荷即可。
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
use crate::space::tasks::TaskEvent;
use crate::state::AppState;

/// 心跳间隔：反代与浏览器都会掐掉长时间静默的连接，15s 留出足够余量。
const HEARTBEAT: Duration = Duration::from_secs(15);

/// 一条事件的载荷；任务态字段与 `/api/tasks` 的视图同名，前端一套类型通吃。
///
/// `wake_turn_done`（唤醒轮结束）是唯一不来自台账的 kind：那类事件只有 `ws_id`
/// 与 `conversation_id` 有意义，其余任务态字段留空串 / `null`。前端据
/// `conversation_id` 判定「这是某个会话多了消息」，不去看任务态。
#[derive(Debug, Serialize)]
pub struct TaskEventPayload {
    /// `registered` / `state` / `terminal` / `orphaned` / `wake_turn_done`。
    pub kind: String,
    /// 任务所在的（目标）空间。
    pub ws_id: String,
    /// `wake_turn_done` 下是空串：唤醒轮没有台账行。
    pub task_id: String,
    pub task_state: String,
    pub wake_state: String,
    pub wake_skip_reason: Option<String>,
    pub origin_kind: String,
    pub created_at: String,
    pub terminal_at: Option<String>,
    pub dispatch_id: Option<String>,
    /// 只有 `wake_turn_done` 有值：消息落在哪个会话。
    pub conversation_id: Option<String>,
}

impl TaskEventPayload {
    /// 台账行 → 载荷。`kind` 取自广播事件：它说的是「哪一刻」，行里没有这一列。
    fn from_row(kind: &str, row: DispatchRow) -> Self {
        Self {
            kind: kind.to_string(),
            ws_id: row.target_ws_id,
            task_id: row.task_id,
            task_state: row.task_state,
            wake_state: row.wake_state,
            wake_skip_reason: row.wake_skip_reason,
            origin_kind: row.origin_kind,
            created_at: row.created_at,
            terminal_at: row.terminal_at,
            dispatch_id: row.dispatch_id,
            conversation_id: None,
        }
    }

    /// 唤醒轮结束 → 载荷；见类型注释里对空字段的说明。
    fn from_wake_turn(event: &TaskEvent) -> Self {
        Self {
            kind: event.kind.to_string(),
            ws_id: event.target_ws_id.clone().unwrap_or_default(),
            task_id: event.task_id.clone(),
            task_state: String::new(),
            wake_state: String::new(),
            wake_skip_reason: None,
            origin_kind: String::new(),
            created_at: String::new(),
            terminal_at: None,
            dispatch_id: None,
            conversation_id: event.conversation_id.clone(),
        }
    }
}

pub async fn stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = state.tasks.subscribe();
    let shutdown = state.shutdown.clone();
    let stream = futures::stream::unfold(
        (receiver, state.pool.clone(), shutdown),
        |(mut receiver, pool, shutdown)| async move {
            loop {
                let received = tokio::select! {
                    _ = shutdown.cancelled() => return None,
                    received = receiver.recv() => received,
                };
                let event = match received {
                    Ok(event) => event,
                    // 慢消费者丢事件：台账是权威，前端按需重取即可，不补发。
                    Err(RecvError::Lagged(skipped)) => {
                        warn!(skipped, "sse subscriber lagged behind the ledger events");
                        continue;
                    }
                    // 发送端随 `TaskObserver` 一起活着；只有进程收尾才会关。
                    Err(RecvError::Closed) => return None,
                };
                // 带 `conversation_id` 的事件不查台账：`task_id` 是空串，查不到行
                // 也不该查到 —— 它说的是「某个会话多了消息」，与任务态无关。
                let payload = if event.conversation_id.is_some() {
                    TaskEventPayload::from_wake_turn(&event)
                } else {
                    match ledger_payload(&pool, &event).await {
                        Some(payload) => payload,
                        None => continue,
                    }
                };
                return Some((Ok(to_event(&payload)), (receiver, pool, shutdown)));
            }
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::new().interval(HEARTBEAT).text("ping"))
}

/// 广播里的 `task_id` → 台账行。行不存在（注册时目标空间没有索引行导致没落库）
/// 就不发这条事件：发一条没有台账依据的状态只会让前端无从核对。
async fn ledger_payload(pool: &SqlitePool, event: &TaskEvent) -> Option<TaskEventPayload> {
    let task_id = event.task_id.as_str();
    match dispatches::get_by_task_id(pool, task_id).await {
        Ok(row) => row.map(|row| TaskEventPayload::from_row(event.kind, row)),
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
