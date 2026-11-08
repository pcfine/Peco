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
use peco_daemon::space::tasks::TaskObserver;
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
        prompt_digest: None,
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
        env.task_and_wake_state(&ws_id, &task.to_string()).await,
        ("working".to_string(), "skipped".to_string()),
        "注册即落库，且 A+B 阶段不产生唤醒"
    );
    assert_eq!(
        env.wake_skip_reason(&ws_id, &task.to_string())
            .await
            .as_deref(),
        Some("not_peco_origin"),
        "夹具的发起方不是 Peco 空间，跳过要说明是来源不合规"
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
        env.task_and_wake_state(&ws_id, &task.to_string()).await.0,
        "input_required"
    );

    sink.on_terminal(
        &obs,
        &snapshot(task, TaskState::Completed, Some("final answer")),
    )
    .await;

    let row: (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT task_state, terminal_at, result_digest FROM d_dispatches WHERE task_id = ?",
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
    assert_eq!(items[0]["task_state"], json!("completed"));
    assert_eq!(items[0]["wake_state"], json!("skipped"));
    assert_eq!(items[0]["wake_skip_reason"], json!("not_peco_origin"));
    assert_eq!(items[0]["result_digest"], json!("final answer"));
    assert!(
        items[0].get("state").is_none(),
        "响应不得再出现旧字段 state：{:#}",
        items[0]
    );

    let detail = env.get(&format!("/api/tasks/{task}")).await;
    assert_eq!(detail.status, 200, "body={}", detail.body);
    assert_eq!(detail.body["task_id"], json!(task.to_string()));

    let for_space = env.get(&format!("/api/spaces/{ws_id}/tasks")).await;
    assert_eq!(for_space.status, 200, "body={}", for_space.body);
    assert_eq!(for_space.body.as_array().unwrap().len(), 1);

    // 过滤：新旧两个参数名都要能用，取值都不匹配时为空。
    for query in ["?state=working", "?task_state=working"] {
        let filtered = env.get(&format!("/api/tasks{query}")).await;
        assert_eq!(filtered.status, 200, "{query} body={}", filtered.body);
        assert_eq!(
            filtered.body.as_array().unwrap().len(),
            0,
            "{query} 必须真的过滤，而不是被当成未知参数忽略"
        );
    }
    // 命中：两个参数名都能筛出这唯一一行。
    for query in ["?state=completed", "?task_state=completed"] {
        let filtered = env.get(&format!("/api/tasks{query}")).await;
        assert_eq!(filtered.body.as_array().unwrap().len(), 1, "{query}");
    }
    // 同时给出且互相矛盾时 `task_state` 优先 —— 两个方向都断一次，只断一边的话
    // 「取旧参数」的实现也能蒙对一半。
    let both = env
        .get("/api/tasks?state=working&task_state=completed")
        .await;
    assert_eq!(
        both.body.as_array().unwrap().len(),
        1,
        "task_state=completed 必须压过 state=working，body={}",
        both.body
    );
    let reversed = env
        .get("/api/tasks?task_state=working&state=completed")
        .await;
    assert_eq!(
        reversed.body.as_array().unwrap().len(),
        0,
        "task_state=working 必须压过 state=completed，body={}",
        reversed.body
    );
}

/// `prompt_digest` 从观察一路落到台账与两个查询接口；列为 NULL 的旧行原样
/// 返回 `null`（设计口径：没有记录就说没有，不编造）。
///
/// 变异：`NewDispatch` 不带这一列 / `insert` 少绑一个占位符 ⇒ 第一条断言红。
#[tokio::test]
async fn prompt_digest_roundtrips_and_legacy_rows_stay_null() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("prompt-space");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    let target_dir = env.work_dir_of(&ws_id).await;
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    let task = TaskId(Uuid::new_v4());
    let mut obs = observation(target_dir, task);
    obs.prompt_digest = Some("整理 output/ 目录并写一份清单".to_string());
    sink.on_registered(&obs, &snapshot(task, TaskState::Working, None))
        .await;

    let stored: Option<String> =
        sqlx::query_scalar("SELECT prompt_digest FROM d_dispatches WHERE task_id = ?")
            .bind(task.to_string())
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(stored.as_deref(), Some("整理 output/ 目录并写一份清单"));

    for uri in [
        format!("/api/tasks/{task}"),
        format!("/api/tasks?target_ws_id={ws_id}"),
        format!("/api/spaces/{ws_id}/tasks"),
    ] {
        let reply = env.get(&uri).await;
        assert_eq!(reply.status, 200, "{uri} body={}", reply.body);
        let item = match reply.body.as_array() {
            Some(items) => {
                assert_eq!(items.len(), 1, "{uri} body={}", reply.body);
                items[0].clone()
            }
            None => reply.body.clone(),
        };
        assert_eq!(
            item["prompt_digest"],
            json!("整理 output/ 目录并写一份清单"),
            "{uri} 必须带 prompt_digest"
        );
    }

    // 迁移前写入的旧行：列是 NULL，接口就是 null。
    let legacy = TaskId(Uuid::new_v4());
    sqlx::query(
        "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, \
         target_team_id, origin_ws_id, agent_name, origin_kind, task_state, wake_state) \
         VALUES (?, ?, ?, '/tmp/work', 'research', 'peco', '@master', \
         'dispatch_to_workspace', 'working', 'pending')",
    )
    .bind(format!("d_{legacy}"))
    .bind(legacy.to_string())
    .bind(&ws_id)
    .execute(&env.pool)
    .await
    .unwrap();

    let legacy_reply = env.get(&format!("/api/tasks/{legacy}")).await;
    assert_eq!(legacy_reply.status, 200, "body={}", legacy_reply.body);
    assert_eq!(
        legacy_reply.body["prompt_digest"],
        json!(null),
        "旧行不得编造一段描述"
    );
}

