//! P2 观察闭环：一轮真实对话 → 工具派发 → 观察 → 台账。
//!
//! 每条用例都从一条真实的 `GET /api/chat/.../stream` 出发，模型由本地假上游
//! （`support::upstream`）脚本化：第一轮返回工具调用，第二轮返回纯文本。断言
//! 落在 `d_dispatches` 上 —— 那是「有人观察」的唯一证据。
//!
//! 工具侧的 `workspace_hub()` 是进程级 `OnceLock`，只能安装一次，故工具派发
//! 相关的用例共用一个 `TestEnv`；隔离靠「一个用例一个团队目录 + 一个假上游」。

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request};
use chrono::Utc;
use futures::StreamExt;
use peco_core::machine::{TaggedJson, Task, TaskContext, TaskId, TaskSource, TaskState};
use peco_daemon::db::dispatches::{
    self, DispatchFilter, DispatchRow, NewDispatch, TASK_ACTIVE_STATES, TASK_TERMINAL_STATES,
    WakeState,
};
use peco_daemon::space::reconcile::{ReconcileReport, reconcile_ledger};
use serde_json::{Value, json};
use support::TestEnv;
use support::upstream::{self, FakeUpstream, Reply};
use support::{fixture, install_team};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use uuid::Uuid;

static SHARED: OnceCell<TestEnv> = OnceCell::const_new();

async fn shared() -> &'static TestEnv {
    SHARED
        .get_or_init(|| async {
            let env = support::new_env(&[]).await;
            peco_core::tools::install_workspace_hub(env.hub.clone())
                .unwrap_or_else(|_| panic!("the process-level hub may only be installed once"));
            env
        })
        .await
}

/// 只建团队目录（不需要 provider 的场景，如「空间没恢复」）。
fn install_bare_team(env: &TestEnv, team: &str) {
    std::fs::create_dir_all(env.home.join("teams").join(team)).expect("create the team root");
}

// ============================================================================
// 夹具：保持一条 SSE 对话存活
// ============================================================================

/// 在跑的对话。drop 即断流，后台 turn 随之收尾。
struct Turn {
    drain: tokio::task::JoinHandle<()>,
}

impl Drop for Turn {
    fn drop(&mut self) {
        self.drain.abort();
    }
}

/// 发起一轮对话并持续消费 SSE 流。
///
/// 消费不可省：响应体一被丢弃，handler 的 `sse_tx.closed()` 立刻就绪，它会退出
/// 循环并丢弃 looper 句柄 —— 于是「这一轮」根本不会发生。
async fn start_turn(env: &TestEnv, ws_id: &str, agent: &str, message: &str) -> Turn {
    let created = env
        .post_in(
            ws_id,
            &format!("/api/chat/{agent}/conversations"),
            json!({}),
        )
        .await;
    assert_eq!(created.status, 201, "建会话失败: {}", created.body);
    let conv_id = created.body["id"]
        .as_str()
        .expect("会话必须带 id")
        .to_string();

    let uri = format!("/api/chat/{agent}/conversations/{conv_id}/stream?message={message}");
    let request = Request::builder()
        .method(Method::GET)
        .uri(&uri)
        .header("X-Peco-Space", ws_id)
        .body(Body::empty())
        .expect("build the SSE request");
    let response = env
        .router()
        .oneshot(request)
        .await
        .expect("router 不会失败");
    assert!(
        response.status().is_success(),
        "SSE 握手失败: {}",
        response.status()
    );

    let mut body = response.into_body().into_data_stream();
    let drain = tokio::spawn(async move { while body.next().await.is_some() {} });
    Turn { drain }
}

// ============================================================================
// 台账断言
// ============================================================================

