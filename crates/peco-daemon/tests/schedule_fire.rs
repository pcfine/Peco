//! 定时任务的端到端：真的开空间、真的投一条 `[定时任务]`、真的跑完一轮并通知。
//!
//! 走的是生产同一条链路（HTTP 路由 → `Scheduler::run_now` → `chat::run` 的无客户端
//! 入口），差别只有假上游站在模型那一侧。

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::Method;
use peco_daemon::chat::ChatActiveRuns;
use peco_daemon::db::conversations;
use peco_daemon::db::schedules::{self, ScheduledRunRow, ScheduledTaskRow};
use peco_daemon::schedule::scheduler::Scheduler;
use peco_daemon::space::peco::PecoIdentity;
use peco_daemon::state::AppState;
use peco_daemon::wake::deliver::WakeDelivery;
use serde_json::{Value, json};
use support::upstream::{FakeUpstream, Reply};
use support::{TestEnv, call, fixture, install_team, new_env};

/// 一轮跑完的等待上限：假上游是进程内的，卡住就说明链路断在别处。
const TURN_DEADLINE: Duration = Duration::from_secs(20);

const PECO_TEAM: &str = "peco";

// ============================================================================
// 夹具
// ============================================================================

/// 铺团队目录并索引一个空间；返回 `(ws_id, work_dir)`。空间**不打开** ——
/// 定时任务自己负责打开它。
async fn indexed_space(env: &TestEnv, name: &str, team: &str, upstream: &FakeUpstream) -> String {
    install_team(env, team, upstream, &[fixture("@master", "master", &[])]);
    let dir = env.work_dir(name);
    env.resolve(&dir, team).await.expect("空间应能被索引")
}

/// Peco 空间：通知的落点。与 `main` 的引导同序 —— 先索引，再打开。
async fn open_peco_space(env: &TestEnv, upstream: &FakeUpstream) -> Arc<PecoIdentity> {
    let ws_id = indexed_space(env, PECO_TEAM, PECO_TEAM, upstream).await;
    env.open(&ws_id).await.expect("Peco 空间应能打开");
    Arc::new(PecoIdentity {
        ws_id: ws_id.clone(),
        work_dir: env.work_dir_of(&ws_id).await,
        team_id: PECO_TEAM.to_string(),
        from_marker: true,
    })
}

/// 与 `main` 同一条装配链的调度器；`chat_runs` 由调用方持有，好在用例里占住会话。
fn scheduler(
    env: &TestEnv,
    peco: Arc<PecoIdentity>,
    chat_runs: Arc<ChatActiveRuns>,
) -> Arc<Scheduler> {
    let spaces = env.spaces();
    let delivery = Arc::new(WakeDelivery::new(
        env.pool.clone(),
        spaces.clone(),
        chat_runs.clone(),
        Arc::downgrade(&env.env),
    ));
    Arc::new(Scheduler::new(
        env.pool.clone(),
        spaces,
        chat_runs,
        peco,
        Arc::downgrade(&env.env),
        delivery,
        env.shutdown.clone(),
    ))
}

/// `AppState` 里换掉调度器：路由与中间件仍是真的，只有一份共享的 `chat_runs`
/// 被换成用例自己那一份。
fn app_with(env: &TestEnv, scheduler: Arc<Scheduler>) -> AppState {
    AppState {
        schedules: scheduler,
        ..env.app_state()
    }
}

async fn create_schedule(
    env: &TestEnv,
    scheduler: &Arc<Scheduler>,
    ws_id: &str,
    cron_expr: &str,
) -> String {
    let reply = call(
        &peco_daemon::http::router(app_with(env, scheduler.clone())),
        Method::POST,
        "/api/schedules",
        Some(json!({
            "name": "每日站会",
            "target_ws_id": ws_id,
            "prompt": "汇报昨天的进展",
            "cron_expr": cron_expr,
        })),
    )
    .await;
    assert_eq!(reply.status, 201, "建排期失败: {}", reply.body);
    reply.body["id"].as_str().expect("必须返回 id").to_string()
}

async fn run_now(env: &TestEnv, scheduler: &Arc<Scheduler>, id: &str) -> Value {
    let reply = call(
        &peco_daemon::http::router(app_with(env, scheduler.clone())),
        Method::POST,
        &format!("/api/schedules/{id}/run"),
        Some(json!({})),
    )
    .await;
    assert_eq!(reply.status, 200, "手动触发失败: {}", reply.body);
    reply.body
}

