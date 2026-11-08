//! `/api/spaces/{ws_id}/tasks/live|{task_id}/stream|{task_id}/cancel` —— 实时任务观察。
//!
//! 实时面只读该空间 `TaskHub` 的内存快照：任务在飞时完整输出只在那里。空间没打开
//! 就没有数据源，故是 409 而不是空数组 —— 空数组会被前端读成「空间开着、没有任务」。
//! 台账的 512 字摘要是空间关闭后的回落，两条路径互不替代。

mod support;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use futures::{Stream, StreamExt};
use peco_core::machine::{
    EchoMachine, Machine, MachineDescriptor, MachineError, TaggedJson, Task, TaskContext, TaskId,
    TaskOutcome, TaskSink, TaskSnapshot, TaskSource, TaskState,
};
use serde_json::json;
use support::{TestEnv, new_env};
use tokio::sync::Notify;
use tokio::time::timeout;
use tower::ServiceExt;
use uuid::Uuid;

/// 读一帧的上限：SSE 帧本身是立刻可读的，30s 只是防止用例挂死。
const READ_BUDGET: Duration = Duration::from_secs(30);

fn text_task(text: &str) -> Task {
    Task {
        // Hub 会覆盖 id 与 kind。
        id: TaskId(Uuid::nil()),
        kind: String::new(),
        input: TaggedJson {
            tag: "text".to_string(),
            payload: json!(text),
        },
        context: TaskContext {
            scope: None,
            data: None,
        },
        source: TaskSource::External {
            principal: "local".to_string(),
        },
        deadline: None,
        labels: BTreeMap::new(),
        created_at: Utc::now(),
    }
}

/// 接单后停在 `Working`，由测试自己推进 —— 实时端点要观察的正是这一小段时间。
struct GatedMachine {
    sink: Mutex<Option<TaskSink>>,
    entered: Notify,
    cancels: AtomicUsize,
}

impl GatedMachine {
    fn new() -> Self {
        Self {
            sink: Mutex::new(None),
            entered: Notify::new(),
            cancels: AtomicUsize::new(0),
        }
    }

    async fn finish(&self, state: TaskState, output: &str) {
        let sink = self
            .sink
            .lock()
            .expect("sink table")
            .clone()
            .expect("the task must have been submitted first");
        sink.finish(
            state,
            TaskOutcome {
                output: Some(TaggedJson {
                    tag: "text".to_string(),
                    payload: json!(output),
                }),
                error: None,
            },
        )
        .await
        .expect("the task is still non-terminal");
    }
}

#[async_trait]
impl Machine for GatedMachine {
    fn descriptor(&self) -> MachineDescriptor {
        MachineDescriptor {
            kind: "gated".to_string(),
            state_tag: "gated".to_string(),
            input_tag: "text".to_string(),
            description: "test double: waits for the test to finish it".to_string(),
        }
    }

    async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
        sink.begin()
            .await
            .map_err(|err| MachineError::Unavailable {
                reason: err.to_string(),
            })?;
        *self.sink.lock().expect("sink table") = Some(sink.clone());
        self.entered.notify_one();
        // future 寿命即执行体寿命：返回后 Hub 会把仍未终态的任务判成 Failed。
        std::future::pending::<()>().await;
        Ok(())
    }

    async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
        Err(MachineError::NotFound { id: *id })
    }
}

/// 把 `kind` 对应的 machine 注册进该空间的 Hub，返回 runtime 里的 Hub 句柄。
async fn register(
    env: &TestEnv,
    ws_id: &str,
    machine: Arc<dyn Machine>,
) -> Arc<peco_core::machine::TaskHub> {
    let handle = env.open(ws_id).await.expect("space opens");
    let runtime = handle.machine_runtime().upgrade().expect("runtime alive");
    let hub = runtime.hub().expect("hub");
    match hub.register(machine) {
        Ok(()) | Err(MachineError::Illegal { .. }) => hub,
        Err(err) => panic!("cannot register the machine: {err}"),
    }
}

/// 提交一个 echo 任务并等它收敛：只有终态才不依赖 Hub 的宽限窗口。
async fn submit_echo(env: &TestEnv, ws_id: &str, text: &str) -> TaskId {
    let hub = register(env, ws_id, Arc::new(EchoMachine::new())).await;
    let id = hub
        .submit("echo", text_task(text))
        .await
        .expect("submit echo");
    hub.await_terminal(&id, Some(Duration::from_secs(10)))
        .await
        .expect("the echo task converges");
    id
}

