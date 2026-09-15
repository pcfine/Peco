// ============================================================================
// 自动整理触发面与观测面集成测试
// ============================================================================
//
// 覆盖：
//   - POST /api/peco/memory/consolidate 异步受理（202 + status/user_id）
//   - 总开关关闭时仍返回既有 Disabled 形态（200）
//   - GET  /api/peco/memory/consolidation/state 无行全 null / 有行返回值 / 用户隔离
//   - cron 注册随总开关：关闭 → job_count 不变；开启 → +1

mod common;

use common::TestApp;
use peco_server::peco::memory::cron::{JOB_NAME, register};

const CONSOLIDATE_PATH: &str = "/api/peco/memory/consolidate";
const STATE_PATH: &str = "/api/peco/memory/consolidation/state";

// ── 手动触发（202 受理）────────────────────────────────────────────────────

#[tokio::test]
async fn consolidate_now_accepts_and_returns_202() {
    let app = TestApp::new_with_consolidation(true).await;

    let resp = app.post(CONSOLIDATE_PATH).send().await.unwrap();
    assert_eq!(resp.status(), 202, "整理为分钟级任务，改异步受理");

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "accepted");
    assert_eq!(
        body["user_id"], app.user_id,
        "受理体回报发起用户，便于前端对账后台任务"
    );
}

#[tokio::test]
async fn consolidate_now_returns_disabled_shape_when_switch_off() {
    let app = TestApp::new().await;

    let resp = app.post(CONSOLIDATE_PATH).send().await.unwrap();
    assert_eq!(resp.status(), 200, "总开关关闭不是错误，沿用既有 200");

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["enabled"], false);
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("未开启"),
        "应说明关闭原因，实际: {body}"
    );
}

// ── 观测端点（最近一次运行结果）────────────────────────────────────────────

#[tokio::test]
async fn consolidation_state_is_all_null_before_first_run() {
    let app = TestApp::new().await;

    let resp = app.get(STATE_PATH).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["last_run_at"].is_null(), "无行 = 从未整理过");
    assert!(body["last_run_stats"].is_null());
}

#[tokio::test]
async fn consolidation_state_returns_last_run_and_is_user_scoped() {
    let app = TestApp::new().await;
    let (other_id, other_token) = app.register_user2().await;

    peco_server::db::memory_consolidation_state::upsert_state(
        &app.state.db,
        &app.user_id,
        Some("2026-09-15T00:00:00+00:00"),
        Some("2026-09-15T00:01:00+00:00"),
        Some(r#"{"scanned":20,"merged":2}"#),
    )
    .await
    .unwrap();

    let body: serde_json::Value = app
        .get(STATE_PATH)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["last_run_at"], "2026-09-15T00:01:00+00:00");
    assert_eq!(
        body["last_run_stats"]["scanned"], 20,
        "统计以 JSON 对象回传（前端观测口直读字段）"
    );
    assert_eq!(body["last_run_stats"]["merged"], 2);

    // 用户隔离：另一用户无行 → 全 null
    let other: serde_json::Value = app
        .get_as(STATE_PATH, &other_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        other["last_run_at"].is_null(),
        "用户 {other_id} 不应看到他人水位"
    );
    assert!(other["last_run_stats"].is_null());
}

// ── cron 注册随总开关 ──────────────────────────────────────────────────────

#[tokio::test]
async fn cron_register_is_noop_when_switch_off() {
    let app = TestApp::new().await;
    let before = app.state.cron_scheduler.job_count().await;

    register(&app.state).await.unwrap();

    assert_eq!(
        app.state.cron_scheduler.job_count().await,
        before,
        "总开关关闭时不注册任务（零开销）"
    );
    assert!(!app.state.cron_scheduler.contains_job(JOB_NAME).await);
}

#[tokio::test]
async fn cron_register_adds_one_job_when_switch_on() {
    let app = TestApp::new_with_consolidation(true).await;
    let before = app.state.cron_scheduler.job_count().await;

    register(&app.state).await.unwrap();

    assert_eq!(
        app.state.cron_scheduler.job_count().await,
        before + 1,
        "总开关开启时注册一个整理任务"
    );
    assert!(app.state.cron_scheduler.contains_job(JOB_NAME).await);
}
