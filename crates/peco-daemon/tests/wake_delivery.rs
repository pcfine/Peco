//! 唤醒闭环的投递用例：认领之后真的把一条 `[peco-wake]` 送进派发方会话并跑完一轮。
//!
//! 每个用例自带一个真实空间与一份指向假上游的 `providers.toml`，走的是生产同一条
//! `chat::run` 执行链；差别只在没有 SSE 客户端（`RunSink::Headless`）。

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request};
use chrono::Utc;
use futures::StreamExt;
use peco_core::agent::hooks::{DispatchObservation, DispatchOrigin, TaskObservationSink};
use peco_core::machine::{OutputView, Progress, TaskId, TaskSnapshot, TaskSource, TaskState};
use peco_daemon::chat::ChatActiveRuns;
use peco_daemon::db::conversations::{self, CreateConversationParams};
use peco_daemon::db::dispatches;
use peco_daemon::space::peco::PecoIdentity;
use peco_daemon::space::tasks::TaskObserver;
use peco_daemon::state::AppState;
use peco_daemon::wake::deliver::WakeDelivery;
use peco_daemon::wake::pump::WakePump;
use peco_daemon::wake::{RETRY_BACKOFF, TICK};
use serde_json::{Value, json};
use support::upstream::{FakeUpstream, Reply, has_tool_result};
use support::{TestEnv, fixture, install_team, new_env};
use tokio::time::timeout;
use tower::ServiceExt;
use uuid::Uuid;

/// 唤醒轮的等待上限。
///
/// 必须小于 [`TICK`]：否则「通知没发出去」会被定时 tick 悄悄兜住，用例在
/// 接线断掉时照样绿。
const WAKE_DEADLINE: Duration = Duration::from_secs(4);

/// 一轮 SSE 对话的等待上限：够一次假上游往返，卡住就说明流根本没打算收尾。
const STREAM_DEADLINE: Duration = Duration::from_secs(10);

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
    let pump = Arc::new(WakePump::new(
        env.pool.clone(),
        peco.clone(),
        env.shutdown.clone(),
    ));
    pump.attach_delivery(Arc::new(WakeDelivery::new(
        env.pool.clone(),
        env.spaces(),
        chat_runs.clone(),
        Arc::downgrade(&env.env),
    )));
    (pump, chat_runs)
}

/// 观察者要叫醒的是同一个泵：接线漏了，终态事件就到不了投递侧。
fn wired_observer(env: &TestEnv, pump: &Arc<WakePump>) -> TaskObserver {
    TaskObserver::new(env.pool.clone(), pump.clone())
}

