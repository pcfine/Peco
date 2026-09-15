// ============================================================================
// Peco 自动整理 opt-in 端点集成测试（查询 + 写入）
// ============================================================================
//
// 覆盖：
//   - GET  /api/peco/memory/consolidation/optin 默认 false（无行 = 未 opt-in）
//   - PUT  /api/peco/memory/consolidation/optin 开启 → 查询往返，落行含首开时刻
//   - 未认证 401（GET / PUT）
//   - 非法请求体 400（缺字段 / 类型不符 / 非法 JSON）

mod common;

use common::TestApp;
use serde_json::json;

const OPTIN_PATH: &str = "/api/peco/memory/consolidation/optin";

/// 默认 fail-closed：无行 = false，写入后往返一致。
#[tokio::test]
async fn test_optin_defaults_false_then_round_trips() {
    let app = TestApp::new().await;

    // 1. 未表态用户：无行 = 未 opt-in
    let resp = app.get(OPTIN_PATH).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["enabled"], json!(false), "无行必须 fail-closed");

    // 2. 开启
    let resp = app
        .put(OPTIN_PATH)
        .json(&json!({ "enabled": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["enabled"], json!(true), "PUT 返回同 GET 形态");

    // 3. 查询往返
    let resp = app.get(OPTIN_PATH).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["enabled"], json!(true));

    // 4. 落行核验：enabled=1 且记录首开时刻
    let row = sqlx::query_as::<_, (i64, Option<String>, String)>(
        "SELECT enabled, opted_in_at, updated_at FROM memory_consolidation_optin \
         WHERE user_id = ?",
    )
    .bind(&app.user_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(row.0, 1);
    assert!(row.1.is_some(), "开启应记录 opted_in_at");
    assert!(!row.2.is_empty(), "updated_at 不应为空");

    // 5. 关闭往返
    let resp = app
        .put(OPTIN_PATH)
        .json(&json!({ "enabled": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["enabled"], json!(false));

    let resp = app.get(OPTIN_PATH).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["enabled"], json!(false));
}

#[tokio::test]
async fn test_optin_endpoints_require_auth() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .get(format!("{}{}", app.base_url, OPTIN_PATH))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let resp = app
        .client
        .put(format!("{}{}", app.base_url, OPTIN_PATH))
        .json(&json!({ "enabled": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // 未认证的写入不得落行
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memory_consolidation_optin")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(count, 0, "未认证请求不得写入 opt-in");
}

#[tokio::test]
async fn test_optin_rejects_invalid_body_with_400() {
    let app = TestApp::new().await;

    // 缺字段
    let resp = app.put(OPTIN_PATH).json(&json!({})).send().await.unwrap();
    assert_eq!(resp.status(), 400);

    // 类型不符
    let resp = app
        .put(OPTIN_PATH)
        .json(&json!({ "enabled": "yes" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 非法 JSON
    let resp = app
        .put(OPTIN_PATH)
        .header("Content-Type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 非法请求不得落行（也不得留下 enabled=0 的残行）
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memory_consolidation_optin")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(count, 0, "非法请求体不得写入 opt-in");
}
