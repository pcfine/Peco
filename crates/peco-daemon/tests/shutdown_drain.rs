//! 停机：断流、拒绝新活、收尾顺序。
//!
//! 这里不跑真实二进制（`daemon_lifecycle.rs` 管那一段），只驱动 `shutdown::drain`
//! 与路由 —— 判据是「取消之后还有没有人为这条流/这个入口活着」。

mod support;

use std::time::Duration;

use axum::http::Method;
use futures::StreamExt;
use model_provider::{Content, InputItem, Role};
use peco_core::session::{AnnotatedMessage, MessageId, MessageSource, Session, SessionPersister};
use peco_daemon::db::schedules::{self, NewRun, NewTask};
use peco_daemon::schedule::{RunStatus, TriggerKind};
use peco_daemon::session_dto::turn_interrupted_reason;
use peco_daemon::session_store::SqliteSessionPersister;
use peco_daemon::shutdown;
use serde_json::json;
use sqlx::SqlitePool;
use support::{new_env, stream_in_space};

/// 等一条被测命令的上限：裸 `await` 在变异时会把整条测试挂死，而不是给出失败。
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

/// 用真实 core 会话落一份「被取消」的快照 —— 停机要改写的正是它。
async fn persist_cancelled_turn(pool: &SqlitePool, conversation_id: &str) {
    let mut session = Session::new(conversation_id.to_string(), "长任务".to_string());
    // 必须带上模型产物：只有 user_input 的 staging 会被冻结路径回滚成空轮。
    session
        .hydrate_inflight(vec![
            AnnotatedMessage::new(
                MessageId(0),
                0,
                InputItem::Message {
                    role: Role::User,
                    content: Content::Text("跑个长任务".to_string()),
                },
                MessageSource::UserInput,
            ),
            AnnotatedMessage::new(
                MessageId(1),
                0,
                InputItem::Message {
                    role: Role::Assistant,
                    content: Content::Text("开始跑".to_string()),
                },
                MessageSource::ModelGeneration,
            ),
        ])
        .expect("空会话可灌入在途轮");
    let token = session
        .interrupt_turn("cancelled by user")
        .expect("冻结在途轮");
    let snapshot = session.snapshot(&token);
    SqliteSessionPersister::new(pool.clone())
        .save(
            &snapshot,
            conversation_id,
            session.description(),
            session.created_at(),
        )
        .await
        .expect("快照落盘");
}

/// 每一条永久 SSE 都必须自己对停机闩做出反应：优雅退出在等它们结束。
#[tokio::test]
async fn the_events_stream_ends_when_the_shutdown_starts() {
    let env = new_env(&[]).await;
    let state = env.app_state();

    let response = stream_in_space(
        &peco_daemon::http::router(state.clone()),
        Method::GET,
        "/api/events",
        None,
    )
    .await;
    assert_eq!(response.status(), 200, "事件流握手失败");

    let mut body = response.into_body().into_data_stream();
    // 没有台账事件时流是静默的：先确认它挂住了，再置闩。
    assert!(
        tokio::time::timeout(Duration::from_millis(100), body.next())
            .await
            .is_err(),
        "没有事件时流必须保持打开"
    );

    state.shutdown.cancel();
    let ended = tokio::time::timeout(Duration::from_secs(2), async {
        while body.next().await.is_some() {}
    })
    .await;
    assert!(
        ended.is_ok(),
        "停机后事件流必须结束（否则优雅退出永远等不到）"
    );
}

