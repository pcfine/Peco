// ============================================================================
// Knowledge 集成测试 — 知识库 CRUD（轻量，不含重文件上传 pipeline）
// ============================================================================

mod common;

use common::TestApp;
use serde_json::json;

// ── 创建知识库 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_knowledge_base() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/knowledge")
        .json(&json!({
            "name": "技术文档",
            "description": "公司内部技术文档库"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "技术文档");
    assert_eq!(body["description"], "公司内部技术文档库");
    assert!(body["id"].as_str().is_some());
}

#[tokio::test]
async fn test_create_knowledge_base_empty_name() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/knowledge")
        .json(&json!({
            "name": "",
            "description": "test"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_create_knowledge_base_duplicate() {
    let app = TestApp::new().await;

    // 第一次：成功
    let resp = app
        .post("/api/knowledge")
        .json(&json!({ "name": "dup-kb" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // 第二次：409
    let resp = app
        .post("/api/knowledge")
        .json(&json!({ "name": "dup-kb" }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 409);
}

// ── 列表 ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_knowledge_bases() {
    let app = TestApp::new().await;

    // 创建 2 个知识库
    for name in ["kb-alpha", "kb-beta"] {
        let resp = app
            .post("/api/knowledge")
            .json(&json!({ "name": name }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
    }

    let resp = app.get("/api/knowledge").send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 2);
}

// ── 删除 ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_knowledge_base() {
    let app = TestApp::new().await;

    // 创建
    let create_resp = app
        .post("/api/knowledge")
        .json(&json!({ "name": "to-delete" }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let kb_id = created["id"].as_str().unwrap();

    // 删除
    let resp = app
        .delete(&format!("/api/knowledge/{}", kb_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 确认已删除
    let resp = app.get("/api/knowledge").send().await.unwrap();
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    let exists = body.iter().any(|k| k["id"] == kb_id);
    assert!(!exists);
}

// ── 文档列表（空知识库） ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_get_documents_empty() {
    let app = TestApp::new().await;

    // 创建知识库
    let create_resp = app
        .post("/api/knowledge")
        .json(&json!({ "name": "empty-kb" }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let kb_id = created["id"].as_str().unwrap();

    // 获取文档列表（空）
    let resp = app
        .get(&format!("/api/knowledge/{}/documents", kb_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

// ── 认证要求 ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_knowledge_endpoints_require_auth() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .get(format!("{}/api/knowledge", app.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}