fn observation(task_id: TaskId, origin_ws_id: &str) -> DispatchObservation {
    DispatchObservation {
        origin: DispatchOrigin {
            ws_id: origin_ws_id.into(),
            principal: "local".into(),
            origin_dir: env_origin_dir(),
            conversation_id: None,
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

/// 注册那一刻的快照：任务刚提交，还没有终态也没有摘要。
fn working_snapshot(task_id: TaskId) -> TaskSnapshot {
    TaskSnapshot {
        state: TaskState::Working,
        ..completed_snapshot(task_id)
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
    let limit = if role == "user" {
        WAKE_DEADLINE
    } else {
        STREAM_DEADLINE
    };
    let deadline = Instant::now() + limit;
    loop {
        if let Some(content) = message_contents(env, role).await.into_iter().next() {
            return content;
        }
        assert!(
            Instant::now() < deadline,
            "{limit:?} 内必须出现 role={role} 的消息"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn seed_pending_for(
    env: &TestEnv,
    seq: i64,
    task_id: &str,
    origin_ws_id: &str,
    conversation_id: &str,
) {
    sqlx::query(
        "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, \
         target_team_id, origin_ws_id, origin_conversation_id, agent_name, origin_kind, \
         task_state, wake_state, result_digest, created_at) \
         VALUES (?, ?, ?, '/tmp/work', 'research', ?, ?, '@master', 'run_machine_task', \
         'completed', 'pending', '重构完成，12 个测试通过', ?)",
    )
    .bind(format!("d_{task_id}"))
    .bind(task_id)
    .bind(origin_ws_id)
    .bind(origin_ws_id)
    .bind(conversation_id)
    .bind(format!("2026-01-01 00:00:{seq:02}"))
    .execute(&env.pool)
    .await
    .unwrap();
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
    let tasks = wired_observer(&env, &pump);

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
    // 只数对话那档模型：记忆提取跑在后台、用的是另一档模型，与本判据无关。
    let exchanges = upstream.exchanges_with_model("master");
    assert_eq!(exchanges.len(), 1, "唤醒轮只该问对话模型一次");
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
// 谁派发谁接收：非 Peco 空间的会话也要收到通知
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_finished_task_returns_to_the_dispatching_conversation() {
    let env = new_env(&[]).await;
    let upstream = FakeUpstream::start(text_reply("收到，结果已核对")).await;
    install_team(
        &env,
        "research",
        &upstream,
        &[fixture("@master", "master", &[])],
    );
    let dir = env.work_dir("research_space");
    let ws_id = env.resolve(&dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();

    let conv_id = "c_origin_chat".to_string();
    conversations::insert(
        &env.pool,
        &CreateConversationParams {
            id: conv_id.clone(),
            workspace_id: ws_id.clone(),
            agent_id: None,
            agent_name: "@master".to_string(),
            title: "派发前的那段对话".to_string(),
        },
    )
    .await
    .unwrap();

    let peco = peco_identity(&env, &ws_id).await;
    let (pump, _chat_runs) = wired_pump(&env, peco);
    seed_pending_for(&env, 1, "t-origin", &ws_id, &conv_id).await;

    pump.tick().await;

    let row = wait_for_delivered(&env, "t-origin").await;
    assert!(row.delivered_at.is_some(), "送达必须留时间戳：{row:?}");

    let deadline = Instant::now() + WAKE_DEADLINE;
    let user = loop {
        let found: Option<String> = sqlx::query_scalar(
            "SELECT content FROM messages WHERE role = 'user' AND conversation_id = ? LIMIT 1",
        )
        .bind(&conv_id)
        .fetch_optional(&env.pool)
        .await
        .unwrap();
        if let Some(content) = found {
            break content;
        }
        assert!(
            Instant::now() < deadline,
            "{WAKE_DEADLINE:?} 内发起会话必须收到唤醒消息"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(user.starts_with("[peco-wake]"), "{user}");
    assert!(user.contains("t-origin"), "要带完整 task_id：{user}");

    let deadline = Instant::now() + STREAM_DEADLINE;
    let assistant = loop {
        let found: Option<String> = sqlx::query_scalar(
            "SELECT content FROM messages WHERE role = 'assistant' AND conversation_id = ? LIMIT 1",
        )
        .bind(&conv_id)
        .fetch_optional(&env.pool)
        .await
        .unwrap();
        if let Some(content) = found {
            break content;
        }
        assert!(
            Instant::now() < deadline,
            "{STREAM_DEADLINE:?} 内发起会话必须收到 agent 的回复"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(assistant, "收到，结果已核对");
}

// ============================================================================
// A1：注册不唤醒，终态才唤醒
// ============================================================================

/// 注册那一刻只登记 `awaiting`，一条消息都不该投出去；终态落库后才入队，投出
/// 去的那条要带终态摘要。
///
/// 变异：`space/tasks.rs::on_registered` 写回 `pending` ⇒ 注册后的第一次 tick
/// 就把一条 `state=working` 的唤醒投出去（`message_count` 由 0 变 1），第二条
/// 断言红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registering_alone_never_wakes_and_the_terminal_state_does() {
    let env = new_env(&[]).await;
    let upstream = FakeUpstream::start(text_reply("收到，我去核对这条任务")).await;
    let ws_id = open_peco_space(&env, &upstream).await;
    let peco = peco_identity(&env, &ws_id).await;
    let (pump, _chat_runs) = wired_pump(&env, peco.clone());
    let tasks = wired_observer(&env, &pump);

    let task_id = TaskId(Uuid::new_v4());
    let mut obs = observation(task_id, &ws_id);
    // 注册要落台账，观察的目标目录必须是真实空间目录。
    obs.target_dir = env.work_dir_of(&ws_id).await;
    tasks.on_registered(&obs, &working_snapshot(task_id)).await;

    assert_eq!(
        ledger_row(&env, &task_id.to_string()).await.wake_state,
        "awaiting",
        "注册时没有可投的内容"
    );
    pump.tick().await;
    assert_eq!(message_count(&env).await, 0, "注册不得投出任何唤醒");
    assert!(upstream.exchanges().is_empty(), "注册时连模型都不该被问到");

    tasks.on_terminal(&obs, &completed_snapshot(task_id)).await;
    pump.tick().await;

    let user = wait_for_message(&env, "user").await;
    assert!(user.starts_with("[peco-wake]"), "{user}");
    assert!(
        user.contains(&task_id.to_string()),
        "唤醒要带完整 task_id，Peco 才能回台账复核：{user}"
    );
    let row = wait_for_delivered(&env, &task_id.to_string()).await;
    assert!(row.delivered_at.is_some(), "送达必须留时间戳：{row:?}");
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
    let deadline = Instant::now() + STREAM_DEADLINE;
    while chat_runs.is_active(&conv_id) {
        if Instant::now() >= deadline {
            panic!(
                "{STREAM_DEADLINE:?} 内这一轮必须结束，但会话 {conv_id} 仍是 active：\
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
        title, "空间对话",
        "唤醒轮没有用户，不得把标题改写成 [peco-wake] 开头 —— 它落在空间会话里，标题是那条会话本来就有的"
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
// 送达信号：唤醒轮结束 → 广播 `wake_turn_done`
// ============================================================================

/// 唤醒轮是 headless 写入，前端不看事件流就永远不知道有这条消息；一轮结束时必须
/// 广播一条带 `conversation_id` 的事件，前端据此失效消息缓存。
///
/// 变异：去掉 `chat/run.rs` 里的 Drop 守卫（或在循环末尾才调用）⇒ 事件不来，
/// 本用例在等待处超时变红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_finished_wake_turn_broadcasts_a_wake_turn_done_event() {
    let env = new_env(&[]).await;
    let upstream = FakeUpstream::start(text_reply("收到")).await;
    let ws_id = open_peco_space(&env, &upstream).await;
    let peco = peco_identity(&env, &ws_id).await;
    let (pump, _chat_runs) = wired_pump(&env, peco);

    // 先订阅再投递：广播没有历史，晚一步就漏掉这条事件。
    let mut events = env.tasks.subscribe();
    env.peco_pending_row(1, "t-signal", &ws_id).await;
    pump.tick().await;

    let event = loop {
        let received = timeout(STREAM_DEADLINE, events.recv())
            .await
            .expect("唤醒轮结束后必须有事件");
        let event = received.expect("广播通道不得关闭");
        // 台账事件也会走同一个通道：这里只认唤醒轮结束那一条。
        if event.kind == "wake_turn_done" {
            break event;
        }
    };

    let conv_id = wake_conversation_id(&env, &ws_id).await;
    assert_eq!(event.conversation_id.as_deref(), Some(conv_id.as_str()));
    assert_eq!(event.target_ws_id.as_deref(), Some(ws_id.as_str()));
    assert_eq!(event.task_id, "", "唤醒轮没有台账行");
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
// W11：来源空间没打开 → 投递时先打开，再投
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn w11_a_closed_origin_space_is_opened_before_delivery() {
    let env = new_env(&[]).await;
    let upstream = FakeUpstream::start(text_reply("收到，我去核对这条任务")).await;
    install_team(
        &env,
        "peco",
        &upstream,
        &[fixture("@master", "master", &[])],
    );
    let dir = env.work_dir("closed_peco");
    let ws_id = env.resolve(&dir, "peco").await.unwrap();
    assert!(!env.is_open(&ws_id).await, "夹具前提：空间此刻是关着的");
    let peco = peco_identity(&env, &ws_id).await;
    let (pump, _chat_runs) = wired_pump(&env, peco);

    env.peco_pending_row(1, "t-closed", &ws_id).await;

    pump.tick().await;

    let row = wait_for_delivered(&env, "t-closed").await;
    assert!(row.delivered_at.is_some(), "送达必须留时间戳：{row:?}");
    assert!(
        env.is_open(&ws_id).await,
        "投递必须先打开来源空间，再往会话里投"
    );
    let user = wait_for_message(&env, "user").await;
    assert!(user.starts_with("[peco-wake]"), "{user}");
}

// ============================================================================
// W11b：来源空间已经不存在 → 跳过，不建会话空壳
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn w11b_a_missing_origin_space_skips_the_row() {
    let env = new_env(&[]).await;
    let peco = Arc::new(PecoIdentity {
        ws_id: "ws_missing".to_string(),
        work_dir: env.home.join("spaces").join("missing").join("work"),
        team_id: "research".to_string(),
        from_marker: false,
    });
    let (pump, _chat_runs) = wired_pump(&env, peco);
    seed_pending_for(&env, 1, "t-gone", "ws_missing", "c_any").await;

    pump.tick().await;

    let (wake_state, reason): (String, Option<String>) = sqlx::query_as(
        "SELECT wake_state, wake_skip_reason FROM d_dispatches WHERE task_id = 't-gone'",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(wake_state, "skipped", "永远投不出去的行要停掉");
    assert_eq!(reason.as_deref(), Some("space_closed"), "成因交给关闭");
    assert_eq!(message_count(&env).await, 0, "没投出去就不该落下任何消息");
    let conversations: i64 = sqlx::query_scalar("SELECT count(*) FROM conversations")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(conversations, 0, "空间都不在了，不得给它建会话空壳");
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

    // 用户正在看的就是空间会话：唤醒的落点与他一模一样，故必须让路。
    let ensured = env
        .post(
            &format!("/api/spaces/{ws_id}/conversation/ensure"),
            json!({}),
        )
        .await;
    assert_eq!(ensured.status, 200, "建会话失败: {}", ensured.body);
    let conv_id = ensured.body["conversation_id"]
        .as_str()
        .unwrap()
        .to_string();
    // 而且要让整轮让路（不认领）。
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
    let pump = WakePump::new(env.pool.clone(), peco, env.shutdown.clone());

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

// ============================================================================
// SSE 一轮正常收尾：流必须自己结束，会话必须随之释放
// ============================================================================

/// 与泵共用同一份 `chat_runs` 的 `AppState`。
///
/// `TestEnv::router()` 每次都新建一份注册表：SSE 侧注册的条目投递端就看不见，
/// 「用户正看着这个会话」与「这一轮已经结束」两条判据都会失真。
fn app_state_sharing_runs(env: &TestEnv, chat_runs: &Arc<ChatActiveRuns>) -> AppState {
    AppState {
        chat_runs: Arc::clone(chat_runs),
        ..env.app_state()
    }
}

/// 起一轮真实的 SSE 对话，返回事件名序列的读取端；流结束时它才结束。
///
/// 与 `dispatch_closure.rs::start_turn` 同一条路（真实路由 + 真读响应体），两点
/// 不同：会话由调用方指定（唤醒的落点），且这里要留下事件名 —— 「流有没有正常
/// 收尾」是本用例的判据，把响应体丢掉就看不见。
async fn start_stream_turn(
    env: &TestEnv,
    chat_runs: &Arc<ChatActiveRuns>,
    ws_id: &str,
    conv_id: &str,
    message: &str,
) -> tokio::task::JoinHandle<Vec<String>> {
    let uri = format!("/api/chat/@master/conversations/{conv_id}/stream?message={message}");
    let request = support::bearer(Request::builder())
        .method(Method::GET)
        .uri(&uri)
        .header("X-Peco-Space", ws_id)
        .body(Body::empty())
        .expect("build the SSE request");
    let response = peco_daemon::http::router(app_state_sharing_runs(env, chat_runs))
        .oneshot(request)
        .await
        .expect("router 不会失败");
    assert!(
        response.status().is_success(),
        "SSE 握手失败: {}",
        response.status()
    );

    let mut body = response.into_body().into_data_stream();
    tokio::spawn(async move {
        let mut names = Vec::new();
        let mut buffer = String::new();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.expect("SSE 帧必须可读");
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(end) = buffer.find('\n') {
                let line: String = buffer.drain(..=end).collect();
                if let Some(name) = line.trim_end().strip_prefix("event: ") {
                    names.push(name.to_string());
                }
            }
        }
        names
    })
}

/// 一轮 SSE 对话跑完后，流必须自己结束、最后一条必须是 `done`、会话注册条目必须
/// 释放，于是「连接开过但这一轮已经结束」时唤醒不再被挡。
///
/// 先前的单测直接调 `turn_end_events` / `remember_turn_usage`：事件循环退不退都
/// 绿。变异方法：把 `chat/run.rs` 的 Idle 分支改回 `}) if sink.is_headless() => {`
/// ⇒ 本轮既不发 `done` 也不断流，第一条断言在 [`STREAM_DEADLINE`] 处超时变红，
/// 后三条也一并失去前提。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_finished_sse_turn_ends_the_stream_and_releases_the_session() {
    let env = new_env(&[]).await;
    // 首轮压 200ms：给「这一轮确实在跑」一个确定的观察窗口，注册条目必已存在。
    let upstream = FakeUpstream::start(|_: Value| async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        Reply::Text("收到".to_string())
    })
    .await;
    let ws_id = open_peco_space(&env, &upstream).await;
    let peco = peco_identity(&env, &ws_id).await;
    let (pump, chat_runs) = wired_pump(&env, peco);

    // 唤醒的落点是 Peco 空间的 `scope='space'` 会话：这一轮必须跑在同一条会话上，
    // 「连接开过、但这一轮已经结束」才可能挡住唤醒。
    let ensured = env
        .post(
            &format!("/api/spaces/{ws_id}/conversation/ensure"),
            json!({}),
        )
        .await;
    assert_eq!(ensured.status, 200, "建空间会话失败: {}", ensured.body);
    let conv_id = ensured.body["conversation_id"]
        .as_str()
        .expect("必须返回会话 id")
        .to_string();
    let scope: String = sqlx::query_scalar("SELECT scope FROM conversations WHERE id = ?")
        .bind(&conv_id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(scope, "space", "唤醒只投进空间会话");

    // 查询参数里不放空格：`message` 直接拼进 URI，空格会让 request 构造失败。
    let drain = start_stream_turn(&env, &chat_runs, &ws_id, &conv_id, "check-the-ledger").await;
    assert!(
        chat_runs.is_active(&conv_id),
        "这一轮必须真的跑起来了，否则本用例什么都没测到"
    );

    let events = timeout(STREAM_DEADLINE, drain)
        .await
        .expect("SSE 流必须在限时内结束：Idle 之后既不补 done 也不断流，前端会一直等下去")
        .expect("事件读取不得 panic");
    assert_eq!(
        events.last().map(String::as_str),
        Some("done"),
        "流的最后一条事件必须是 done，实际 {events:?}"
    );

    // 守卫的 drop 与断流不必同一时刻，但不能永远不到：不释放条目，「忙」判据就
    // 永远是「连接开过」，该会话此后的唤醒全被让路。
    let deadline = Instant::now() + STREAM_DEADLINE;
    while chat_runs.is_active(&conv_id) {
        if Instant::now() >= deadline {
            panic!("{STREAM_DEADLINE:?} 内注册条目必须释放，否则该会话的唤醒被永久堵死");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // 连接已开过、这一轮已结束：此时投进来的唤醒必须照常送达。
    env.peco_pending_row(1, "t-after-stream", &ws_id).await;
    pump.tick().await;
    let row = wait_for_delivered(&env, "t-after-stream").await;
    assert!(row.delivered_at.is_some(), "送达必须留时间戳：{row:?}");
}