/// 停机序列：先停泵与调度，再取消在途轮；run 全部结束之后改写中断措辞，
/// 台账与排期行收尾、空间关掉、库关掉。
///
/// 变异方法：去掉 `drain` 里的 `cancel_all_for_shutdown` → 那个 run 任务永远收不到
/// 命令、注册表永远排不空；去掉 `interrupt_running` → 两行排期的断言变红；
/// 去掉 `relabel_interrupted_turns` → 历史里的成因串仍是 `cancelled by user`。
#[tokio::test]
async fn drain_stops_everything_and_finishes_the_ledger_rows() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("alpha");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();
    assert!(env.is_open(&ws_id).await, "前置：空间必须是打开状态");

    // 一条 running 的排期行 + 一条在飞的台账行。
    schedules::insert_task(
        &env.pool,
        &NewTask {
            id: "s1",
            name: "站会",
            target_ws_id: &ws_id,
            prompt: "汇报",
            cron_expr: "0 9 * * *",
            enabled: true,
            next_run_at: None,
        },
    )
    .await
    .unwrap();
    schedules::insert_run(
        &env.pool,
        &NewRun {
            id: "r1",
            schedule_id: "s1",
            trigger_kind: TriggerKind::Manual,
            scheduled_for: "2026-01-01 00:00:00",
            status: RunStatus::Pending,
            error: None,
            finished_at: None,
        },
    )
    .await
    .unwrap();
    schedules::mark_running(&env.pool, "r1", "conv-1")
        .await
        .unwrap();
    env.insert_dispatch(
        &ws_id,
        "t-inflight",
        "dispatch_to_workspace",
        "ws_peco",
        "working",
    )
    .await;

    // 这一轮的历史已经由 core 的取消路径落过盘，成因串还是「cancelled by user」。
    persist_cancelled_turn(&env.pool, "conv-1").await;

    // 在途轮：收到停机命令就收尾（真实 run 任务干的是同一件事里的登记部分）。
    let state = env.app_state();
    state.wake.spawn();
    state.schedules.spawn();
    let (mut control_rx, guard) = state.chat_runs.register("conv-1");
    let run_task = tokio::spawn(async move {
        let command = control_rx.recv().await;
        // 守卫随任务一起 drop —— 注册表条目消失，drain 才走得下去。
        drop(guard);
        command
    });

    shutdown::drain(&state).await;

    // 具体是哪一条命令由 `chat::active` 的单测钉住（`ChatControl` 不出 crate）。
    let received = tokio::time::timeout(COMMAND_TIMEOUT, run_task)
        .await
        .expect("在途轮必须收到停机命令（超时说明 drain 没投递）")
        .expect("在途任务不得 panic");
    assert!(received.is_some(), "在途轮必须收到停机命令");
    // 库已被停机序列关掉，索引行读不了：空间开没开只有实例表知道。
    assert!(!env.hub.is_open(&work_dir), "停机必须关掉已打开的空间");

    // 库已关：另开一个连接读收尾结果。
    let pool = peco_daemon::db::connect(&env.config.db_url())
        .await
        .unwrap();
    let run = schedules::run_by_id(&pool, "r1").await.unwrap().unwrap();
    assert_eq!(run.status, "interrupted", "running 的排期行必须收尾");
    assert_eq!(
        run.error.as_deref(),
        Some("service_shutdown"),
        "成因要能与「上个进程死掉」区分开"
    );
    let task_state: String =
        sqlx::query_scalar("SELECT task_state FROM d_dispatches WHERE task_id = 't-inflight'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(task_state, "orphan", "在飞台账行必须标成结果不可回捞");

    // 措辞改写发生在最后一轮：历史里读到的必须是「停机」而不是「用户取消」。
    let (snapshot, _meta) = SqliteSessionPersister::new(pool.clone())
        .load("conv-1")
        .await
        .unwrap()
        .expect("被中断的会话必须留下快照");
    assert_eq!(snapshot.committed_turns.len(), 1, "轮次结构不得被改写动过");
    assert_eq!(snapshot.turn_index, 1, "轮次编号不得变");
    assert_eq!(
        turn_interrupted_reason(&snapshot.committed_turns[0]).as_deref(),
        Some("service shutdown"),
        "改写没生效，drain 之后历史里仍是「用户取消」"
    );
    pool.close().await;
}

/// 停机期间的新活一律 503 `shutting_down`：开了也是白开，用户会先收到一条被中断的对话。
#[tokio::test]
async fn new_work_is_rejected_with_shutting_down() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("alpha");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();

    let state = env.app_state();
    state.shutdown.cancel();
    let router = peco_daemon::http::router(state);

    let reply = support::call_in_space(
        &router,
        Method::GET,
        "/api/chat/@master/conversations/conv-x/stream?message=hi",
        None,
        Some(&ws_id),
    )
    .await;
    assert_eq!(reply.status, 503, "churn: {}", reply.body);
    assert_eq!(reply.code(), "shutting_down");

    let reply = support::call_in_space(
        &router,
        Method::POST,
        "/api/schedules/s1/run",
        Some(json!({})),
        None,
    )
    .await;
    assert_eq!(reply.status, 503, "churn: {}", reply.body);
    assert_eq!(reply.code(), "shutting_down");

    let reply = support::call_in_space(
        &router,
        Method::POST,
        &format!("/api/spaces/{ws_id}/open"),
        Some(json!({})),
        None,
    )
    .await;
    assert_eq!(reply.status, 503, "churn: {}", reply.body);
    assert_eq!(reply.code(), "shutting_down");
}
