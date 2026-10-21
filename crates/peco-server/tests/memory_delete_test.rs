// ============================================================================
// Peco 记忆删除端点集成测试（R1–R9 / R11）
// ============================================================================
//
// 覆盖：
//   - DELETE /api/peco/memory/documents/{id}   三索引同步 / 审计行 / profile 守卫 / 404 / 重复删
//   - DELETE /api/peco/memory/graph/nodes/{id} 级联边 / 未知 id 404 / 非实体 id 400 / 快照 / 回滚 409
//   - 认证与跨用户隔离

mod common;

use common::TestApp;
use knowledge_base::{BackendType, ChunkingStrategySerde, Fact, FastembedModelTypeSerde, KbConfig};
use peco_server::db;

const KB: &str = "@private_memory";

fn memory_kb_config() -> KbConfig {
    KbConfig {
        name: KB.to_string(),
        description: "测试记忆库".into(),
        embedding_model: FastembedModelTypeSerde::AllMiniLML6V2Q,
        chunking: ChunkingStrategySerde::FixedSize { size: 100 },
        backend: BackendType::InMemory,
        storage_path: None,
        default_storage_mode: Default::default(),
        helix_url: None,
    }
}

/// 建 @private_memory KB（幂等）。
async fn ensure_memory_kb(app: &TestApp) {
    let ws = app
        .state
        .workspace_manager
        .get_synced(&app.user_id, &app.state.db)
        .await
        .unwrap();
    let km = ws.knowledge_manager();
    km.ensure_loaded().await.unwrap();
    if km.list_kbs().await.unwrap().iter().all(|k| k.name != KB) {
        km.create_kb(memory_kb_config()).await.unwrap();
    }
}

/// 写一条记忆文档，返回 doc id。
async fn write_test_doc(app: &TestApp, title: &str, content: &str, source: &str) -> String {
    ensure_memory_kb(app).await;
    let ws = app
        .state
        .workspace_manager
        .get_synced(&app.user_id, &app.state.db)
        .await
        .unwrap();
    ws.knowledge_manager()
        .add_text_to_kb(KB, title, content, source)
        .await
        .unwrap()
        .id
}

/// 在图上真实写入测试事实。
async fn write_test_facts(app: &TestApp, facts: &[(&str, &str, &str, f32)]) {
    ensure_memory_kb(app).await;
    let facts: Vec<Fact> = facts
        .iter()
        .map(|(s, p, o, w)| Fact::new(*s, *p, *o, *w))
        .collect();
    let ws = app
        .state
        .workspace_manager
        .get_synced(&app.user_id, &app.state.db)
        .await
        .unwrap();
    ws.knowledge_manager()
        .add_facts_to_kb(KB, &facts, true)
        .await
        .unwrap();
}