/// 1s 内必须出现指定 `origin_kind` 的行；超时即失败（而不是返回 `None` ——
/// 那会让「什么都没写」也变成可接受的结果）。
async fn wait_for_row(env: &TestEnv, target_ws_id: &str, origin_kind: &str) -> DispatchRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let rows = dispatches::list(
            &env.pool,
            DispatchFilter {
                target_ws_id: Some(target_ws_id),
                ..DispatchFilter::default()
            },
        )
        .await
        .expect("list the ledger");
        if let Some(row) = rows.into_iter().find(|row| row.origin_kind == origin_kind) {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "1s 内台账必须出现 {origin_kind} 行（目标空间 {target_ws_id}）"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn assert_active_or_terminal(row: &DispatchRow) {
    assert!(
        TASK_ACTIVE_STATES.contains(&row.task_state.as_str())
            || TASK_TERMINAL_STATES.contains(&row.task_state.as_str()),
        "台账行的状态必须落在状态机里，实际 {}",
        row.task_state
    );
}

fn print_exchanges(label: &str, upstream: &FakeUpstream) {
    for (index, exchange) in upstream.exchanges().iter().enumerate() {
        eprintln!(
            "[{label}] #{index} request={} response={}",
            exchange.request, exchange.response
        );
    }
}

// ============================================================================
// T-A：跨空间派发（V6-7）
// ============================================================================

/// 变异方法：删掉 `space/mod.rs::SpaceEnv::looper_config` 里的 `config.hooks.push(...)`
/// ⇒ 派发成功但无人观察 ⇒ 1s 内台账没有行，本用例超时断言变红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispatch_from_a_chat_turn_lands_in_the_target_ledger() {
    let env = shared().await;
    let target_dir = env.work_dir("t_a_target");

    let origin_upstream = {
        let target = target_dir.display().to_string();
        FakeUpstream::start(move |request: Value| {
            let target = target.clone();
            async move {
                // 已经带着工具结果 = 这是工具执行后的续轮：收尾。
                if upstream::has_tool_result(&request) {
                    Reply::Text("dispatched".to_string())
                } else {
                    Reply::tool_call(
                        "dispatch_to_workspace",
                        json!({"dir": target, "prompt": "hi", "wait": false}),
                    )
                }
            }
        })
        .await
    };
    let target_upstream =
        FakeUpstream::start(|_: Value| async move { Reply::Text("target done".to_string()) }).await;

    install_team(
        env,
        "t_a_origin",
        &origin_upstream,
        &[fixture("@master", "master", &["dispatch_to_workspace"])],
    );
    install_team(
        env,
        "t_a_target",
        &target_upstream,
        &[fixture("@master", "target", &[])],
    );

    let ws_a = env.open_space("t_a_origin", "t_a_origin").await;
    let ws_b = env.open_space("t_a_target", "t_a_target").await;

    let _turn = start_turn(env, &ws_a, "@master", "hi").await;
    let row = wait_for_row(env, &ws_b, "dispatch_to_workspace").await;

    assert_eq!(
        row.origin_ws_id, ws_a,
        "发起方必须是说话的那个空间，实际 {}",
        row.origin_ws_id
    );
    assert_eq!(row.target_ws_id, ws_b, "目标必须是派发目录所属的空间");
    assert_eq!(row.agent_name, "@master", "派发的目标恒为团队总管");
    assert_eq!(
        row.prompt_digest.as_deref(),
        Some("hi"),
        "台账要记录派发入参里那段任务描述：卡片靠它显示「让它做什么」"
    );
    assert_active_or_terminal(&row);

    // 假上游确实收到过两个模型的请求，且工具真的在模型可见的工具表里。
    let offered = origin_upstream.offered_tools();
    assert!(
        offered.contains(&"dispatch_to_workspace".to_string()),
        "模型必须看到 dispatch_to_workspace，实际 {offered:?}"
    );
    assert!(
        !target_upstream.exchanges().is_empty(),
        "目标空间的 @master 必须真的被跑过一轮"
    );
    print_exchanges("T-A origin", &origin_upstream);
    print_exchanges("T-A target", &target_upstream);
}

// ============================================================================
// T-E2：被派发的 @master 自己再派发，同样被观察（空间打开时的 hook 注入）
// ============================================================================

