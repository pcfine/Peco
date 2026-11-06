//! 唤醒台账的用例：谁能进队列、队列怎么回收、消息长什么样。
//!
//! 边界：本文件断的是**台账与合成规则**本身 —— 每条用例的判据都能落在一列
//! `wake_state` / `wake_attempts` 或一段合成文本上。真正跑一轮模型、断消息落库
//! 与幂等重放的用例在 `wake_delivery`，那里才有假上游可脚本化。
//!
//! 唯一越界的是 W4（一批上限）：它的「剩余行仍 pending」必须真的跑一次 tick 才
//! 成立，故这一条自带 Peco 空间与假上游。

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use peco_core::agent::hooks::{DispatchObservation, DispatchOrigin, TaskObservationSink};
use peco_core::machine::{OutputView, Progress, TaskId, TaskSnapshot, TaskSource, TaskState};
use peco_daemon::chat::ChatActiveRuns;
use peco_daemon::db::dispatches::{self, DispatchRow};
use peco_daemon::space::peco::PecoIdentity;
use peco_daemon::space::tasks::TaskObserver;
use peco_daemon::wake::deliver::PecoDelivery;
use peco_daemon::wake::{self, compose::compose};
use serde_json::Value;
use sqlx::SqlitePool;
use support::upstream::{FakeUpstream, Reply};
use support::{TestEnv, fixture, install_team, new_env};
use uuid::Uuid;

/// 一批投递要真的跑一轮模型：等它落库的上限。明显小于 [`wake::TICK`]，缺任何
/// 一环都不会被定时 tick 兜成绿的。
const DELIVERY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

const ORPHAN_DIGEST: &str = "任务在进程重启时中断，输出已丢失";

// ============================================================================
// 夹具
// ============================================================================