/// 失联落 `orphan`，且未知 task / 未知空间都返回 404。
#[tokio::test]
async fn orphan_is_recorded_and_unknown_ids_are_404() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("orphan-space");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    let target_dir = env.work_dir_of(&ws_id).await;

    // 发起方取 Peco 自己的空间：这一行注册时要真的进队列，失联才有东西可停。
    let task = TaskId(Uuid::new_v4());
    let mut obs = observation(target_dir, task);
    obs.origin.ws_id = env.env.peco.ws_id.clone();
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    sink.on_registered(&obs, &snapshot(task, TaskState::Submitted, None))
        .await;
    assert_eq!(
        env.task_and_wake_state(&ws_id, &task.to_string()).await.1,
        "pending"
    );
    sink.on_orphaned(&obs, "target space closed").await;

    let (task_state, wake_state, terminal_at, reason) =
        sqlx::query_as::<_, (String, String, Option<String>, Option<String>)>(
            "SELECT task_state, wake_state, terminal_at, wake_skip_reason \
         FROM d_dispatches WHERE task_id = ?",
        )
        .bind(task.to_string())
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(task_state, "orphan");
    assert_eq!(wake_state, "skipped", "orphan 不得留下待唤醒");
    assert!(terminal_at.is_some(), "orphan 是终态");
    assert_eq!(
        reason.as_deref(),
        Some("orphaned"),
        "运行期失联的跳过要给 orphaned"
    );

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

/// 观察者拿不到 Peco 身份（装配异常）时跳过要自成一因：它与「来源不是 Peco」
/// 都落在 `wake_state='skipped'`，只有 `wake_skip_reason` 能区分「该修装配」
/// 和「本该如此」。
#[tokio::test]
async fn a_task_observer_without_peco_identity_records_that_reason() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("no-identity-space");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    let target_dir = env.work_dir_of(&ws_id).await;

    // 故意不调 attach_peco：等价于装配点漏注入身份。
    let observer = TaskObserver::new(env.pool.clone(), env.wake.clone());
    let sink: &dyn TaskObservationSink = &observer;

    // 来源取 Peco 自己的空间：只要身份在场，这一行本该进队列。
    let peco_ws_id = env.env.peco.ws_id.clone();
    let blind = TaskId(Uuid::new_v4());
    let mut blind_obs = observation(target_dir.clone(), blind);
    blind_obs.origin.ws_id = peco_ws_id.clone();
    sink.on_registered(&blind_obs, &snapshot(blind, TaskState::Working, None))
        .await;

    assert_eq!(
        env.task_and_wake_state(&ws_id, &blind.to_string()).await.1,
        "skipped"
    );
    assert_eq!(
        env.wake_skip_reason(&ws_id, &blind.to_string())
            .await
            .as_deref(),
        Some("peco_identity_missing"),
        "身份缺失是装配异常，不能与来源不合规混成同一个原因"
    );

    // 同一来源、同一形状，身份注入后就该进队列 —— 证明上一条的成因确实是身份。
    observer.attach_peco(env.env.peco.clone());
    let sighted = TaskId(Uuid::new_v4());
    let mut sighted_obs = observation(target_dir, sighted);
    sighted_obs.origin.ws_id = peco_ws_id;
    sink.on_registered(&sighted_obs, &snapshot(sighted, TaskState::Working, None))
        .await;

    assert_eq!(
        env.task_and_wake_state(&ws_id, &sighted.to_string())
            .await
            .1,
        "pending"
    );
    assert_eq!(
        env.wake_skip_reason(&ws_id, &sighted.to_string()).await,
        None,
        "进队列的行不写跳过原因"
    );
}
