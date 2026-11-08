//! P4 唤醒闭环的投递用例：认领之后真的把一条 `[peco-wake]` 送进 Peco 会话并跑完一轮。
//!
//! 每个用例自带一个真实空间与一份指向假上游的 `providers.toml`，走的是生产同一条
//! `chat::run` 执行链；差别只在没有 SSE 客户端（`RunSink::Headless`）。

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use peco_core::agent::hooks::{DispatchObservation, DispatchOrigin, TaskObservationSink};
use peco_core::machine::{OutputView, Progress, TaskId, TaskSnapshot, TaskSource, TaskState};
use peco_daemon::chat::ChatActiveRuns;
use peco_daemon::db::dispatches;
use peco_daemon::space::peco::PecoIdentity;
use peco_daemon::space::tasks::TaskObserver;
use peco_daemon::wake::deliver::PecoDelivery;
use peco_daemon::wake::pump::WakePump;
use peco_daemon::wake::{RETRY_BACKOFF, TICK};
use serde_json::{Value, json};
use support::upstream::{FakeUpstream, Reply, has_tool_result};
use support::{TestEnv, fixture, install_team, new_env};
use uuid::Uuid;

/// 唤醒轮的等待上限。
///
/// 必须小于 [`TICK`]：否则「通知没发出去」会被定时 tick 悄悄兜住，用例在
/// 接线断掉时照样绿。
const WAKE_DEADLINE: Duration = Duration::from_secs(4);

// ============================================================================
// 夹具
// ============================================================================

/// 建出 Peco 空间（含 `@master` 的 agent.md）并打开它，返回 `ws_id`。
async fn open_peco_space(env: &TestEnv, upstream: &FakeUpstream) -> String {
    open_peco_space_with(env, upstream, &[]).await
}

/// 同上，但让 `@master` 挂上指定工具。
///
/// 走引导路径（索引行 + 打开）而不是 `POST /api/spaces`：保留团队不接受经接口
/// 新建空间，真实的 Peco 空间也是启动引导建出来的。
async fn open_peco_space_with(env: &TestEnv, upstream: &FakeUpstream, tools: &[&str]) -> String {
    install_team(
        env,
        "peco",
        upstream,
        &[fixture("@master", "master", tools)],
    );
    let ws_id = env
        .resolve(&env.work_dir("peco"), "peco")
        .await
        .expect("引导路径应能为 Peco 团队建出空间行");
    env.open(&ws_id).await.expect("Peco 空间应能打开");
    ws_id
}

/// 与台账里的空间对上的 Peco 身份：投递端只按它决定「投给谁、去哪儿开实例」。
async fn peco_identity(env: &TestEnv, ws_id: &str) -> Arc<PecoIdentity> {
    Arc::new(PecoIdentity {
        ws_id: ws_id.to_string(),
        work_dir: env.work_dir_of(ws_id).await,
        team_id: "peco".to_string(),
        from_marker: true,
    })
}

/// 把泵与投递端接起来，返回两者；`chat_runs` 交给调用方是为了能预先「占住会话」。
fn wired_pump(env: &TestEnv, peco: Arc<PecoIdentity>) -> (Arc<WakePump>, Arc<ChatActiveRuns>) {
    let chat_runs = Arc::new(ChatActiveRuns::new());
    let pump = Arc::new(WakePump::new(env.pool.clone(), peco.clone()));
    pump.attach_delivery(Arc::new(PecoDelivery::new(
        env.pool.clone(),
        env.spaces(),
        chat_runs.clone(),
        peco,
        Arc::downgrade(&env.env),
    )));
    (pump, chat_runs)
}

/// 观察者要叫醒的是同一个泵：接线漏了，终态事件就到不了投递侧。
fn wired_observer(env: &TestEnv, pump: &Arc<WakePump>, peco: &Arc<PecoIdentity>) -> TaskObserver {
    let tasks = TaskObserver::new(env.pool.clone(), pump.clone());
    tasks.attach_peco(peco.clone());
    tasks
}

fn observation(task_id: TaskId, origin_ws_id: &str) -> DispatchObservation {
    DispatchObservation {
        origin: DispatchOrigin {
            ws_id: origin_ws_id.into(),
            principal: "local".into(),
            origin_dir: env_origin_dir(),
        },
        target_dir: env_origin_dir(),
        task_id,
        dispatch_id: None,
        origin_kind: "run_machine_task".into(),
        agent_name: "@master".into(),
        prompt_digest: None,
    }
}

fn env_origin_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("/tmp/peco-origin")
}

