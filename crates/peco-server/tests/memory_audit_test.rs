// ============================================================================
// Peco 记忆审计端点集成测试（查询 + 回滚）
// ============================================================================
//
// 覆盖：
//   - GET  /api/peco/memory/audit 分页 + 用户隔离 + 认证
//   - POST /api/peco/memory/audit/:id/restore 真实回滚（写入 → 删除 → 审计 done → 回滚复原）
//   - 越权（他人审计行）404；pending 行与重复回滚 409；知识库缺失 404

mod common;

use common::TestApp;
use knowledge_base::{BackendType, ChunkingStrategySerde, Fact, FastembedModelTypeSerde, KbConfig};
use peco_core::tools::MemoryAuditEntry;
use peco_server::db;
use serde_json::json;

const KB: &str = "@private_memory";

// ── graph_fact 回滚用例共享的图事实 ────────────────────────────────────────
const FACT_SUBJECT: &str = "小C";
const FACT_PREDICATE: &str = "喜欢";
const FACT_OBJECT: &str = "rust";
const FACT_WEIGHT: f32 = 0.95;

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

/// 在用户 workspace 中建 @private_memory KB，写入一条记忆并真实删除，
/// 落一条 done 审计行（与删除工具收口后的形态一致）。
/// 返回 (doc_id, 审计行 id)。
async fn seed_deleted_memory(app: &TestApp, content: &str) -> (String, i64) {
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

    let doc = km
        .add_text_to_kb(KB, "memory_1", content, "ppa_semantic")
        .await
        .unwrap();
    let report = km.delete_document(KB, &doc.id).await.unwrap();
    assert_eq!(report.doc_id, doc.id);

    let audit_id = db::memory_audit::insert_pending(
        &app.state.db,
        &MemoryAuditEntry {
            user_id: app.user_id.clone(),
            kb_name: KB.into(),
            doc_id: doc.id.clone(),
            title: "memory_1".into(),
            content: content.into(),
            source: "ppa_semantic".into(),
            reason: "manual_organize".into(),
            deleted_by: "agent:@memory".into(),
            deleted_at: "2026-09-10T00:00:00+00:00".into(),
            topic_key: None,
            successor_doc_id: None,
        },
    )
    .await
    .unwrap();
    db::memory_audit::mark_done(&app.state.db, audit_id)
        .await
        .unwrap();

    (doc.id, audit_id)
}