/// 读一次记忆图谱。
async fn read_graph(app: &TestApp) -> serde_json::Value {
    let resp = app.get("/api/peco/memory/graph").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

/// 审计表里 `doc_id` 匹配（`None` = 全表）的行数 —— 直接查库，避免端点过滤口径干扰。
async fn audit_row_count(app: &TestApp, doc_id: Option<&str>) -> i64 {
    match doc_id {
        Some(id) => sqlx::query_scalar("SELECT count(*) FROM memory_audit WHERE doc_id = ?")
            .bind(id)
            .fetch_one(&app.state.db)
            .await
            .unwrap(),
        None => sqlx::query_scalar("SELECT count(*) FROM memory_audit")
            .fetch_one(&app.state.db)
            .await
            .unwrap(),
    }
}

/// 图上名为 `name` 的节点的稳定 id。
fn node_id_by_name(graph: &serde_json::Value, name: &str) -> String {
    graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == name)
        .unwrap_or_else(|| panic!("节点 {name} 不在图里: {graph}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

// ── R1 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_document_removes_all_indexes() {
    let app = TestApp::new().await;
    let content = "记忆删除测试正文 ZXQ-DEL-9";
    let doc_id = write_test_doc(&app, "memory_del_1", content, "ppa_semantic").await;

    let resp = app
        .get("/api/peco/memory/search?q=ZXQ-DEL-9")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["hits"].as_array().unwrap().len(), 1, "{body}");

    let resp = app
        .delete(&format!("/api/peco/memory/documents/{doc_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["doc_id"], doc_id.as_str());
    assert!(
        body["removed_chunks"].as_u64().unwrap() >= 1,
        "应报告清理的分块数: {body}"
    );

    let resp = app
        .get("/api/peco/memory/search?q=ZXQ-DEL-9")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["hits"].as_array().unwrap().len(), 0, "{body}");

    let resp = app
        .get(&format!("/api/peco/memory/documents/{doc_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ── R2 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_document_writes_done_audit_row() {
    let app = TestApp::new().await;
    let content = "审计行断言正文 ZXQ-DEL-2";
    let doc_id = write_test_doc(&app, "memory_del_2", content, "ppa_semantic").await;

    let resp = app
        .delete(&format!("/api/peco/memory/documents/{doc_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let audit_id = body["audit_id"].as_i64().unwrap();

    let row = db::memory_audit::get(&app.state.db, audit_id)
        .await
        .unwrap()
        .expect("审计行必须存在");
    assert_eq!(row.doc_id, doc_id);
    assert_eq!(row.reason, "user_delete");
    assert_eq!(row.deleted_by, "user");
    assert_eq!(row.status, "done");
    assert_eq!(row.content, content, "审计必须存原文（回滚重放依据）");
    assert_eq!(row.source, "ppa_semantic");

    let resp = app
        .get("/api/peco/memory/audit?reason=user_delete")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let rows: serde_json::Value = resp.json().await.unwrap();
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"].as_i64() == Some(audit_id)),
        "{rows}"
    );
}

// ── R3 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_profile_document_is_403_without_audit_row() {
    let app = TestApp::new().await;
    let doc_id = write_test_doc(&app, "memory_del_3", "画像记忆 ZXQ-DEL-3", "ppa_profile").await;

    let resp = app
        .delete(&format!("/api/peco/memory/documents/{doc_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "forbidden");

    let resp = app
        .get(&format!("/api/peco/memory/documents/{doc_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "profile 文档不得被删");

    assert_eq!(
        audit_row_count(&app, Some(&doc_id)).await,
        0,
        "profile 守卫必须在写审计之前"
    );
}

// ── R4 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_missing_document_is_404() {
    let app = TestApp::new().await;
    ensure_memory_kb(&app).await;
    let before = audit_row_count(&app, None).await;

    let resp = app
        .delete("/api/peco/memory/documents/no-such-doc")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "not_found");

    assert_eq!(
        audit_row_count(&app, None).await,
        before,
        "不存在的文档不写审计"
    );
}

// ── R5 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_same_document_twice_second_is_404() {
    let app = TestApp::new().await;
    let doc_id = write_test_doc(&app, "memory_del_5", "重复删除 ZXQ-DEL-5", "ppa_semantic").await;

    let first = app
        .delete(&format!("/api/peco/memory/documents/{doc_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);

    let second = app
        .delete(&format!("/api/peco/memory/documents/{doc_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 404, "重复删除是「已不存在」而不是内部错误");
}

// ── R6 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_entity_removes_edges() {
    let app = TestApp::new().await;
    write_test_facts(
        &app,
        &[("小C", "朋友", "chen", 0.95), ("小C", "同事", "lily", 0.9)],
    )
    .await;

    let graph = read_graph(&app).await;
    let entity_id = node_id_by_name(&graph, "小C");

    let resp = app
        .delete(&format!(
            "/api/peco/memory/graph/nodes/{}",
            urlencode(&entity_id)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["entity_id"], entity_id.as_str());
    assert_eq!(body["removed_edges"], 2, "两条出边必须级联带走: {body}");
    assert_eq!(body["removed_nodes"], 1);

    let after = read_graph(&app).await;
    assert!(
        after["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["id"] != entity_id.as_str()),
        "节点应已消失: {after}"
    );
    assert!(
        after["edges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["source"] != entity_id.as_str() && e["target"] != entity_id.as_str()),
        "任一端为该节点的边都应消失: {after}"
    );
}

// ── R7 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_entity_by_unknown_id_is_404() {
    let app = TestApp::new().await;
    ensure_memory_kb(&app).await;

    let resp = app
        .delete("/api/peco/memory/graph/nodes/entity:Entity:nope:deadbeef")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "删不存在的实体不得回成功");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "not_found");

    let resp = app
        .delete("/api/peco/memory/graph/nodes/not-an-entity-id")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "bad_request");
}

// ── R8 / R9 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_entity_writes_graph_fact_snapshot() {
    let app = TestApp::new().await;
    write_test_facts(
        &app,
        &[("小C", "朋友", "chen", 0.95), ("小C", "同事", "lily", 0.9)],
    )
    .await;

    let graph = read_graph(&app).await;
    let entity_id = node_id_by_name(&graph, "小C");

    let resp = app
        .delete(&format!(
            "/api/peco/memory/graph/nodes/{}",
            urlencode(&entity_id)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let audit_id = body["audit_id"].as_i64().unwrap();

    let row = db::memory_audit::get(&app.state.db, audit_id)
        .await
        .unwrap()
        .expect("审计行必须存在");
    assert_eq!(row.source, "graph_fact", "回滚分流按 source 判定");
    assert_eq!(row.doc_id, entity_id);
    assert!(row.title.starts_with("entity:"), "{}", row.title);
    assert_eq!(row.status, "done");
    assert_eq!(row.reason, "user_delete");
    assert_eq!(row.deleted_by, "user");

    let snapshot: serde_json::Value = serde_json::from_str(&row.content).unwrap();
    let edges = snapshot["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 2, "快照必须含全部关联边: {snapshot}");
    for e in edges {
        assert!(e["weight"].is_number(), "{e}");
        assert!(e["predicate"].is_string(), "{e}");
    }

    // R9：实体级联行天生不可自动回滚 —— 409 而不是 200/500
    let resp = app
        .post(&format!("/api/peco/memory/audit/{audit_id}/restore"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
}

// ── R11 ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_requires_auth() {
    let app = TestApp::new().await;
    let doc_id = write_test_doc(&app, "memory_del_11", "认证 ZXQ-DEL-11", "ppa_semantic").await;

    let resp = app
        .client
        .delete(format!(
            "{}/api/peco/memory/documents/{doc_id}",
            app.base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let resp = app
        .client
        .delete(format!(
            "{}/api/peco/memory/graph/nodes/entity:Entity:x:deadbeef",
            app.base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn delete_other_users_document_is_404() {
    let app = TestApp::new().await;
    let doc_id = write_test_doc(&app, "memory_del_12", "越权 ZXQ-DEL-12", "ppa_semantic").await;
    let (_user2, token2) = app.register_user2().await;

    let resp = app
        .delete_as(&format!("/api/peco/memory/documents/{doc_id}"), &token2)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "他人 workspace 里的 id 不可见");

    let resp = app
        .get(&format!("/api/peco/memory/documents/{doc_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "原用户的文档必须还在");
}

/// 路径参数编码 —— 实体 id 含 `:`，客户端必须 `encodeURIComponent`。
fn urlencode(s: &str) -> String {
    s.replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
}