fn completed_snapshot(task_id: TaskId) -> TaskSnapshot {
    TaskSnapshot {
        id: task_id,
        kind: "agent".into(),
        state: TaskState::Completed,
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
        scope: Some("peco".into()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        last_seq: 0,
    }
}

// ============================================================================
// 台账 / 消息断言
// ============================================================================

struct LedgerRow {
    wake_state: String,
    delivered_at: Option<String>,
    wake_attempts: i64,
    wake_claimed_at: Option<String>,
}

impl std::fmt::Debug for LedgerRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "wake_state={} delivered_at={:?} attempts={} claimed_at={:?}",
            self.wake_state, self.delivered_at, self.wake_attempts, self.wake_claimed_at
        )
    }
}

async fn ledger_row(env: &TestEnv, task_id: &str) -> LedgerRow {
    let (wake_state, delivered_at, wake_attempts, wake_claimed_at) =
        sqlx::query_as::<_, (String, Option<String>, i64, Option<String>)>(
            "SELECT wake_state, delivered_at, wake_attempts, wake_claimed_at \
             FROM d_dispatches WHERE task_id = ?",
        )
        .bind(task_id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    LedgerRow {
        wake_state,
        delivered_at,
        wake_attempts,
        wake_claimed_at,
    }
}

async fn message_contents(env: &TestEnv, role: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT content FROM messages WHERE role = ? ORDER BY created_at ASC, id ASC",
    )
    .bind(role)
    .fetch_all(&env.pool)
    .await
    .unwrap()
}

async fn message_count(env: &TestEnv) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM messages")
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