/// 读到已收到的整段文本里出现 `needle` 为止（SSE 帧可能被切成多块）。
async fn read_until<S>(stream: &mut S, seen: &mut String, needle: &str)
where
    S: Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin,
{
    while !seen.contains(needle) {
        let chunk = timeout(READ_BUDGET, stream.next())
            .await
            .unwrap_or_else(|_| panic!("{needle} 必须到达，已收到：{seen}"))
            .expect("流未结束")
            .expect("帧可读");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
}

async fn open_stream(env: &TestEnv, ws_id: &str, task_id: &str) -> Body {
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("/api/spaces/{ws_id}/tasks/{task_id}/stream"))
        .body(Body::empty())
        .expect("build the SSE request");
    let response = env
        .router()
        .oneshot(request)
        .await
        .expect("router 不会失败");
    assert_eq!(response.status(), StatusCode::OK, "SSE 握手必须成功");
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream"),
        "SSE 必须声明事件流类型"
    );
    response.into_body()
}

/// 首帧事件的数据载荷：`event:` 行之后第一条 `data:` 行。
fn data_of(wire: &str, event: &str) -> serde_json::Value {
    let mut lines = wire.lines();
    while let Some(line) = lines.next() {
        if line == format!("event: {event}") {
            let data = lines
                .by_ref()
                .find(|line| line.starts_with("data: "))
                .unwrap_or_else(|| panic!("{event} 帧必须有 data 行：{wire}"));
            return serde_json::from_str(data.trim_start_matches("data: "))
                .unwrap_or_else(|err| panic!("data 必须是 JSON（{err}）：{wire}"));
        }
    }
    panic!("没有 {event} 帧：{wire}");
}

/// `/live` 只返回该空间自己的快照，且不外泄执行体的内部字段。
///
/// 变异：把 handler 换成返回 `Vec::new()` ⇒ 条数与字段断言同时红；把 `From`
/// 换成直接序列化 `TaskSnapshot` ⇒ `last_seq` 断言红。
#[tokio::test]
async fn live_lists_the_snapshots_of_that_space() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("live-a", "research").await;
    let ws_b = env.open_space("live-b", "research").await;
    let task = submit_echo(&env, &ws_a, "hello live").await;

    let reply = env.get(&format!("/api/spaces/{ws_a}/tasks/live")).await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    let items = reply.body.as_array().expect("必须是数组");
    assert_eq!(items.len(), 1, "该空间只有一个任务：{}", reply.body);
    assert_eq!(items[0]["id"], json!(task.to_string()));
    assert_eq!(items[0]["kind"], json!("echo"));
    assert_eq!(items[0]["state"], json!("completed"));
    assert_eq!(
        items[0]["outcome"]["output"]["payload"],
        json!("hello live"),
        "终态输出就在快照里"
    );
    assert!(items[0]["created_at"].is_string());
    assert!(items[0]["updated_at"].is_string());
    for internal in ["last_seq", "source", "labels", "scope", "deadline"] {
        assert!(
            items[0].get(internal).is_none(),
            "执行体内部字段 {internal} 不该出现在实时视图里：{}",
            items[0]
        );
    }

    let other = env.get(&format!("/api/spaces/{ws_b}/tasks/live")).await;
    assert_eq!(other.status, 200, "body={}", other.body);
    assert_eq!(
        other.body.as_array().map(Vec::len),
        Some(0),
        "另一个空间看不到这个任务：{}",
        other.body
    );
}

/// 未打开的空间是错误，不是空列表 —— 空列表会让「Hub 不在」看起来像「没有任务」。
#[tokio::test]
async fn live_reports_an_unopened_space_instead_of_an_empty_list() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("live-closed", "research").await;
    let work_dir = env.work_dir_of(&ws_id).await;
    // 只摘实例，不删空间行：这正是「daemon 重启、空间还没恢复」的形状。
    assert!(env.hub.forget(&work_dir), "夹具需要一次释放");

    let reply = env.get(&format!("/api/spaces/{ws_id}/tasks/live")).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "body={}", reply.body);
    assert_eq!(reply.code(), "space_not_open");

    let missing = env.get("/api/spaces/ws_ghost/tasks/live").await;
    assert_eq!(
        missing.status,
        StatusCode::NOT_FOUND,
        "body={}",
        missing.body
    );
    assert_eq!(missing.code(), "space_not_found");
}