/// 等到 run 落到指定状态；超时就把当前行打出来。
async fn wait_for_status(env: &TestEnv, run_id: &str, status: &str) -> ScheduledRunRow {
    let deadline = Instant::now() + TURN_DEADLINE;
    loop {
        let row = schedules::run_by_id(&env.pool, run_id)
            .await
            .unwrap()
            .expect("run 行必须存在");
        if row.status == status {
            return row;
        }
        assert!(
            Instant::now() < deadline,
            "{TURN_DEADLINE:?} 内必须落到 {status}，实际 {}（error={:?}）",
            row.status,
            row.error
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn messages_of(env: &TestEnv, conversation_id: &str) -> Vec<(String, String)> {
    sqlx::query_as::<_, (String, String)>(
        "SELECT role, content FROM messages WHERE conversation_id = ? ORDER BY rowid",
    )
    .bind(conversation_id)
    .fetch_all(&env.pool)
    .await
    .unwrap()
}

async fn task_of(env: &TestEnv, id: &str) -> ScheduledTaskRow {
    schedules::task_by_id(&env.pool, id).await.unwrap().unwrap()
}

/// 等到调度行的 `last_status` 落到指定值。
///
/// run 的终态与「上次结果」是两条 UPDATE（`finish_run` → `record_task_last_run`），
/// 中间有一个极短窗口。只等 run 行就断言 `last_status`，会偶发红在这个窗口上 ——
/// 用例必须等它，而不是把窗口当成失败。生产侧的窗口无害：列表页晚一个 tick 显示
/// 「上次结果」不影响任何判定。
async fn wait_for_last_status(env: &TestEnv, id: &str, status: &str) -> ScheduledTaskRow {
    let deadline = Instant::now() + TURN_DEADLINE;
    loop {
        let row = task_of(env, id).await;
        if row.last_status.as_deref() == Some(status) {
            return row;
        }
        assert!(
            Instant::now() < deadline,
            "{TURN_DEADLINE:?} 内 last_status 必须落到 {status}，实际 {:?}",
            row.last_status
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 把槽位摆到「刚刚到期」或「早就过期」：tick 只看 `next_run_at <= now`。
async fn set_next_run_at(env: &TestEnv, id: &str, offset: &str) {
    sqlx::query("UPDATE scheduled_tasks SET next_run_at = datetime('now', ?) WHERE id = ?")
        .bind(offset)
        .bind(id)
        .execute(&env.pool)
        .await
        .unwrap();
}

// ============================================================================
// 一次完整的手动运行
// ============================================================================

/// 目标空间原本是关着的：触发要自己把它打开、真的投一条 `[定时任务]`、真的跑完一轮，
/// 并把结果写进 run 行与调度行。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_manual_run_opens_the_space_and_completes_in_it() {
    let env = new_env(&["research"]).await;
    let upstream = FakeUpstream::start(|_: Value| async move {
        Reply::Text("昨天的进展：12 个测试通过".to_string())
    })
    .await;
    let peco = open_peco_space(&env, &upstream).await;
    let ws_id = indexed_space(&env, "alpha", "research", &upstream).await;
    assert!(!env.is_open(&ws_id).await, "起点必须是一个关着的空间");

    let scheduler = scheduler(&env, peco, Arc::new(ChatActiveRuns::new()));
    let id = create_schedule(&env, &scheduler, &ws_id, "0 9 * * *").await;

    let body = run_now(&env, &scheduler, &id).await;
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let row = wait_for_status(&env, &run_id, "completed").await;

    assert!(env.is_open(&ws_id).await, "触发必须自己把空间打开");
    let conversation_id = row.conversation_id.clone().expect("必须记下会话 id");
    let messages = messages_of(&env, &conversation_id).await;
    assert_eq!(
        messages.len(),
        2,
        "一轮 = 一条 user + 一条 assistant：{messages:?}"
    );
    assert_eq!(messages[0].0, "user");
    assert!(
        messages[0].1.starts_with("[定时任务] 每日站会（schedule="),
        "投出去的文本要带固定前缀与排期 id：{}",
        messages[0].1
    );
    assert!(
        messages[0].1.ends_with("汇报昨天的进展"),
        "正文必须原样带上：{}",
        messages[0].1
    );
    assert!(messages[1].1.contains("12 个测试通过"), "{}", messages[1].1);

    assert!(row.attempts >= 1, "启动这一轮要计一次尝试");
    assert!(row.finished_at.is_some());
    assert_eq!(
        row.result_digest.as_deref(),
        Some("昨天的进展：12 个测试通过"),
        "摘要取自这一轮的正文"
    );
    assert_eq!(row.notify_state, "pending", "完成的轮次要通知");

    let task = wait_for_last_status(&env, &id, "completed").await;
    assert!(task.last_run_at.is_some());
    // 模型确实收到了那条文本 —— 落库只证明「写了消息」。
    let saw = upstream
        .exchanges_with_model("master")
        .iter()
        .any(|exchange| {
            exchange.request["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|message| {
                    message["role"] == json!("user")
                        && message["content"]
                            .as_str()
                            .is_some_and(|text| text.starts_with("[定时任务]"))
                })
        });
    assert!(saw, "模型必须真的收到 [定时任务] 文本");
}

// ============================================================================
// 通知
// ============================================================================

/// 跑完之后的那一轮通知真的进 Peco 空间：投递端与唤醒共用，落点是 `@master` 会话。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_completed_run_notifies_the_peco_space() {
    let env = new_env(&["research"]).await;
    let upstream =
        FakeUpstream::start(|_: Value| async move { Reply::Text("都正常".to_string()) }).await;
    let peco = open_peco_space(&env, &upstream).await;
    let ws_id = indexed_space(&env, "alpha", "research", &upstream).await;
    let scheduler = scheduler(&env, peco.clone(), Arc::new(ChatActiveRuns::new()));
    let id = create_schedule(&env, &scheduler, &ws_id, "0 9 * * *").await;

    let body = run_now(&env, &scheduler, &id).await;
    let run_id = body["run_id"].as_str().unwrap().to_string();
    wait_for_status(&env, &run_id, "completed").await;

    scheduler.tick().await;

    let notified = wait_for_status(&env, &run_id, "completed").await;
    assert_eq!(
        notified.notify_state, "sent",
        "通知必须送达 Peco 空间，实际 {:?}",
        notified.notify_state
    );
    assert!(notified.notified_at.is_some(), "送达要留时间戳");

    // 通知轮的 user 行由 `spawn_run` 的任务写，`deliver` 返回时它可能还没落库。
    let peco_conv = conversations::ensure_space_session(&env.pool, &peco.ws_id, "@master")
        .await
        .unwrap()
        .0;
    let deadline = Instant::now() + TURN_DEADLINE;
    let notification = loop {
        let messages = messages_of(&env, &peco_conv).await;
        if let Some((_, content)) = messages
            .iter()
            .find(|(role, content)| role == "user" && content.starts_with("[peco-schedule]"))
        {
            break content.clone();
        }
        assert!(
            Instant::now() < deadline,
            "{TURN_DEADLINE:?} 内 Peco 会话里必须出现通知，实际 {messages:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        notification.contains("定时任务已完成 每日站会"),
        "{notification}"
    );
    assert!(
        notification.contains(&format!("run={run_id}")),
        "{notification}"
    );
    assert!(
        notification.contains("- 目标空间: research（"),
        "{notification}"
    );
    assert!(notification.contains("- 触发: manual"), "{notification}");
    assert!(notification.contains("都正常"), "{notification}");
}

// ============================================================================
// 会话忙：排队 → 烧完预算 → 放弃
// ============================================================================

/// 用户正占着目标会话：这一轮让路（`pending` + 60 秒后重试），下一次到点仍忙则
/// 记满 10 次尝试后放弃，且**不会**多出一行 run。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_busy_conversation_retries_then_gives_up() {
    let env = new_env(&["research"]).await;
    let upstream = FakeUpstream::start(|_: Value| async move {
        Reply::Text("这一轮本来就不该跑".to_string())
    })
    .await;
    let peco = open_peco_space(&env, &upstream).await;
    let ws_id = indexed_space(&env, "alpha", "research", &upstream).await;
    env.open(&ws_id).await.expect("目标空间必须已打开");

    // 用户正看着这个会话：注册条目在，「忙」成立。
    let chat_runs = Arc::new(ChatActiveRuns::new());
    let (conv_id, _created) = conversations::ensure_space_session(&env.pool, &ws_id, "@master")
        .await
        .unwrap();
    let (_control_rx, _guard) = chat_runs.register(&conv_id);

    let scheduler = scheduler(&env, peco, chat_runs);
    let id = create_schedule(&env, &scheduler, &ws_id, "0 9 * * *").await;

    let body = run_now(&env, &scheduler, &id).await;
    assert_eq!(body["status"], json!("pending"), "忙不是失败，是排队");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let queued = schedules::run_by_id(&env.pool, &run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(queued.attempts, 1);
    assert!(queued.next_attempt_at.is_some(), "排队必须给出重试时刻");
    assert!(
        messages_of(&env, &conv_id).await.is_empty(),
        "让路时一条消息都不该落库"
    );

    // 把这一行推到「第 10 次尝试 + 已到重试时刻」：再来一次就到头。
    sqlx::query(
        "UPDATE scheduled_runs SET attempts = 9, next_attempt_at = datetime('now', '-1 minute') \
         WHERE id = ?",
    )
    .bind(&run_id)
    .execute(&env.pool)
    .await
    .unwrap();
    scheduler.tick().await;

    let given_up = schedules::run_by_id(&env.pool, &run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(given_up.status, "skipped");
    assert_eq!(given_up.error.as_deref(), Some("conversation_busy"));
    assert_eq!(given_up.attempts, 10, "第 10 次尝试到头");
    assert!(given_up.finished_at.is_some());
    assert_eq!(given_up.notify_state, "none", "放弃的轮次不通知用户");
    assert_eq!(
        schedules::runs_by_schedule(&env.pool, &id, 10)
            .await
            .unwrap()
            .len(),
        1,
        "重试复用同一行，不得新建 run"
    );
    wait_for_last_status(&env, &id, "skipped").await;
}

/// 停用任务时，上一枪还排在重试队列里：这一枪必须跟着停 —— 不能照样打开空间、
/// 往会话里投一条、再跑一轮。
///
/// 构造顺序是这一条的关键：先让会话被占住（run 落 `pending`），再放开会话、关掉
/// 空间、停用任务。此后任何一次「照样重试」都会重新打开空间并投出消息。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disabling_a_schedule_drops_the_run_queued_for_retry() {
    let env = new_env(&["research"]).await;
    let upstream = FakeUpstream::start(|_: Value| async move {
        Reply::Text("这一轮本来就不该跑".to_string())
    })
    .await;
    let peco = open_peco_space(&env, &upstream).await;
    let ws_id = indexed_space(&env, "alpha", "research", &upstream).await;

    // 用户正看着这个会话：注册条目在，「忙」成立，这一枪只能排队。
    let chat_runs = Arc::new(ChatActiveRuns::new());
    let (conv_id, _created) = conversations::ensure_space_session(&env.pool, &ws_id, "@master")
        .await
        .unwrap();
    let (_control_rx, guard) = chat_runs.register(&conv_id);

    let scheduler = scheduler(&env, peco, chat_runs);
    let id = create_schedule(&env, &scheduler, &ws_id, "0 9 * * *").await;
    let body = run_now(&env, &scheduler, &id).await;
    assert_eq!(body["status"], json!("pending"), "忙不是失败，是排队");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let queued = schedules::run_by_id(&env.pool, &run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(queued.status, "pending");
    assert!(queued.next_attempt_at.is_some(), "排队必须给出重试时刻");

    // 用户离开了会话、关掉了空间，又把任务停用。
    drop(guard);
    let closed = env
        .post(&format!("/api/spaces/{ws_id}/close"), json!({}))
        .await;
    assert_eq!(closed.status, 200, "关空间失败: {}", closed.body);
    assert!(!env.is_open(&ws_id).await, "空间应已关掉");
    let reply = call(
        &peco_daemon::http::router(app_with(&env, scheduler.clone())),
        Method::PATCH,
        &format!("/api/schedules/{id}"),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(reply.status, 200, "停用失败: {}", reply.body);
    assert!(!task_of(&env, &id).await.enabled, "任务必须真的停用了");

    // 重试时刻到了。
    sqlx::query(
        "UPDATE scheduled_runs SET next_attempt_at = datetime('now', '-1 minute') WHERE id = ?",
    )
    .bind(&run_id)
    .execute(&env.pool)
    .await
    .unwrap();
    let opens_before = env.opener.calls();
    scheduler.tick().await;

    let stopped = schedules::run_by_id(&env.pool, &run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stopped.status, "skipped", "停用后这一枪要收成跳过");
    assert_eq!(stopped.error.as_deref(), Some("schedule_disabled"));
    assert!(stopped.finished_at.is_some(), "跳过也要留终态时间戳");
    assert!(
        messages_of(&env, &conv_id).await.is_empty(),
        "停用之后不得再往会话里投消息"
    );
    assert_eq!(env.opener.calls(), opens_before, "停用之后不得再打开空间");
    assert!(!env.is_open(&ws_id).await, "空间必须仍关着");
    assert!(
        upstream.exchanges_with_model("master").is_empty(),
        "停用之后模型一次都不该被调用"
    );
    wait_for_last_status(&env, &id, "skipped").await;
    assert_eq!(
        schedules::runs_by_schedule(&env.pool, &id, 10)
            .await
            .unwrap()
            .len(),
        1,
        "重试复用同一行，不得新建 run"
    );
}

// ============================================================================
// 前一轮未完成 / 错过槽位
// ============================================================================

/// 上一轮还挂在 `running`（进程被杀、观察者没了）：下一槽位记 `previous_running`，
/// 不再往上叠一轮 —— 同一会话里叠两轮只会互相抢注册。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slot_is_skipped_while_a_previous_run_never_finished() {
    let env = new_env(&["research"]).await;
    let upstream =
        FakeUpstream::start(|_: Value| async move { Reply::Text("不该跑到".into()) }).await;
    let peco = open_peco_space(&env, &upstream).await;
    let ws_id = indexed_space(&env, "alpha", "research", &upstream).await;
    let scheduler = scheduler(&env, peco, Arc::new(ChatActiveRuns::new()));
    let id = create_schedule(&env, &scheduler, &ws_id, "0 9 * * *").await;

    let stuck = schedules::insert_run(
        &env.pool,
        &schedules::NewRun {
            id: "r-stuck",
            schedule_id: &id,
            trigger_kind: peco_daemon::schedule::TriggerKind::Cron,
            scheduled_for: "2000-01-01 00:00:00",
            status: peco_daemon::schedule::RunStatus::Running,
            error: None,
            finished_at: None,
        },
    )
    .await
    .unwrap()
    .expect("槽位首次写入必须成功");
    assert_eq!(stuck.status, "running");
    set_next_run_at(&env, &id, "-1 minute").await;

    scheduler.tick().await;

    let rows = schedules::runs_by_schedule(&env.pool, &id, 10)
        .await
        .unwrap();
    let skipped = rows
        .iter()
        .find(|row| row.id != "r-stuck")
        .unwrap_or_else(|| panic!("必须留下一条跳过记录，实际 {rows:?}"));
    assert_eq!(skipped.status, "skipped");
    assert_eq!(skipped.error.as_deref(), Some("previous_running"));
    assert_eq!(
        schedules::run_by_id(&env.pool, "r-stuck")
            .await
            .unwrap()
            .unwrap()
            .status,
        "running",
        "在途的那一行不得被 tick 改写"
    );
    assert!(
        messages_of(
            &env,
            &conversations::ensure_space_session(&env.pool, &ws_id, "@master")
                .await
                .map(|(id, _)| id)
                .unwrap_or_default()
        )
        .await
        .is_empty(),
        "跳过时不得投出消息"
    );
}

/// 错过补跑窗口（11 分钟前）：记 `missed` 并推进槽位，不补跑。
///
/// 变异：把 `GRACE` 改成 0 ⇒ 这一轮会被当成「还来得及」直接跑起来，本用例红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slot_beyond_the_grace_window_is_skipped_as_missed() {
    let env = new_env(&["research"]).await;
    let upstream =
        FakeUpstream::start(|_: Value| async move { Reply::Text("不该跑到".into()) }).await;
    let peco = open_peco_space(&env, &upstream).await;
    let ws_id = indexed_space(&env, "alpha", "research", &upstream).await;
    let scheduler = scheduler(&env, peco, Arc::new(ChatActiveRuns::new()));
    let id = create_schedule(&env, &scheduler, &ws_id, "0 9 * * *").await;
    set_next_run_at(&env, &id, "-11 minutes").await;
    let before = task_of(&env, &id).await.next_run_at.clone();

    scheduler.tick().await;

    let rows = schedules::runs_by_schedule(&env.pool, &id, 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "只该有一条跳过记录：{rows:?}");
    assert_eq!(rows[0].status, "skipped");
    assert_eq!(rows[0].error.as_deref(), Some("missed"));
    assert_eq!(
        rows[0].scheduled_for,
        before.clone().expect("到期前必须有一个槽位"),
        "跳过记录要落在错过的那个槽位上"
    );
    let after = task_of(&env, &id).await;
    assert_ne!(
        after.next_run_at, before,
        "槽位必须推进，否则每 tick 都会重算同一条"
    );
    assert!(
        after
            .next_run_at
            .as_deref()
            .is_some_and(|next| next > "2026"),
        "推进后的槽位必须是将来：{:?}",
        after.next_run_at
    );
    assert!(!env.is_open(&ws_id).await, "跳过时不该顺手把空间打开");
}

// ============================================================================
// 同一槽位只触发一次
// ============================================================================

/// 两个 tick 同时看见同一个到期槽位：只能有一次触发。
///
/// 变异方法：`db::schedules::advance_next_run` 去掉 `AND next_run_at = ?`
/// ⇒ 两个 tick 都认为自己拿下了槽位（DAO 用例 `advancing_the_slot_only_succeeds_once`
/// 直接变红）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_ticks_racing_on_one_slot_fire_it_once() {
    let env = new_env(&["research"]).await;
    let upstream = FakeUpstream::start(|_: Value| async move { Reply::Text("收到".into()) }).await;
    let peco = open_peco_space(&env, &upstream).await;
    let ws_id = indexed_space(&env, "alpha", "research", &upstream).await;
    let scheduler = scheduler(&env, peco, Arc::new(ChatActiveRuns::new()));
    let id = create_schedule(&env, &scheduler, &ws_id, "0 9 * * *").await;
    set_next_run_at(&env, &id, "-1 minute").await;

    tokio::join!(scheduler.tick(), scheduler.tick());

    let rows = schedules::runs_by_schedule(&env.pool, &id, 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "同一槽位只能有一行 run：{rows:?}");
    let conv_id = conversations::ensure_space_session(&env.pool, &ws_id, "@master")
        .await
        .map(|(id, _)| id)
        .unwrap_or_default();
    let deadline = Instant::now() + TURN_DEADLINE;
    loop {
        let messages = messages_of(&env, &conv_id).await;
        if messages.iter().filter(|(role, _)| role == "user").count() >= 1 {
            let users: Vec<&String> = messages
                .iter()
                .filter(|(role, _)| role == "user")
                .map(|(_, content)| content)
                .collect();
            assert_eq!(users.len(), 1, "只能投出一条：[定时任务] 消息：{users:?}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{TURN_DEADLINE:?} 内必须投出这一轮"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ============================================================================
// 补跑窗口
// ============================================================================

/// 迟到了 5 分钟但还在补跑窗口里：照常跑，不记 `missed`。
///
/// 这条守的是窗口的两侧：只有「过期太久的跳过」没有「迟到一点的照跑」，把
/// `GRACE` 改成 0 也照样全绿 —— 那样补跑就彻底没了。
///
/// 变异：`schedule::GRACE` 改成 0 ⇒ 这一轮被判成错过，本用例红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_slot_within_the_grace_window_is_still_run() {
    let env = new_env(&["research"]).await;
    let upstream =
        FakeUpstream::start(|_: Value| async move { Reply::Text("补跑完成".into()) }).await;
    let peco = open_peco_space(&env, &upstream).await;
    let ws_id = indexed_space(&env, "alpha", "research", &upstream).await;
    let scheduler = scheduler(&env, peco, Arc::new(ChatActiveRuns::new()));
    let id = create_schedule(&env, &scheduler, &ws_id, "0 9 * * *").await;
    set_next_run_at(&env, &id, "-5 minutes").await;

    scheduler.tick().await;

    let rows = schedules::runs_by_schedule(&env.pool, &id, 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "只该有一行 run：{rows:?}");
    assert_ne!(
        rows[0].status, "skipped",
        "迟到 5 分钟还在窗口里，不得记成跳过（error={:?}）",
        rows[0].error
    );
    assert!(
        rows[0].error.is_none(),
        "补跑不该留跳过成因：{:?}",
        rows[0].error
    );
    assert!(env.is_open(&ws_id).await, "补跑必须把空间打开");

    // 真的跑完这一轮，而不是只排了一行。
    wait_for_status(&env, &rows[0].id, "completed").await;
}
