//! `/api/events` —— 任务台账变化的 SSE 流。
//!
//! 事件由 `TaskObserver` 的进程内广播发出；本测试走的是生产同一条路：
//! 观察 hook 落库 → 广播 → SSE 转发。

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use futures::StreamExt;
use peco_core::agent::hooks::{DispatchObservation, DispatchOrigin, TaskObservationSink};
use peco_core::machine::{OutputView, Progress, TaskId, TaskSnapshot, TaskSource, TaskState};
use serde_json::json;
use support::new_env;
use tokio::time::timeout;
use uuid::Uuid;

fn snapshot(task_id: TaskId, state: TaskState) -> TaskSnapshot {
    TaskSnapshot {
        id: task_id,
        kind: "agent".into(),
        state,
        state_detail: None,
        progress: Progress {
            phase: None,
            current: None,
            total: None,
        },
        output: OutputView {
            tail: Vec::new(),
            start_offset: 0,
            end_offset: 0,
        },
        outcome: None,
        artifacts: Vec::new(),
        deadline: None,
        source: TaskSource::External {
            principal: "local".into(),
        },
        labels: BTreeMap::new(),
        scope: Some("research".into()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        last_seq: 0,
    }
}

fn observation(target_dir: PathBuf, task_id: TaskId) -> DispatchObservation {
    DispatchObservation {
        origin: DispatchOrigin {
            ws_id: "ws_peco".into(),
            principal: "local".into(),
            origin_dir: PathBuf::from("/tmp/peco-origin"),
        },
        target_dir,
        task_id,
        dispatch_id: Some("disp-1".into()),
        origin_kind: "dispatch_to_workspace".into(),
        agent_name: "@master".into(),
        prompt_digest: None,
    }
}

/// 读到出现 `needle` 为止；30s 内没出现就判失败。
async fn read_until(body: Body, needle: &str) -> String {
    let mut stream = body.into_data_stream();
    let mut seen = String::new();
    loop {
        let chunk = timeout(Duration::from_secs(30), stream.next())
            .await
            .expect("30s 内必须有事件")
            .expect("流未结束")
            .expect("事件体可读");
        seen.push_str(&String::from_utf8_lossy(&chunk));
        if seen.contains(needle) {
            return seen;
        }
    }
}

/// 打开连接后提交一个任务，30s 内必须收到 `event: task`；断开不得 panic。
///
/// 变异：把 handler 改成不订阅广播（返回空流）→ 30s 超时后本测试变红。
#[tokio::test]
async fn a_ledger_event_reaches_an_open_connection() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("events-space");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    let target_dir = env.work_dir_of(&ws_id).await;

    let router = env.router();
    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/events")
        .body(Body::empty())
        .unwrap();
    let response = tower::ServiceExt::oneshot(router.clone(), request)
        .await
        .expect("路由不会失败");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream"),
        "SSE 必须声明事件流类型"
    );

    // 提交任务：与生产同一条路径（观察 hook 落库后广播一条）。
    let task = TaskId(Uuid::new_v4());
    let obs = observation(target_dir, task);
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();
    let body = response.into_body();
    // 先把连接挂起来再提交任务：顺序反过来事件会在订阅之前发出。
    let reader = tokio::spawn(async move { read_until(body, "event: task").await });
    sink.on_registered(&obs, &snapshot(task, TaskState::Working))
        .await;

    let wire = timeout(Duration::from_secs(30), reader)
        .await
        .expect("读线程不得挂死")
        .expect("读线程不得 panic");

    assert!(wire.contains("event: task"), "wire={wire}");
    let data = wire
        .lines()
        .find(|line| line.starts_with("data: "))
        .expect("必须有 data 行")
        .trim_start_matches("data: ");
    let payload: serde_json::Value = serde_json::from_str(data).expect("data 必须是 JSON");
    assert_eq!(payload["task_id"], json!(task.to_string()));
    assert_eq!(
        payload["ws_id"],
        json!(ws_id),
        "载荷里的空间是任务的目标空间"
    );
    assert_eq!(payload["task_state"], json!("working"));
    assert_eq!(
        payload["wake_state"],
        json!("pending"),
        "发起方是 Peco 空间，这一行要进唤醒队列"
    );
    assert_eq!(payload["wake_skip_reason"], json!(null));
    assert_eq!(payload["origin_kind"], json!("dispatch_to_workspace"));
    assert!(payload["created_at"].is_string());
    assert_eq!(payload["terminal_at"], json!(null));
    assert!(
        payload.get("state").is_none(),
        "载荷不得再出现旧字段 state：{payload}"
    );

    // 断开后再发一条：没有订阅者时广播 `send` 失败，属正常路径，不得 panic。
    sink.on_registered(&obs, &snapshot(task, TaskState::Working))
        .await;
    drop(router);
}

/// `/api/events` 是精确路径：子路径不会被这条 SSE 路由吞掉。
#[tokio::test]
async fn events_route_is_exact() {
    let env = new_env(&["research"]).await;

    let reply = support::call(&env.router(), Method::GET, "/api/events/nope", None).await;

    assert_eq!(reply.status, 404, "body={}", reply.body);
}