/// 首帧是快照，随后的推进以增量帧到达；终态后连接自行结束。
///
/// 变异：把首帧改成 `update` 帧 ⇒ 第一条断言红；订阅时不等首帧就返回空流 ⇒
/// `read_until` 在 30s 后 panic。
#[tokio::test]
async fn the_stream_starts_with_a_snapshot_and_then_forwards_updates() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("stream-space", "research").await;
    let machine = Arc::new(GatedMachine::new());
    let hub = register(&env, &ws_id, machine.clone() as Arc<dyn Machine>).await;

    let task = hub
        .submit("gated", text_task("stream me"))
        .await
        .expect("submit gated");
    timeout(Duration::from_secs(10), machine.entered.notified())
        .await
        .expect("the machine must accept the task");

    let body = open_stream(&env, &ws_id, &task.to_string()).await;
    let mut frames = body.into_data_stream();
    let mut wire = String::new();
    read_until(&mut frames, &mut wire, "event: snapshot").await;

    let snapshot = data_of(&wire, "snapshot");
    assert_eq!(snapshot["id"], json!(task.to_string()));
    assert_eq!(
        snapshot["state"],
        json!("working"),
        "首帧必须是订阅那一刻的权威状态：{snapshot}"
    );

    machine.finish(TaskState::Completed, "final answer").await;
    read_until(&mut frames, &mut wire, "event: update").await;
    let update = data_of(&wire, "update");
    assert_eq!(update["task_id"], json!(task.to_string()));
    assert!(
        update["seq"].as_u64().is_some_and(|seq| seq >= 1),
        "增量必须带 seq：{update}"
    );

    // 终态投递完就关流：客户端不该挂着一条永远不再有新内容的连接。
    let rest = timeout(Duration::from_secs(10), async {
        while frames.next().await.is_some() {}
    })
    .await;
    assert!(rest.is_ok(), "终态后事件流必须结束");
}

/// cancel 真的转发到了执行体；未知 / 非法 id 是 404，空间没开是 409。
///
/// 变异：handler 改成直接答 `{"cancelled": true}` 不调 Hub ⇒ 执行体收到的取消
/// 次数为 0，断言红。
#[tokio::test]
async fn cancel_forwards_to_the_executor_and_reports_unknown_ids() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("cancel-space", "research").await;
    let machine = Arc::new(GatedMachine::new());
    let hub = register(&env, &ws_id, machine.clone() as Arc<dyn Machine>).await;
    let task = hub
        .submit("gated", text_task("cancel me"))
        .await
        .expect("submit gated");
    timeout(Duration::from_secs(10), machine.entered.notified())
        .await
        .expect("the machine must accept the task");

    let reply = env
        .post(
            &format!("/api/spaces/{ws_id}/tasks/{task}/cancel"),
            json!({}),
        )
        .await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["cancelled"], json!(true));

    // 转发的证据在执行体自己身上：取消是 Hub 异步派给 machine 的。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while machine.cancels.load(Ordering::SeqCst) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "cancel 必须转发到执行体"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let unknown = env
        .post(
            &format!("/api/spaces/{ws_id}/tasks/{}/cancel", Uuid::new_v4()),
            json!({}),
        )
        .await;
    assert_eq!(
        unknown.status,
        StatusCode::NOT_FOUND,
        "body={}",
        unknown.body
    );
    assert_eq!(unknown.code(), "task_not_found");

    // 路径里不是 UUID 时同样不可能命中任何任务。
    let malformed = env
        .post(
            &format!("/api/spaces/{ws_id}/tasks/not-a-uuid/cancel"),
            json!({}),
        )
        .await;
    assert_eq!(
        malformed.status,
        StatusCode::NOT_FOUND,
        "body={}",
        malformed.body
    );
    assert_eq!(malformed.code(), "task_not_found");

    let work_dir = env.work_dir_of(&ws_id).await;
    assert!(env.hub.forget(&work_dir), "夹具需要一次释放");
    let closed = env
        .post(
            &format!("/api/spaces/{ws_id}/tasks/{task}/cancel"),
            json!({}),
        )
        .await;
    assert_eq!(closed.status, StatusCode::CONFLICT, "body={}", closed.body);
    assert_eq!(closed.code(), "space_not_open");
}