/// 变异方法：删掉 `space/open.rs` 里 `ws.machine_runtime()` 之后的
/// `set_looper_hooks` ⇒ B 的 machine 没有 hook ⇒ B 派发给 C 的任务无人观察，
/// 本用例在「目标空间 C 出现 origin_ws_id = B 的行」处超时变红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispatched_master_that_dispatches_again_is_also_observed() {
    let env = shared().await;
    let middle_dir = env.work_dir("t_e2_middle");
    let leaf_dir = env.work_dir("t_e2_leaf");

    let dispatch_rule = |target: String| {
        move |request: Value| {
            let target = target.clone();
            async move {
                if upstream::has_tool_result(&request) {
                    Reply::Text("sent".to_string())
                } else {
                    Reply::tool_call(
                        "dispatch_to_workspace",
                        json!({"dir": target, "prompt": "hi", "wait": false}),
                    )
                }
            }
        }
    };

    let origin_upstream =
        FakeUpstream::start(dispatch_rule(middle_dir.display().to_string())).await;
    let middle_upstream = FakeUpstream::start(dispatch_rule(leaf_dir.display().to_string())).await;
    let leaf_upstream =
        FakeUpstream::start(|_: Value| async move { Reply::Text("leaf done".to_string()) }).await;

    for (team, upstream, model) in [
        ("t_e2_origin", &origin_upstream, "origin"),
        ("t_e2_middle", &middle_upstream, "middle"),
    ] {
        install_team(
            env,
            team,
            upstream,
            &[fixture("@master", model, &["dispatch_to_workspace"])],
        );
    }
    install_team(
        env,
        "t_e2_leaf",
        &leaf_upstream,
        &[fixture("@master", "leaf", &[])],
    );

    let ws_origin = env.open_space("t_e2_origin", "t_e2_origin").await;
    let ws_middle = env.open_space("t_e2_middle", "t_e2_middle").await;
    let ws_leaf = env.open_space("t_e2_leaf", "t_e2_leaf").await;

    let _turn = start_turn(env, &ws_origin, "@master", "hi").await;
    // 第一跳（origin → middle）先到：它把 middle 的 machine 注册起来。
    let first = wait_for_row(env, &ws_middle, "dispatch_to_workspace").await;
    assert_eq!(first.origin_ws_id, ws_origin);

    // 第二跳（middle → leaf）只有 middle 的 machine 带 hook 才可能出现。
    let second = wait_for_row(env, &ws_leaf, "dispatch_to_workspace").await;
    assert_eq!(
        second.origin_ws_id, ws_middle,
        "被派发的团队总管自己派发出去的任务同样必须有人观察"
    );
    assert_eq!(second.target_ws_id, ws_leaf);
    assert_active_or_terminal(&second);
    print_exchanges("T-E2 middle", &middle_upstream);
}

// ============================================================================
// T-B：同空间异步（V6-6）
// ============================================================================

/// 变异方法：`plan_watches` 去掉 `run_machine_task` 分支 ⇒ 台账没有行，
/// 本用例超时断言变红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_same_space_async_machine_task_lands_in_the_ledger() {
    let env = shared().await;

    let upstream = FakeUpstream::start(|request: Value| async move {
        match (
            upstream::model_of(&request),
            upstream::has_tool_result(&request),
        ) {
            ("master", false) => Reply::tool_call(
                "run_machine_task",
                json!({"tasks": [{"agent_name": "worker", "prompt": "hi"}], "wait": false}),
            ),
            ("master", true) => Reply::Text("submitted".to_string()),
            _ => Reply::Text("worker done".to_string()),
        }
    })
    .await;

    install_team(
        env,
        "t_b_team",
        &upstream,
        &[
            fixture("@master", "master", &["run_machine_task"]),
            fixture("worker", "worker", &[]),
        ],
    );
    let ws = env.open_space("t_b_space", "t_b_team").await;

    let _turn = start_turn(env, &ws, "@master", "hi").await;
    let row = wait_for_row(env, &ws, "run_machine_task").await;

    assert_eq!(row.origin_ws_id, ws, "同空间异步任务的发起方就是本空间");
    assert_eq!(row.target_ws_id, ws, "任务落在发起方自己的 Hub 里");
    assert_eq!(row.agent_name, "worker", "agent_name 取自结果条目");
    assert_eq!(
        row.prompt_digest.as_deref(),
        Some("hi"),
        "同空间任务的描述来自入参 `tasks[i].prompt`，不是结果里的回显"
    );
    assert_active_or_terminal(&row);
    print_exchanges("T-B", &upstream);
}

// ============================================================================
// T-C：关闭目标空间 → orphan（V6-8）
// ============================================================================