fn snapshot(task_id: TaskId) -> TaskSnapshot {
    TaskSnapshot {
        id: task_id,
        kind: "agent".into(),
        state: TaskState::Working,
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

fn observation(target_dir: PathBuf, task_id: TaskId, origin_ws_id: &str) -> DispatchObservation {
    DispatchObservation {
        origin: DispatchOrigin {
            ws_id: origin_ws_id.into(),
            principal: "local".into(),
            origin_dir: PathBuf::from("/tmp/peco-origin"),
        },
        target_dir,
        task_id,
        dispatch_id: None,
        origin_kind: "run_machine_task".into(),
        agent_name: "@master".into(),
    }
}

/// 把播下的行推进到用例需要的那一态：唤醒态与尝试次数一起改，免得两处各写一次。
async fn arrange(pool: &SqlitePool, task_id: &str, wake_state: &str, attempts: i64) {
    sqlx::query("UPDATE d_dispatches SET wake_state = ?, wake_attempts = ? WHERE task_id = ?")
        .bind(wake_state)
        .bind(attempts)
        .bind(task_id)
        .execute(pool)
        .await
        .unwrap();
}

/// 造一条「已被认领、但认领发生在 `seconds_ago` 秒前」的行。
async fn claim_seconds_ago(pool: &SqlitePool, task_id: &str, seconds_ago: i64) {
    sqlx::query(
        "UPDATE d_dispatches SET wake_state = 'delivering', wake_attempts = 1, \
         wake_claimed_at = datetime('now', ?) WHERE task_id = ?",
    )
    .bind(format!("-{seconds_ago} seconds"))
    .bind(task_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn skip_reason(pool: &SqlitePool, task_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT wake_skip_reason FROM d_dispatches WHERE task_id = ?")
        .bind(task_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn wake_state(pool: &SqlitePool, task_id: &str) -> String {
    sqlx::query_scalar("SELECT wake_state FROM d_dispatches WHERE task_id = ?")
        .bind(task_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn claimed_at(pool: &SqlitePool, task_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT wake_claimed_at FROM d_dispatches WHERE task_id = ?")
        .bind(task_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn attempts(pool: &SqlitePool, task_id: &str) -> i64 {
    sqlx::query_scalar("SELECT wake_attempts FROM d_dispatches WHERE task_id = ?")
        .bind(task_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// 从台账读回待唤醒行，按投递侧的方式合成一条消息。
async fn composed_from_ledger(env: &TestEnv) -> String {
    let rows: Vec<DispatchRow> =
        dispatches::list_pending(&env.pool, wake::MAX_BATCH, wake::RETRY_BACKOFF)
            .await
            .unwrap();
    compose(&rows)
}

/// 建出 Peco 空间（含 `@master` 的 agent.md）并打开它，返回 `ws_id`。
async fn open_peco_space(env: &TestEnv, upstream: &FakeUpstream) -> String {
    install_team(env, "peco", upstream, &[fixture("@master", "master", &[])]);
    env.open_space("peco", "peco").await
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

/// 给夹具里的泵接上真实投递端（与 daemon 启动同一条装配路径）。
fn wire_delivery(env: &TestEnv, peco: Arc<PecoIdentity>) {
    env.wake.attach_delivery(Arc::new(PecoDelivery::new(
        env.pool.clone(),
        env.spaces(),
        Arc::new(ChatActiveRuns::new()),
        peco,
        Arc::downgrade(&env.env),
    )));
}

/// 等唤醒轮把 user 行落库：投递在 spawn 出的任务里写消息，tick 返回不代表已写完。
async fn wait_for_wake_message(env: &TestEnv) -> String {
    let deadline = std::time::Instant::now() + DELIVERY_DEADLINE;
    loop {
        let found: Option<String> =
            sqlx::query_scalar("SELECT content FROM messages WHERE role = 'user' LIMIT 1")
                .fetch_optional(&env.pool)
                .await
                .unwrap();
        if let Some(content) = found {
            return content;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{DELIVERY_DEADLINE:?} 内必须出现唤醒消息"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

// ============================================================================
// W2：非 Peco 来源不进队列
// ============================================================================

/// 判据必须看运行时解析出的身份，不能拿种子常量顶替 —— 这里的身份刻意为
/// `ws_recorded`（与 `PECO_WS_ID_SEED` 不同值），退回比种子就会把 Peco 自己的
/// 任务也判成「别人的」。
#[tokio::test]
async fn non_peco_origins_never_enter_the_wake_queue() {
    let env = new_env(&["research"]).await;
    let dir = env.work_dir("wake_space");
    let target_ws_id = env.resolve(&dir, "research").await.unwrap();
    let target_dir = env.work_dir_of(&target_ws_id).await;

    let peco = Arc::new(PecoIdentity {
        ws_id: "ws_recorded".to_string(),
        work_dir: env.home.join("spaces").join("peco").join("work"),
        team_id: "peco".to_string(),
        from_marker: true,
    });
    let tasks = Arc::new(TaskObserver::new(env.pool.clone(), env.wake.clone()));
    tasks.attach_peco(peco);
    let sink: &dyn TaskObservationSink = tasks.as_ref();

    let own = TaskId(Uuid::new_v4());
    sink.on_registered(
        &observation(target_dir.clone(), own, "ws_recorded"),
        &snapshot(own),
    )
    .await;
    let foreign = TaskId(Uuid::new_v4());
    sink.on_registered(
        &observation(target_dir, foreign, "ws_other"),
        &snapshot(foreign),
    )
    .await;

    assert_eq!(
        env.task_and_wake_state(&target_ws_id, &own.to_string())
            .await
            .1,
        "pending",
        "Peco 自己派出去的任务要进队列"
    );
    assert_eq!(
        env.wake_skip_reason(&target_ws_id, &own.to_string()).await,
        None,
        "进队列的行不写跳过原因"
    );
    assert_eq!(
        env.task_and_wake_state(&target_ws_id, &foreign.to_string())
            .await
            .1,
        "skipped",
        "别的空间的自有任务不是 Peco 的待办"
    );
    assert_eq!(
        env.wake_skip_reason(&target_ws_id, &foreign.to_string())
            .await
            .as_deref(),
        Some("not_peco_origin"),
        "另一个空间的来源要标成 not_peco_origin"
    );
    let pending = dispatches::list_pending(&env.pool, wake::MAX_BATCH, wake::RETRY_BACKOFF)
        .await
        .unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|row| row.task_id.clone())
            .collect::<Vec<_>>(),
        vec![own.to_string()],
        "投递侧只能看到那一行"
    );
}

// ============================================================================
// W3：孤儿行给固定文案
// ============================================================================

#[tokio::test]
async fn orphan_rows_get_the_fixed_digest_only() {
    let env = new_env(&[]).await;
    env.pending_row(1, "t-orphan", "orphan", Some("别的内容"))
        .await;

    let text = composed_from_ledger(&env).await;

    assert!(text.contains(ORPHAN_DIGEST), "缺固定文案：{text}");
    assert!(!text.contains("别的内容"), "旧摘要不得出现：{text}");
}

// ============================================================================
// W4：一个 tick 最多带 5 项
// ============================================================================

/// 真跑一次 tick：前 5 行送达，第 6/7 行留在队列里。
///
/// 判据必须落在「跑完一次投递之后」—— 没跑 tick 时「超出的行仍 pending」在任何
/// 实现下都成立（它们本来就是 pending）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tick_delivers_at_most_five_rows_and_leaves_the_rest_pending() {
    let env = new_env(&[]).await;
    let upstream =
        FakeUpstream::start(|_: Value| async move { Reply::Text("收到".to_string()) }).await;
    let ws_id = open_peco_space(&env, &upstream).await;
    wire_delivery(&env, peco_identity(&env, &ws_id).await);
    for seq in 1..=7 {
        env.peco_pending_row(seq, &format!("t-{seq}"), &ws_id).await;
    }

    env.wake.tick().await;

    for task_id in ["t-1", "t-2", "t-3", "t-4", "t-5"] {
        assert_eq!(
            wake_state(&env.pool, task_id).await,
            "delivered",
            "{task_id} 必须在这一批里"
        );
    }
    for task_id in ["t-6", "t-7"] {
        assert_eq!(
            wake_state(&env.pool, task_id).await,
            "pending",
            "{task_id} 超出一批，留给下一轮"
        );
    }

    let text = wait_for_wake_message(&env).await;
    assert!(
        text.starts_with("[peco-wake] 后台任务结果（5 项）"),
        "{text}"
    );
    for kept in ["t-1", "t-2", "t-3", "t-4", "t-5"] {
        assert!(text.contains(kept), "{kept} 必须在这一条里：{text}");
    }
    for left in ["t-6", "t-7"] {
        assert!(!text.contains(left), "{left} 超出上限：{text}");
    }
}

// ============================================================================
// W5：单项摘要 500 字符
// ============================================================================

#[tokio::test]
async fn digests_are_cut_at_five_hundred_chars() {
    let env = new_env(&[]).await;
    let digest = "x".repeat(600);
    env.pending_row(1, "t-long", "completed", Some(&digest))
        .await;

    let text = composed_from_ledger(&env).await;

    assert_eq!(text.matches('x').count(), wake::MAX_ITEM_CHARS);
}

// ============================================================================
// W6：尝试次数边界（8/9 → pending，10 → skipped）
// ============================================================================

#[tokio::test]
async fn attempts_below_the_limit_are_rescheduled_and_the_limit_is_skipped() {
    let env = new_env(&[]).await;
    for (seq, task_id) in [(1, "t-8"), (2, "t-9"), (3, "t-10")] {
        env.pending_row(seq, task_id, "failed", None).await;
    }
    arrange(&env.pool, "t-8", "failed", 8).await;
    arrange(&env.pool, "t-9", "failed", 9).await;
    arrange(&env.pool, "t-10", "failed", 10).await;

    let skipped = dispatches::skip_exhausted(&env.pool, wake::MAX_ATTEMPTS)
        .await
        .unwrap();
    let rescheduled = dispatches::reschedule_failed(&env.pool, wake::MAX_ATTEMPTS)
        .await
        .unwrap();

    assert_eq!(skipped, 1, "只有用尽上限的那行被跳过");
    assert_eq!(rescheduled, 2);
    assert_eq!(wake_state(&env.pool, "t-8").await, "pending");
    assert_eq!(wake_state(&env.pool, "t-9").await, "pending");
    assert_eq!(
        wake_state(&env.pool, "t-10").await,
        "skipped",
        "第 10 次尝试失败后不再重排"
    );
    assert_eq!(
        skip_reason(&env.pool, "t-10").await.as_deref(),
        Some("attempts_exhausted"),
        "次数用尽要自成一种跳过原因"
    );
}

// ============================================================================
// W7：启动回收
// ============================================================================

#[tokio::test]
async fn startup_reclaim_returns_every_claimed_row_to_pending() {
    let env = new_env(&[]).await;
    env.pending_row(1, "t-claimed", "completed", None).await;
    env.pending_row(2, "t-done", "completed", None).await;
    claim_seconds_ago(&env.pool, "t-claimed", 3_600).await;
    arrange(&env.pool, "t-done", "delivered", 1).await;

    let requeued = dispatches::requeue_all_delivering(&env.pool).await.unwrap();

    assert_eq!(requeued, 1, "只回收停在投递中的行");
    assert_eq!(wake_state(&env.pool, "t-claimed").await, "pending");
    assert_eq!(claimed_at(&env.pool, "t-claimed").await, None);
    assert_eq!(
        wake_state(&env.pool, "t-done").await,
        "delivered",
        "已送达的行不得被回收"
    );
}

// ============================================================================
// W8：60s 超时只回收旧的
// ============================================================================

#[tokio::test]
async fn only_stale_claims_are_reclaimed_by_the_timeout() {
    let env = new_env(&[]).await;
    env.pending_row(1, "t-stale", "completed", None).await;
    env.pending_row(2, "t-fresh", "completed", None).await;
    claim_seconds_ago(&env.pool, "t-stale", 61).await;
    claim_seconds_ago(&env.pool, "t-fresh", 30).await;

    let requeued = dispatches::requeue_stale_delivering(&env.pool, wake::DELIVERING_TIMEOUT)
        .await
        .unwrap();

    assert_eq!(requeued, 1);
    assert_eq!(wake_state(&env.pool, "t-stale").await, "pending");
    assert_eq!(
        wake_state(&env.pool, "t-fresh").await,
        "delivering",
        "还没到时限的认领仍属在途"
    );
}

// ============================================================================
// W10：并发认领只有一个赢家
// ============================================================================

#[tokio::test]
async fn claiming_the_same_row_twice_yields_one_delivery() {
    let env = new_env(&[]).await;
    env.pending_row(1, "t-claim", "completed", None).await;

    assert_eq!(
        dispatches::claim(&env.pool, "d_t-claim", wake::RETRY_BACKOFF)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        dispatches::claim(&env.pool, "d_t-claim", wake::RETRY_BACKOFF)
            .await
            .unwrap(),
        0,
        "第二次认领拿不到行"
    );

    assert_eq!(wake_state(&env.pool, "t-claim").await, "delivering");
    assert_eq!(attempts(&env.pool, "t-claim").await, 1, "一次尝试只计一次");
}

// ============================================================================
// W12：关闭空间的竞态
// ============================================================================

/// 关闭空间只把「还没投出去」的行停掉；在途与已送达的行留着唤醒态 —— 它们
/// 已经有一条消息在途或已送达，倒回 `skipped` 会让台账与事实分叉。
#[tokio::test]
async fn closing_a_space_keeps_the_wake_state_of_rows_already_in_flight() {
    let env = new_env(&[]).await;
    for (seq, task_id) in [
        (1, "t-delivering"),
        (2, "t-delivered"),
        (3, "t-pending"),
        (4, "t-failed"),
    ] {
        env.pending_row(seq, task_id, "working", None).await;
    }
    arrange(&env.pool, "t-delivering", "delivering", 1).await;
    arrange(&env.pool, "t-delivered", "delivered", 1).await;
    arrange(&env.pool, "t-failed", "failed", 1).await;

    let orphaned = dispatches::mark_orphan(&env.pool, "ws_target")
        .await
        .unwrap();

    assert_eq!(orphaned, 4, "孤儿化照旧按状态维度计数");
    for task_id in ["t-delivering", "t-delivered", "t-pending", "t-failed"] {
        let (task_state, _) = env.task_and_wake_state("ws_target", task_id).await;
        assert_eq!(task_state, "orphan", "{task_id} 必须被孤儿化");
    }
    assert_eq!(
        wake_state(&env.pool, "t-delivering").await,
        "delivering",
        "在途的行不改唤醒态"
    );
    assert_eq!(
        wake_state(&env.pool, "t-delivered").await,
        "delivered",
        "已送达的行不改唤醒态"
    );
    assert_eq!(
        wake_state(&env.pool, "t-pending").await,
        "skipped",
        "还没投出去的行不再唤醒"
    );
    // 只有「还没投出去」的两种态被关闭顶掉；在途与已送达保留原唤醒态。
    for task_id in ["t-pending", "t-failed"] {
        assert_eq!(
            skip_reason(&env.pool, task_id).await.as_deref(),
            Some("space_closed"),
            "{task_id} 的跳过原因由关闭给出"
        );
    }
    for task_id in ["t-delivering", "t-delivered"] {
        assert_eq!(
            skip_reason(&env.pool, task_id).await,
            None,
            "{task_id} 没被跳过，就不该有原因"
        );
    }
    let pending = dispatches::list_pending(&env.pool, wake::MAX_BATCH, wake::RETRY_BACKOFF)
        .await
        .unwrap();
    assert!(pending.is_empty(), "没有行留给投递侧");
}
