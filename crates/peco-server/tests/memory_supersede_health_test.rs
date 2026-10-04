// ============================================================================
// 取代对账健康端点 + 手动对账端点集成测试
// ============================================================================
//
// 覆盖：
//   - GET /api/peco/memory/supersede/health 无 token → 401（证路由存在）
//   - 带 token → 200 + 形状 {pending, processing, failed, degraded, last_converged_at}
//   - 直插 intent 行 → pending 计数可见；用户隔离（他人行不计入）
//   - POST /api/peco/memory/supersede/reconcile 401 / 触发收口 / 返回 health 形状

mod common;

use common::TestApp;
use knowledge_base::{BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig};
use peco_server::db;
use serde_json::json;

const HEALTH_PATH: &str = "/api/peco/memory/supersede/health";
const RECONCILE_PATH: &str = "/api/peco/memory/supersede/reconcile";
const MEMORY_KB: &str = "@private_memory";

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

/// 建 @private_memory KB（幂等）— 手动对账的 ensure-add 需要它存在。
async fn ensure_memory_kb(app: &TestApp) {
    let ws = app
        .state
        .workspace_manager
        .get_synced(&app.user_id, &app.state.db)
        .await
        .unwrap();
    let km = ws.knowledge_manager();
    km.ensure_loaded().await.unwrap();
    if km
        .list_kbs()
        .await
        .unwrap()
        .iter()
        .all(|k| k.name != MEMORY_KB)
    {
        km.create_kb(KbConfig {
            name: MEMORY_KB.to_string(),
            description: "测试记忆库".into(),
            embedding_model: FastembedModelTypeSerde::AllMiniLML6V2Q,
            chunking: ChunkingStrategySerde::FixedSize { size: 100 },
            backend: BackendType::InMemory,
            storage_path: None,
            default_storage_mode: Default::default(),
            helix_url: None,
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn supersede_reconcile_requires_auth() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .post(format!("{}{}", app.base_url, RECONCILE_PATH))
        .send()
        .await
        .unwrap();
    // 401（而非 404）证明路由已注册且挂在 JWT 层后
    assert_eq!(resp.status(), 401);
}

/// 手动对账端点：pending 意图 → POST → 收口为 done + health 形状返回。
/// 幂等重放的 ensure-add 按内容哈希落库（reconcile 的 add 走 add_text_to_kb，
/// doc id = text_doc_id(new_content)），非 intent 里虚构的 new_doc_id。
#[tokio::test]
async fn supersede_reconcile_converges_pending_intent() {
    let app = TestApp::new().await;
    ensure_memory_kb(&app).await;
    insert_pending_intent(&app, &app.user_id).await;

    // 收口前：pending 可见
    let resp = app.get(HEALTH_PATH).send().await.unwrap();
    let before: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(before["pending"], 1);

    let resp = app.post(RECONCILE_PATH).send().await.unwrap();
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
    assert_eq!(body["pending"], 0, "对账后意图应收口");
    assert_eq!(body["processing"], 0);
    assert_eq!(body["failed"], 0, "单次成功对账不得转 failed");
    assert!(
        body["last_converged_at"].is_string(),
        "本轮干净收口应更新 last_converged_at"
    );

    // ensure-add 已把新内容按内容哈希写进 KB（M1：old-doc 不在 = ③c 视为成功）
    let ws = app
        .state
        .workspace_manager
        .get_synced(&app.user_id, &app.state.db)
        .await
        .unwrap();
    let new_doc_id = knowledge_base::text_doc_id("新记忆内容");
    let doc = ws
        .knowledge_manager()
        .get_document(MEMORY_KB, &new_doc_id)
        .await
        .unwrap();
    assert!(
        doc.is_some(),
        "对账 ensure-add 应按内容哈希 {new_doc_id} 落库新内容"
    );
}
