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
            "name": "tech-docs",
            "description": "公司内部技术文档库"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "tech-docs");
    assert_eq!(body["description"], "公司内部技术文档库");
    assert!(body["id"].as_str().is_some());
}

#[tokio::test]
async fn test_create_knowledge_base_rejects_non_ascii_name() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/knowledge")
        .json(&json!({ "name": "技术文档" }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["details"].as_str().is_some_and(|d| !d.is_empty()));
}

#[tokio::test]
async fn test_create_knowledge_base_rejects_reserved_prefix() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/knowledge")
        .json(&json!({ "name": "@private_memory" }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["details"].as_str().is_some_and(|d| d.contains('@')),
        "details 应说明 @ 为保留前缀: {}",
        body["details"]
    );
}

#[tokio::test]
async fn test_create_knowledge_base_rejects_overlong_name() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/knowledge")
        .json(&json!({ "name": "a".repeat(65) }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_create_knowledge_base_rejects_trailing_dot_name() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/knowledge")
        .json(&json!({ "name": "kb." }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_create_knowledge_base_accepts_symbol_name() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/knowledge")
        .json(&json!({ "name": "kb_01.v2-test" }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
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

// ── backend / embedding_model 取自 KbInfo（D4 回归） ─────────────────────────

/// `backend` / `embedding_model` 必须报 `kb_config.json` 的真实值，而非硬编码。
/// 需可用的 HelixDB 端点（`PECO_KB_HELIX_URL`，回退 `http://localhost:6970`，跑测试时
/// 指向一次性实例）。非默认嵌入模型用于区分真值与旧的硬编码 `BGEBaseZHV15`。
#[tokio::test]
async fn get_kb_reports_real_backend() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/knowledge")
        .json(&json!({
            "name": "backend-label-kb",
            "embedding_model": "all-minilm-l6-v2q"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let created: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(created["backend"], "HelixDB", "创建响应应报真实后端");
    let kb_id = created["id"].as_str().unwrap();

    // 详情
    let resp = app
        .get(&format!("/api/knowledge/{kb_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["backend"], "HelixDB");
    assert_eq!(body["embedding_model"], "AllMiniLML6V2Q");

    // 列表
    let resp = app.get("/api/knowledge").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let list: Vec<serde_json::Value> = resp.json().await.unwrap();
    let item = list
        .iter()
        .find(|k| k["id"] == kb_id)
        .expect("列表应包含刚建的知识库");
    assert_eq!(item["backend"], "HelixDB");
    assert_eq!(item["embedding_model"], "AllMiniLML6V2Q");
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
