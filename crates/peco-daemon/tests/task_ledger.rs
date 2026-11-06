//! 跨空间任务台账：`TaskObserver` 落库 + `/api/tasks` 查询。

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::Utc;
use peco_core::agent::hooks::{DispatchObservation, DispatchOrigin, TaskObservationSink};
use peco_core::machine::{
    ObsEvent, OutputView, Progress, TaggedJson, TaskId, TaskOutcome, TaskSnapshot, TaskSource,
    TaskState, Update, UpdateBody, UpdateOrigin,
};
use serde_json::json;
use support::new_env;
use uuid::Uuid;

fn snapshot(task_id: TaskId, state: TaskState, output: Option<&str>) -> TaskSnapshot {
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
        outcome: output.map(|text| TaskOutcome {
            output: Some(TaggedJson {
                tag: "text".into(),
                payload: json!(text),
            }),
            error: None,
        }),
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
            ws_id: "peco".into(),
            principal: "local".into(),
            origin_dir: PathBuf::from("/tmp/peco-origin"),
        },
        target_dir,
        task_id,
        dispatch_id: Some("disp-1".into()),
        origin_kind: "dispatch_to_workspace".into(),
        agent_name: "@master".into(),
    }
}

/// 注册 → 状态 → 终态写进 `d_dispatches`，并能从三个查询接口读回。
#[tokio::test]
async fn registered_state_and_terminal_roundtrip_through_http() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("task-space");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    // 与 opener 看到的 target_dir 完全一致（canonical）。
    let target_dir = env.work_dir_of(&ws_id).await;

    let task = TaskId(Uuid::new_v4());
    let obs = observation(target_dir, task);
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    sink.on_registered(&obs, &snapshot(task, TaskState::Working, None))
        .await;
    assert_eq!(
        env.dispatch_state(&ws_id, &task.to_string()).await,
        ("working".to_string(), "skipped".to_string()),
        "注册即落库，且 A+B 阶段不产生唤醒"
    );

    let update = ObsEvent::Update(Update {
        task_id: task,
        seq: 1,
        at: Utc::now(),
        origin: UpdateOrigin::Machine,
        body: UpdateBody::Status {
            state: TaskState::InputRequired,
            detail: None,
        },
    });
    sink.on_event(&obs, &update).await;
    assert_eq!(
        env.dispatch_state(&ws_id, &task.to_string()).await.0,
        "input_required"
    );

    sink.on_terminal(
        &obs,
        &snapshot(task, TaskState::Completed, Some("final answer")),
    )
    .await;

    let row: (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT state, terminal_at, result_digest FROM d_dispatches WHERE task_id = ?",
    )
    .bind(task.to_string())
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(row.0, "completed");
    assert!(row.1.is_some(), "终态必须写 terminal_at");
    assert_eq!(row.2.as_deref(), Some("final answer"));

    // ── HTTP 查询 ────────────────────────────────────────────────────────
    let list = env.get(&format!("/api/tasks?target_ws_id={ws_id}")).await;
    assert_eq!(list.status, 200, "body={}", list.body);
    let items = list.body.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["task_id"], json!(task.to_string()));
    assert_eq!(items[0]["target_ws_id"], json!(ws_id));
    assert_eq!(items[0]["origin_ws_id"], json!("peco"));
    assert_eq!(items[0]["origin_kind"], json!("dispatch_to_workspace"));
    assert_eq!(items[0]["agent_name"], json!("@master"));
    assert_eq!(items[0]["state"], json!("completed"));
    assert_eq!(items[0]["result_digest"], json!("final answer"));

    let detail = env.get(&format!("/api/tasks/{task}")).await;
    assert_eq!(detail.status, 200, "body={}", detail.body);
    assert_eq!(detail.body["task_id"], json!(task.to_string()));

    let for_space = env.get(&format!("/api/spaces/{ws_id}/tasks")).await;
    assert_eq!(for_space.status, 200, "body={}", for_space.body);
    assert_eq!(for_space.body.as_array().unwrap().len(), 1);

    // 过滤：state 不匹配时为空。
    let filtered = env.get("/api/tasks?state=working").await;
    assert_eq!(filtered.status, 200);
    assert_eq!(filtered.body.as_array().unwrap().len(), 0);
}

/// 失联落 `orphan`，且未知 task / 未知空间都返回 404。
#[tokio::test]
async fn orphan_is_recorded_and_unknown_ids_are_404() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("orphan-space");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    let target_dir = env.work_dir_of(&ws_id).await;

    let task = TaskId(Uuid::new_v4());
    let obs = observation(target_dir, task);
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    sink.on_registered(&obs, &snapshot(task, TaskState::Submitted, None))
        .await;
    sink.on_orphaned(&obs, "target space closed").await;

    let (state, wake_state, terminal_at) = sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT state, wake_state, terminal_at FROM d_dispatches WHERE task_id = ?",
    )
    .bind(task.to_string())
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(state, "orphan");
    assert_eq!(wake_state, "skipped", "orphan 不得留下待唤醒");
    assert!(terminal_at.is_some(), "orphan 是终态");

    let missing_task = env.get("/api/tasks/not-a-task").await;
    assert_eq!(missing_task.status, 404, "body={}", missing_task.body);
    assert_eq!(missing_task.code(), "task_not_found");

    let missing_space = env.get("/api/spaces/ws_ghost/tasks").await;
    assert_eq!(missing_space.status, 404, "body={}", missing_space.body);
    assert_eq!(missing_space.code(), "space_not_found");
}

/// 终态摘要只保留前 512 个字符；非文本输出不落摘要。
#[tokio::test]
async fn terminal_digest_is_truncated_and_non_text_is_null() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("digest-space");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    let target_dir = env.work_dir_of(&ws_id).await;
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    let long = TaskId(Uuid::new_v4());
    let obs = observation(target_dir.clone(), long);
    sink.on_registered(&obs, &snapshot(long, TaskState::Working, None))
        .await;
    let long_text = "x".repeat(700);
    sink.on_terminal(
        &obs,
        &snapshot(long, TaskState::Completed, Some(&long_text)),
    )
    .await;

    let digest: Option<String> =
        sqlx::query_scalar("SELECT result_digest FROM d_dispatches WHERE task_id = ?")
            .bind(long.to_string())
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(digest.as_deref().map(str::len), Some(512));

    // 非文本载荷：outcome.output 的 payload 不是字符串 ⇒ null。
    let non_text = TaskId(Uuid::new_v4());
    let obs = observation(target_dir, non_text);
    let mut snap = snapshot(non_text, TaskState::Completed, None);
    snap.outcome = Some(TaskOutcome {
        output: Some(TaggedJson {
            tag: "json".into(),
            payload: json!({"ok": true}),
        }),
        error: None,
    });
    sink.on_registered(&obs, &snapshot(non_text, TaskState::Working, None))
        .await;
    sink.on_terminal(&obs, &snap).await;

    let digest: Option<String> =
        sqlx::query_scalar("SELECT result_digest FROM d_dispatches WHERE task_id = ?")
            .bind(non_text.to_string())
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert!(digest.is_none(), "非文本输出不应落摘要");
}