/// 变异方法：把 `space/close.rs` 的 `mark_orphan` 去掉（force 时只统计不落库）
/// ⇒ 台账停在非终态，本用例的 `orphan` 断言变红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_the_target_space_orphans_the_ledger_row() {
    let env = shared().await;
    let target_dir = env.work_dir("t_c_target");

    let origin_upstream = {
        let target = target_dir.display().to_string();
        FakeUpstream::start(move |request: Value| {
            let target = target.clone();
            async move {
                if upstream::has_tool_result(&request) {
                    Reply::Text("dispatched".to_string())
                } else {
                    Reply::tool_call(
                        "dispatch_to_workspace",
                        json!({"dir": target, "prompt": "hi", "wait": false}),
                    )
                }
            }
        })
        .await
    };
    // 目标空间的 @master 卡住不回：任务必须一直是非终态，否则无法判定 orphan。
    let gate = Arc::new(tokio::sync::Notify::new());
    let target_upstream = {
        let gate = Arc::clone(&gate);
        FakeUpstream::start(move |_: Value| {
            let gate = Arc::clone(&gate);
            async move {
                gate.notified().await;
                Reply::Text("late".to_string())
            }
        })
        .await
    };

    install_team(
        env,
        "t_c_origin",
        &origin_upstream,
        &[fixture("@master", "master", &["dispatch_to_workspace"])],
    );
    install_team(
        env,
        "t_c_target",
        &target_upstream,
        &[fixture("@master", "target", &[])],
    );

    let ws_a = env.open_space("t_c_origin", "t_c_origin").await;
    let ws_b = env.open_space("t_c_target", "t_c_target").await;

    let _turn = start_turn(env, &ws_a, "@master", "hi").await;
    let row = wait_for_row(env, &ws_b, "dispatch_to_workspace").await;
    assert!(
        TASK_ACTIVE_STATES.contains(&row.task_state.as_str()),
        "关闭前这条台账必须还在飞，实际 {}",
        row.task_state
    );

    let closed = env
        .post(&format!("/api/spaces/{ws_b}/close"), json!({"force": true}))
        .await;
    assert_eq!(closed.status, 200, "关闭失败: {}", closed.body);

    let (task_state, wake_state) = env.task_and_wake_state(&ws_b, &row.task_id).await;
    assert_eq!(task_state, "orphan", "空间被关闭后这条台账必须是 orphan");
    assert_eq!(
        wake_state, "skipped",
        "孤儿行不得再唤醒（结果已随 Hub 消失）"
    );
    // 这行注册时就因来源不是 Peco 而 skipped；关闭只补任务态，不覆盖更早的成因。
    assert_eq!(
        env.wake_skip_reason(&ws_b, &row.task_id).await.as_deref(),
        Some("not_peco_origin"),
        "已跳过的行保留更早的原因，不被 space_closed 顶掉"
    );

    // 放行那个卡住的回复，避免后台任务泄漏到下一个用例。
    gate.notify_one();
}

// ============================================================================
// T-D：启动对账（V6-9）
// ============================================================================

/// 目标空间没恢复：新进程的 Hub 里没有这个任务，结果已随上个进程消失。
///
/// 变异方法：`reconcile_row` 里去掉 `hub.get` 的 `None` 分支（或整段不扫）
/// ⇒ 行停在 `working`，断言变红。
#[tokio::test]
async fn reconcile_orphans_rows_whose_target_space_did_not_come_back() {
    let env = support::new_env(&[]).await;
    install_bare_team(&env, "t_d1_team");
    let dir = env.work_dir("t_d1_space");
    let ws_id = env
        .resolve(&dir, "t_d1_team")
        .await
        .expect("index the space");
    assert!(!env.is_open(&ws_id).await, "本用例的前提是目标空间没有实例");

    let task_id = Uuid::new_v4().to_string();
    insert_row(&env, &ws_id, &dir, &task_id, "working").await;

    let report = reconcile_ledger(&env.app_state()).await;
    assert_eq!(
        report,
        ReconcileReport {
            scanned: 1,
            finished: 0,
            orphaned: 1,
            resubscribed: 0,
        }
    );
    let (task_state, wake_state) = env.task_and_wake_state(&ws_id, &task_id).await;
    assert_eq!(task_state, "orphan");
    // 重启对账与运行期失联的差别就在这里：这条行还没被通知过，仍要投一次唤醒
    // 去让 Peco 复核，所以唤醒态保持 pending，跳过原因也就没有。
    assert_eq!(wake_state, "pending", "重启对账的孤儿仍留在待投递队列");
    assert_eq!(
        env.wake_skip_reason(&ws_id, &task_id).await,
        None,
        "没跳过的行不写原因"
    );
}

/// 目标空间恢复了、但 Hub 里没有这个任务（上个进程的 `InMemoryStore` 已空）。
///
/// 变异方法：`NotFound` 分支改成「保持原状」⇒ 行停在 `working`，断言变红。
#[tokio::test]
async fn reconcile_orphans_rows_the_restored_hub_does_not_know() {
    let env = support::new_env(&["t_d2_team"]).await;
    let ws_id = env.open_space("t_d2_space", "t_d2_team").await;
    let dir = env.work_dir_of(&ws_id).await;

    let task_id = Uuid::new_v4().to_string();
    insert_row(&env, &ws_id, &dir, &task_id, "submitted").await;

    let report = reconcile_ledger(&env.app_state()).await;
    assert_eq!(
        report,
        ReconcileReport {
            scanned: 1,
            finished: 0,
            orphaned: 1,
            resubscribed: 0,
        }
    );
    assert_eq!(env.task_and_wake_state(&ws_id, &task_id).await.0, "orphan");
}