async fn wait_for_delivered(env: &TestEnv, task_id: &str) -> LedgerRow {
    let deadline = Instant::now() + WAKE_DEADLINE;
    loop {
        let row = ledger_row(env, task_id).await;
        if row.wake_state == "delivered" {
            return row;
        }
        assert!(
            Instant::now() < deadline,
            "{WAKE_DEADLINE:?} 内必须送达（只等通知，不等定时器），实际 {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_message(env: &TestEnv, role: &str) -> String {
    let deadline = Instant::now() + WAKE_DEADLINE;
    loop {
        if let Some(content) = message_contents(env, role).await.into_iter().next() {
            return content;
        }
        assert!(
            Instant::now() < deadline,
            "{WAKE_DEADLINE:?} 内必须出现 role={role} 的消息"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn text_reply(text: &str) -> impl Fn(Value) -> std::future::Ready<Reply> + Send + Sync + 'static {
    let text = text.to_string();
    move |_: Value| std::future::ready(Reply::Text(text.clone()))
}

/// 打印整轮的模型往返，供人工核对「真的跑了一轮」。
fn print_exchanges(label: &str, upstream: &FakeUpstream) {
    for (index, exchange) in upstream.exchanges().iter().enumerate() {
        eprintln!(
            "[{label}] #{index} request={} response={}",
            exchange.request, exchange.response
        );
    }
}

// ============================================================================
// W1：终态通知 → 真的跑完一轮
// ============================================================================

/// 变异方法：`space/tasks.rs::on_terminal` 去掉 `self.wake.notify_one()` ⇒ 泵收不到
/// 通知，[`WAKE_DEADLINE`] 的等待上限等不到任何东西（定时 tick 是 5s，兜不住这个断言）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn w1_a_finished_task_wakes_peco_and_runs_a_real_turn() {
    assert!(
        WAKE_DEADLINE < TICK,
        "等待上限必须小于 tick 间隔，否则缺通知时定时 tick 会假绿"
    );

    let env = new_env(&[]).await;
    let upstream = FakeUpstream::start(text_reply("收到，我去核对这条任务")).await;
    let ws_id = open_peco_space(&env, &upstream).await;
    let peco = peco_identity(&env, &ws_id).await;
    let (pump, _chat_runs) = wired_pump(&env, peco.clone());
    let tasks = wired_observer(&env, &pump, &peco);

    let task_id = TaskId(Uuid::new_v4());
    env.peco_pending_row(1, &task_id.to_string(), &ws_id).await;

    pump.spawn();
    tasks
        .on_terminal(&observation(task_id, &ws_id), &completed_snapshot(task_id))
        .await;

    let row = wait_for_delivered(&env, &task_id.to_string()).await;
    let user = wait_for_message(&env, "user").await;
    let assistant = wait_for_message(&env, "assistant").await;

    eprintln!(
        "[W1] d_dispatches: task_id={task_id} wake_state={} delivered_at={:?} wake_attempts={}",
        row.wake_state, row.delivered_at, row.wake_attempts
    );
    // 转义后打印：唤醒文本是多行的，原样打印会把「一条消息」在报告里拆成好几行。
    eprintln!("[W1] messages role=user: {user:?}");
    eprintln!("[W1] messages role=assistant: {assistant}");

    assert!(row.delivered_at.is_some(), "送达必须留时间戳：{row:?}");
    assert!(
        user.starts_with("[peco-wake]"),
        "唤醒消息以固定前缀开头：{user}"
    );
    assert!(
        user.contains(&task_id.to_string()),
        "消息里要给完整 task_id，Peco 才能回台账复核：{user}"
    );
    assert!(!assistant.trim().is_empty(), "agent 必须真的回了一句话");

    // 模型确实收到了这条文本 —— 落库只能证明「写了消息」，这件事证明真的跑了一轮。
    let exchanges = upstream.exchanges();
    assert_eq!(exchanges.len(), 1, "唤醒轮只该问模型一次");
    let saw_wake_text = exchanges[0].request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|message| {
            message["role"] == json!("user")
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.starts_with("[peco-wake]"))
        });
    assert!(saw_wake_text, "模型必须真的收到 [peco-wake] 文本");
    print_exchanges("W1", &upstream);
}

// ============================================================================
// W14：无客户端的一轮忽略 Pause，且不写会话标题
// ============================================================================

/// 无客户端的运行没有断连信号，也没有人能发 Resume：真去 pause 会把 looper 与
/// 守卫一起钉在暂停态，该会话此后的唤醒永远让路。
///
/// 变异方法：把 `chat/run.rs` 里 headless 分支的 Pause 改回 `handle.pause()` ⇒
/// 第一次模型往返期间的 Pause 落在工具批之前的挂起点，looper 不再发 Idle，
/// 本用例在「等 assistant 消息」处超时变红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn w14_a_headless_run_ignores_pause_and_still_finishes() {
    let env = new_env(&[]).await;
    // 首轮工具调用的回复压 300ms：给出「运行中收到 Pause」的窗口。
    let upstream = FakeUpstream::start(|request: Value| async move {
        if has_tool_result(&request) {
            return Reply::Text("核对完成".to_string());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        Reply::tool_call("list_tools", json!({}))
    })
    .await;
    let ws_id = open_peco_space_with(&env, &upstream, &["list_tools"]).await;
    let peco = peco_identity(&env, &ws_id).await;
    let (pump, chat_runs) = wired_pump(&env, peco);
    env.peco_pending_row(1, "t-pause", &ws_id).await;

    pump.tick().await;

    let conv_id = wake_conversation_id(&env, &ws_id).await;
    assert!(
        chat_runs.pause(&conv_id),
        "唤醒轮必须已注册（否则这条用例没有测到任何东西）"
    );

    let assistant = wait_for_message(&env, "assistant").await;
    assert!(
        !assistant.trim().is_empty(),
        "收到 Pause 也必须跑到这一轮结束"
    );
    // assistant 行落库早于这一轮收尾：守卫要等 spawn 出的任务结束才 drop，
    // 「消息已经看得见」不等于「条目已经清掉」，故等条目真的消失再断言。
    let deadline = Instant::now() + WAKE_DEADLINE;
    while chat_runs.is_active(&conv_id) {
        if Instant::now() >= deadline {
            panic!(
                "{WAKE_DEADLINE:?} 内这一轮必须结束，但会话 {conv_id} 仍是 active：\
                 结束之后守卫必须 drop，否则该会话的唤醒队列会被永久堵死"
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !chat_runs.pause(&conv_id),
        "运行结束后守卫必须 drop，否则该会话的唤醒队列会被永久堵死"
    );
    let title: String = sqlx::query_scalar("SELECT title FROM conversations WHERE id = ?")
        .bind(&conv_id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(
        title, "后台任务",
        "唤醒轮没有用户，不得把标题改写成 [peco-wake] 开头"
    );
}

/// 唤醒投递建的会话 id：投递端每轮都按它定位目标会话。
async fn wake_conversation_id(env: &TestEnv, ws_id: &str) -> String {
    sqlx::query_scalar(
        "SELECT id FROM conversations WHERE workspace_id = ? AND agent_name = '@master' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(ws_id)
    .fetch_one(&env.pool)
    .await
    .unwrap()
}

// ============================================================================
// W9：重放不产生第二条消息
// ============================================================================

/// 变异方法：`db/dispatches.rs::claim` 删掉 `AND wake_state = 'pending'` ⇒
/// 已送达的行还能被再次认领（本用例的 `claim` 断言变红）。两条 tick 本身由
/// `list_pending` 的谓词挡住，故幂等的第二道防线必须单独断一次。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn w9_replaying_the_same_row_delivers_one_message() {
    let env = new_env(&[]).await;
    let upstream = FakeUpstream::start(text_reply("收到")).await;
    let ws_id = open_peco_space(&env, &upstream).await;
    let peco = peco_identity(&env, &ws_id).await;
    let (pump, _chat_runs) = wired_pump(&env, peco);

    env.peco_pending_row(1, "t-once", &ws_id).await;

    pump.tick().await;
    let user = wait_for_message(&env, "user").await;
    assert!(user.starts_with("[peco-wake]"), "{user}");

    assert_eq!(
        dispatches::claim(&env.pool, "d_t-once", RETRY_BACKOFF)
            .await
            .unwrap(),
        0,
        "已送达的行不得再被认领"
    );

    pump.tick().await;
    let _ = wait_for_message(&env, "assistant").await;

    assert_eq!(
        message_contents(&env, "user").await.len(),
        1,
        "只该有一条唤醒消息"
    );
    let row = ledger_row(&env, "t-once").await;
    assert_eq!(row.wake_state, "delivered");
    assert_eq!(row.wake_attempts, 1, "两次 tick 也只算一次尝试");
}

// ============================================================================
// W11：目标空间没打开 → 显式失败
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn w11_a_closed_target_space_marks_the_row_failed() {
    let env = new_env(&["peco"]).await;
    // 只索引、不打开：`SpaceRegistry` 拿不到实例，`space_not_open` 是投递的失败面。
    let dir = env.work_dir("closed_peco");
    let ws_id = env.resolve(&dir, "peco").await.unwrap();
    let peco = peco_identity(&env, &ws_id).await;
    let (pump, _chat_runs) = wired_pump(&env, peco);

    env.peco_pending_row(1, "t-closed", &ws_id).await;

    pump.tick().await;

    let row = ledger_row(&env, "t-closed").await;
    assert_eq!(row.wake_state, "failed", "投不出去要显形：{row:?}");
    assert_eq!(row.wake_attempts, 1, "失败的尝试要计数");
    assert_eq!(message_count(&env).await, 0, "失败时不得落一条假消息");
}

// ============================================================================
// Busy：用户在看着这个会话
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn busy_sessions_leave_the_row_pending_without_an_attempt() {
    let env = new_env(&[]).await;
    let upstream = FakeUpstream::start(text_reply("不该被问到")).await;
    let ws_id = open_peco_space(&env, &upstream).await;
    let peco = peco_identity(&env, &ws_id).await;
    let (pump, chat_runs) = wired_pump(&env, peco);

    let created = env
        .post_in(&ws_id, "/api/chat/@master/conversations", json!({}))
        .await;
    assert_eq!(created.status, 201, "建会话失败: {}", created.body);
    let conv_id = created.body["id"].as_str().unwrap().to_string();
    // 用户正开着这个会话：唤醒必须让路，而且要整轮让路（不认领）。
    let (_control_rx, _guard) = chat_runs.register(&conv_id);

    env.peco_pending_row(1, "t-busy", &ws_id).await;
    pump.tick().await;

    let row = ledger_row(&env, "t-busy").await;
    assert_eq!(row.wake_state, "pending", "忙不是一次失败的投递：{row:?}");
    assert_eq!(row.wake_attempts, 0, "没认领就不该计尝试");
    assert_eq!(row.wake_claimed_at, None);
    assert_eq!(message_count(&env).await, 0, "让路时一条消息都不该产生");
    assert!(upstream.exchanges().is_empty(), "让路时连模型都不该被问到");
}

// ============================================================================
// 启动回收
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_reclaim_returns_leftover_deliveries_to_pending() {
    let env = new_env(&[]).await;
    let peco = Arc::new(PecoIdentity {
        ws_id: "ws_peco".to_string(),
        work_dir: env.home.join("spaces").join("peco").join("work"),
        team_id: "peco".to_string(),
        from_marker: true,
    });
    let pump = WakePump::new(env.pool.clone(), peco);

    env.peco_pending_row(1, "t-leftover", "ws_peco").await;
    sqlx::query(
        "UPDATE d_dispatches SET wake_state = 'delivering', wake_attempts = 1, \
         wake_claimed_at = datetime('now', '-30 seconds') WHERE task_id = 't-leftover'",
    )
    .execute(&env.pool)
    .await
    .unwrap();

    pump.startup_reclaim().await;

    let row = ledger_row(&env, "t-leftover").await;
    assert_eq!(row.wake_state, "pending", "上个进程留下的认领要退回队列");
    assert_eq!(row.wake_claimed_at, None);
    assert_eq!(row.wake_attempts, 1, "回收不改尝试次数");
}
