// ============================================================================
// 取代对账健康端点集成测试
// ============================================================================
//
// 覆盖：
//   - GET /api/peco/memory/supersede/health 无 token → 401（证路由存在）
//   - 带 token → 200 + 形状 {pending, processing, failed, degraded, last_converged_at}
//   - 直插 intent 行 → pending 计数可见；用户隔离（他人行不计入）

mod common;

use common::TestApp;
use peco_server::db;
use serde_json::json;

const HEALTH_PATH: &str = "/api/peco/memory/supersede/health";

/// 为指定用户插一条 pending 意图（新条不在 KB、旧条虚构 —— 仅计数用途）。
async fn insert_pending_intent(app: &TestApp, user_id: &str) {
    let now = chrono::Utc::now().to_rfc3339();
    db::memory_supersede::write_intent(
        &app.state.db,
        &db::memory_supersede::IntentRow {
            user_id: user_id.to_string(),
            kb_name: "@private_memory".to_string(),
            topic_key: Some("answer_style".to_string()),
            old_doc_id: "old-doc".to_string(),
            old_title: "old_title".to_string(),
            old_source: "ppa_profile".to_string(),
            new_doc_id: "new-doc".to_string(),
            new_title: "new_title".to_string(),
            new_content: "新记忆内容".to_string(),
            new_source: "ppa_profile".to_string(),
            created_at: now.clone(),
            updated_at: now,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn supersede_health_requires_auth() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .get(format!("{}{}", app.base_url, HEALTH_PATH))
        .send()
        .await
        .unwrap();
    // 401（而非 404）证明路由已注册且挂在 JWT 层后
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn supersede_health_returns_full_shape_and_pending_count() {
    let app = TestApp::new().await;
    insert_pending_intent(&app, &app.user_id).await;

    let resp = app.get(HEALTH_PATH).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    for key in [
        "pending",
        "processing",
        "failed",
        "degraded",
        "last_converged_at",
    ] {
        assert!(body.get(key).is_some(), "缺少字段 {key}，实际: {body}");
    }
    assert_eq!(body["pending"], 1, "直插的 pending 行应被计入");
    assert_eq!(body["processing"], 0);
    assert_eq!(body["failed"], 0);
    // degraded 是进程级计数：本测试进程内无 enforce 运行，断言形状而非定值
    assert!(
        body["degraded"].as_i64().unwrap_or(-1) >= 0,
        "degraded 应为非负整数"
    );
    assert!(
        body["last_converged_at"].is_null() || body["last_converged_at"].is_string(),
        "last_converged_at 应为 null 或 RFC 3339 字符串"
    );
}

#[tokio::test]
async fn supersede_health_is_user_scoped() {
    let app = TestApp::new().await;
    let (other_id, other_token) = app.register_user2().await;
    insert_pending_intent(&app, &other_id).await;

    // 当前用户：他人意图不计入
    let resp = app.get(HEALTH_PATH).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["pending"], 0, "健康计数应按 JWT sub 隔离");

    // 他人视角：计入自己的行
    let resp = app.get_as(HEALTH_PATH, &other_token).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["pending"], 1);

    // 形状稳定性：序列化键集与 JSON 文档一致
    assert_eq!(
        body.as_object().unwrap().keys().len(),
        json!({
            "pending": 0,
            "processing": 0,
            "failed": 0,
            "degraded": 0,
            "last_converged_at": null
        })
        .as_object()
        .unwrap()
        .keys()
        .len(),
        "响应键数应为 5"
    );
}