/// 目标空间恢复了、任务仍在跑：**重新订阅**，状态不变。
///
/// 变异方法：把 `Ok(_)`（非终态快照）也接到 orphan 上 ⇒ 状态被改写成 `orphan`
/// 且 `resubscribed` 为 0，两条断言同时变红。
#[tokio::test]
async fn reconcile_resubscribes_rows_that_are_still_running() {
    let env = support::new_env(&["t_d3_team"]).await;
    let gate = Arc::new(tokio::sync::Notify::new());
    let upstream = {
        let gate = Arc::clone(&gate);
        FakeUpstream::start(move |_: Value| {
            let gate = Arc::clone(&gate);
            async move {
                gate.notified().await;
                Reply::Text("late".to_string())
            }
        })
        .await
    };
    install_team(
        &env,
        "t_d3_team",
        &upstream,
        &[
            fixture("@master", "master", &[]),
            fixture("worker", "worker", &[]),
        ],
    );
    let ws_id = env.open_space("t_d3_space", "t_d3_team").await;
    let dir = env.work_dir_of(&ws_id).await;

    // 真提交一个卡住的 turn：对账看到的必须是「正在跑的任务」，不是假快照。
    let handle = env.hub.get(&dir).expect("the space is open");
    let workspace = handle.workspace();
    let runtime = workspace
        .machine_runtime()
        .upgrade()
        .expect("the runtime outlives the workspace handle");
    let kind = runtime
        .ensure_machine("worker", || workspace.agent_manager().load_cached("worker"))
        .expect("register the worker machine");
    let hub = runtime.hub().expect("the task hub");
    let task_id = hub
        .submit(&kind, prompt_task())
        .await
        .expect("submit the gated task");
    wait_for_working(&hub, &task_id).await;

    insert_row(&env, &ws_id, &dir, &task_id.to_string(), "working").await;

    let report = reconcile_ledger(&env.app_state()).await;
    assert_eq!(
        report,
        ReconcileReport {
            scanned: 1,
            finished: 0,
            orphaned: 0,
            resubscribed: 1,
        },
        "仍在跑的任务必须重新订阅，不能 orphan"
    );
    assert_eq!(
        env.task_and_wake_state(&ws_id, &task_id.to_string())
            .await
            .0,
        "working",
        "重新订阅不得改写状态"
    );

    gate.notify_one();
}

// ============================================================================
// T-D / T-A / T-B 的共用辅助
// ============================================================================

/// 写一条在飞台账行。`target_work_dir` 必须是真实目录：对账靠它找 Hub 实例。
async fn insert_row(
    env: &TestEnv,
    ws_id: &str,
    dir: &std::path::Path,
    task_id: &str,
    task_state: &str,
) {
    let id = format!("d_{task_id}");
    dispatches::insert(
        &env.pool,
        &NewDispatch {
            id: &id,
            task_id,
            target_ws_id: ws_id,
            target_work_dir: &dir.to_string_lossy(),
            target_team_id: "t_d_team",
            origin_ws_id: "ws_origin",
            agent_name: "@master",
            origin_kind: "dispatch_to_workspace",
            task_state,
            wake_state: WakeState::Pending,
            wake_skip_reason: None,
            prompt_digest: None,
            dispatch_id: None,
        },
    )
    .await
    .expect("insert a ledger row");
}

fn prompt_task() -> Task {
    Task {
        // Hub 会覆盖 id 与 kind。
        id: TaskId(Uuid::nil()),
        kind: String::new(),
        input: TaggedJson {
            tag: "prompt".to_string(),
            payload: Value::String("hi".to_string()),
        },
        context: TaskContext {
            scope: None,
            data: None,
        },
        source: TaskSource::External {
            principal: "tester".to_string(),
        },
        deadline: None,
        labels: BTreeMap::new(),
        created_at: Utc::now(),
    }
}

async fn wait_for_working(hub: &Arc<peco_core::machine::TaskHub>, id: &TaskId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match hub.snapshot(id).await {
            Ok(snapshot) if snapshot.state == TaskState::Working => return,
            Ok(snapshot) if snapshot.state.is_terminal() => {
                panic!("the gated task became terminal: {:?}", snapshot.state)
            }
            _ => {}
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the gated task never reached Working"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