#[tokio::test]
async fn test_audit_list_shows_done_row_and_restore_roundtrip() {
    let app = TestApp::new().await;
    let content = "用户偏好 Rust 语言";
    let (doc_id, audit_id) = seed_deleted_memory(&app, content).await;

    // ── 查询：当前用户可见该 done 行（含被删原文）────────────────────────
    let resp = app.get("/api/peco/memory/audit").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let items: Vec<serde_json::Value> = resp.json().await.unwrap();
    let item = items
        .iter()
        .find(|i| i["id"].as_i64() == Some(audit_id))
        .expect("done 行应可见");
    assert_eq!(item["status"], "done");
    assert_eq!(item["doc_id"], doc_id.as_str());
    assert_eq!(item["content"], content);
    assert_eq!(item["deleted_by"], "agent:@memory");
    // 未回滚行不透出 restored 字段
    assert!(item.get("restored_at").is_none());

    // ── 回滚：内容哈希幂等 → doc_id 复原 ────────────────────────────────
    let resp = app
        .post(&format!("/api/peco/memory/audit/{audit_id}/restore"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["doc_id"], doc_id.as_str());
    assert_eq!(body["success"], true);

    // KB 中文档已恢复（同 id、同内容、同 source）
    let ws = app
        .state
        .workspace_manager
        .get_synced(&app.user_id, &app.state.db)
        .await
        .unwrap();
    let doc = ws
        .knowledge_manager()
        .get_document(KB, &doc_id)
        .await
        .unwrap()
        .expect("回滚后文档应存在");
    assert_eq!(doc.content, content);
    assert_eq!(doc.source_path, "ppa_semantic");

    // 审计行已回填 restored_at / restored_doc_id
    let row = db::memory_audit::get(&app.state.db, audit_id)
        .await
        .unwrap()
        .unwrap();
    assert!(row.restored_at.is_some());
    assert_eq!(row.restored_doc_id.as_deref(), Some(doc_id.as_str()));

    // 重复回滚 → 409
    let resp = app
        .post(&format!("/api/peco/memory/audit/{audit_id}/restore"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
}

#[tokio::test]
async fn test_audit_list_paginates() {
    let app = TestApp::new().await;
    seed_deleted_memory(&app, "第一条记忆内容足够长").await;
    seed_deleted_memory(&app, "第二条记忆内容也足够长").await;

    let resp = app
        .get("/api/peco/memory/audit?limit=1&offset=0")
        .send()
        .await
        .unwrap();
    let page1: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(page1.len(), 1);

    let resp = app
        .get("/api/peco/memory/audit?limit=1&offset=1")
        .send()
        .await
        .unwrap();
    let page2: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(page2.len(), 1);
    assert_ne!(page1[0]["id"], page2[0]["id"]);
}

#[tokio::test]
async fn test_audit_isolated_per_user_and_cross_user_restore_is_404() {
    let app = TestApp::new().await;
    let (_, audit_id) = seed_deleted_memory(&app, "用户偏好独处").await;

    let (_user2_id, token2) = app.register_user2().await;

    // 他人列表不可见
    let resp = app
        .get_as("/api/peco/memory/audit", &token2)
        .send()
        .await
        .unwrap();
    let items: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(
        items.iter().all(|i| i["id"].as_i64() != Some(audit_id)),
        "审计行不得跨用户可见"
    );

    // 他人回滚 → 404（不泄露记录存在性）
    let resp = app
        .client
        .post(format!(
            "{}/api/peco/memory/audit/{audit_id}/restore",
            app.base_url
        ))
        .bearer_auth(&token2)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_restore_rejects_pending_row() {
    let app = TestApp::new().await;
    // pending 是未决的删除流程，无原文可恢复语义之外的状态 — 直接拒绝
    let audit_id = db::memory_audit::insert_pending(
        &app.state.db,
        &MemoryAuditEntry {
            user_id: app.user_id.clone(),
            kb_name: KB.into(),
            doc_id: "doc-pending".into(),
            title: "memory_x".into(),
            content: "内容".into(),
            source: "ppa_semantic".into(),
            reason: "manual_organize".into(),
            deleted_by: "agent:@memory".into(),
            deleted_at: "2026-09-10T00:00:00+00:00".into(),
            topic_key: None,
            successor_doc_id: None,
        },
    )
    .await
    .unwrap();

    let resp = app
        .post(&format!("/api/peco/memory/audit/{audit_id}/restore"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
}

#[tokio::test]
async fn test_restore_missing_kb_returns_404() {
    let app = TestApp::new().await;
    // done 行指向不存在的知识库 → 回滚在重放阶段 404
    let audit_id = db::memory_audit::insert_pending(
        &app.state.db,
        &MemoryAuditEntry {
            user_id: app.user_id.clone(),
            kb_name: "@no_such_kb".into(),
            doc_id: "doc-gone".into(),
            title: "memory_1".into(),
            content: "内容".into(),
            source: "ppa_semantic".into(),
            reason: "manual_organize".into(),
            deleted_by: "agent:@memory".into(),
            deleted_at: "2026-09-10T00:00:00+00:00".into(),
            topic_key: None,
            successor_doc_id: None,
        },
    )
    .await
    .unwrap();
    db::memory_audit::mark_done(&app.state.db, audit_id)
        .await
        .unwrap();

    let resp = app
        .post(&format!("/api/peco/memory/audit/{audit_id}/restore"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_restore_missing_row_returns_404() {
    let app = TestApp::new().await;
    let resp = app
        .post("/api/peco/memory/audit/999/restore")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_audit_endpoints_require_auth() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .get(format!("{}/api/peco/memory/audit", app.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let resp = app
        .client
        .post(format!("{}/api/peco/memory/audit/1/restore", app.base_url))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

// ============================================================================
// graph_fact 回滚（restore 端点 source == "graph_fact" 分支）
// ============================================================================

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

/// graph_fact 审计快照 content — 与删除工具 `fact_snapshot_json` 同形。
fn fact_snapshot_content() -> String {
    json!({
        "subject": FACT_SUBJECT,
        "predicate": FACT_PREDICATE,
        "object": FACT_OBJECT,
        "edges": [{
            "source_id": "entity:Entity:0000000000000000",
            "target_id": "entity:Entity:1111111111111111",
            "weight": FACT_WEIGHT,
            "properties": {},
        }],
    })
    .to_string()
}

/// 落一条 `source = "graph_fact"` 的 done 审计行，**不**改动图上状态
/// （建 KB 幂等；图上事实的写入/删除由各用例自行控制）。
async fn seed_graph_audit(app: &TestApp, doc_id: &str, content: &str) -> i64 {
    ensure_memory_kb(app).await;
    let audit_id = db::memory_audit::insert_pending(
        &app.state.db,
        &MemoryAuditEntry {
            user_id: app.user_id.clone(),
            kb_name: KB.into(),
            doc_id: doc_id.into(),
            title: format!("{FACT_SUBJECT} -[{FACT_PREDICATE}]-> {FACT_OBJECT}"),
            content: content.into(),
            source: "graph_fact".into(),
            reason: "manual_organize".into(),
            deleted_by: "agent:@memory".into(),
            deleted_at: "2026-09-10T00:00:00+00:00".into(),
            topic_key: None,
            successor_doc_id: None,
        },
    )
    .await
    .unwrap();
    db::memory_audit::mark_done(&app.state.db, audit_id)
        .await
        .unwrap();
    audit_id
}

/// 在图上真实写入测试事实（与 `add_facts_to_knowledge_base` 工具默认一致，index_text=true）。
async fn write_test_fact(app: &TestApp) {
    let ws = app
        .state
        .workspace_manager
        .get_synced(&app.user_id, &app.state.db)
        .await
        .unwrap();
    ws.knowledge_manager()
        .add_facts_to_kb(
            KB,
            &[Fact::new(
                FACT_SUBJECT,
                FACT_PREDICATE,
                FACT_OBJECT,
                FACT_WEIGHT,
            )],
            true,
        )
        .await
        .unwrap();
}

/// 全量回滚：事实已删 → restore 200 恢复（weight 还原）→ 回填 → 再回滚 409。
#[tokio::test]
async fn test_restore_graph_fact_roundtrip() {
    let app = TestApp::new().await;
    let doc_id = Fact::compute_id(FACT_SUBJECT, FACT_PREDICATE, FACT_OBJECT);
    let audit_id = seed_graph_audit(&app, &doc_id, &fact_snapshot_content()).await;

    // 模拟删除工具收口后的真实状态：图上写入再删除
    write_test_fact(&app).await;
    let ws = app
        .state
        .workspace_manager
        .get_synced(&app.user_id, &app.state.db)
        .await
        .unwrap();
    let km = ws.knowledge_manager();
    let removed = km
        .delete_fact(KB, FACT_SUBJECT, FACT_PREDICATE, FACT_OBJECT)
        .await
        .unwrap();
    assert!(removed > 0, "前置：事实应已删除");
    assert!(
        km.read_fact(KB, FACT_SUBJECT, FACT_PREDICATE, FACT_OBJECT)
            .await
            .unwrap()
            .is_empty()
    );

    // 回滚 → 200，事实恢复且 weight 还原
    let resp = app
        .post(&format!("/api/peco/memory/audit/{audit_id}/restore"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], true);
    assert_eq!(body["doc_id"], doc_id.as_str());

    let edges = km
        .read_fact(KB, FACT_SUBJECT, FACT_PREDICATE, FACT_OBJECT)
        .await
        .unwrap();
    assert_eq!(edges.len(), 1, "重放应恰好补回一条边");
    assert!(
        (edges[0].weight - FACT_WEIGHT).abs() < 1e-6,
        "weight 应按快照还原，实际 {}",
        edges[0].weight
    );

    // 审计行回填 restored_at / restored_doc_id
    let row = db::memory_audit::get(&app.state.db, audit_id)
        .await
        .unwrap()
        .unwrap();
    assert!(row.restored_at.is_some());
    assert_eq!(row.restored_doc_id.as_deref(), Some(doc_id.as_str()));

    // 重复回滚 → 409（restored_at 守卫），图上仍只有 1 条边
    let resp = app
        .post(&format!("/api/peco/memory/audit/{audit_id}/restore"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    assert_eq!(
        km.read_fact(KB, FACT_SUBJECT, FACT_PREDICATE, FACT_OBJECT)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// 幂等门：事实**仍在**（并发重建 / 人工重放后）→ restore 200 且不产生并行边。
#[tokio::test]
async fn test_restore_graph_fact_existing_fact_skips_replay() {
    let app = TestApp::new().await;
    let doc_id = Fact::compute_id(FACT_SUBJECT, FACT_PREDICATE, FACT_OBJECT);
    let audit_id = seed_graph_audit(&app, &doc_id, &fact_snapshot_content()).await;

    // 事实已被外部重建（不在 restore 流程内）
    write_test_fact(&app).await;

    let resp = app
        .post(&format!("/api/peco/memory/audit/{audit_id}/restore"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let ws = app
        .state
        .workspace_manager
        .get_synced(&app.user_id, &app.state.db)
        .await
        .unwrap();
    let edges = ws
        .knowledge_manager()
        .read_fact(KB, FACT_SUBJECT, FACT_PREDICATE, FACT_OBJECT)
        .await
        .unwrap();
    assert_eq!(
        edges.len(),
        1,
        "存在性门必须跳过重放，不得累积并行边（评审 D2）"
    );
}

/// 实体级联行（entity: doc_id）→ 409 拒绝，且 KB 不产生任何文档（无污染）。
#[tokio::test]
async fn test_restore_graph_fact_entity_row_rejected_without_pollution() {
    let app = TestApp::new().await;
    let content = json!({
        "entity_name": "小C",
        "entity_id": "entity:Entity:0000000000000000",
        "node": null,
        "edges": [],
    })
    .to_string();
    let audit_id = seed_graph_audit(&app, "entity:Entity:0000000000000000", &content).await;

    let resp = app
        .post(&format!("/api/peco/memory/audit/{audit_id}/restore"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);

    let ws = app
        .state
        .workspace_manager
        .get_synced(&app.user_id, &app.state.db)
        .await
        .unwrap();
    let km = ws.knowledge_manager();
    let docs = km.list_documents(KB, 0, 100).await.unwrap();
    assert!(
        docs.is_empty(),
        "拒绝路径不得把快照 JSON 灌成文档（污染），实际 {} 篇",
        docs.len()
    );
}

/// 损坏快照三连：非 JSON / 缺字段 / doc_id 不匹配 → 409，全部零写入。
#[tokio::test]
async fn test_restore_graph_fact_corrupt_snapshots_rejected() {
    let app = TestApp::new().await;
    ensure_memory_kb(&app).await;

    // 第三例：快照本身合法，但 doc_id 与三元组算得的 id 不一致
    let mismatch_content = fact_snapshot_content();
    let cases: Vec<(&str, &str)> = vec![
        ("fact:aaaaaaaaaaaaaaaa", "这不是 JSON"),
        ("fact:bbbbbbbbbbbbbbbb", r#"{"subject": "小C"}"#),
        ("fact:cccccccccccccccc", &mismatch_content),
    ];

    for (doc_id, content) in cases {
        let audit_id = seed_graph_audit(&app, doc_id, content).await;
        let resp = app
            .post(&format!("/api/peco/memory/audit/{audit_id}/restore"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            409,
            "doc_id={doc_id} 的损坏快照必须先于写入被拒绝"
        );
    }

    // 三次拒绝零写入：图上无事实、KB 无文档
    let ws = app
        .state
        .workspace_manager
        .get_synced(&app.user_id, &app.state.db)
        .await
        .unwrap();
    let km = ws.knowledge_manager();
    assert!(
        km.read_fact(KB, FACT_SUBJECT, FACT_PREDICATE, FACT_OBJECT)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(km.list_documents(KB, 0, 100).await.unwrap().is_empty());
}
