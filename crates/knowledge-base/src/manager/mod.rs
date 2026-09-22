pub mod config;
pub mod kb_manager;
pub mod knowledge_base;

pub use kb_manager::KnowledgeBaseManager;
pub use knowledge_base::KnowledgeBase;

// 测试所需导入（通过 `use super::*` 在 test 模块中可用）
#[cfg(test)]
use crate::error::KnowledgeError;
#[cfg(test)]
use crate::traits::*;
#[cfg(test)]
use crate::types::*;
#[cfg(test)]
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_use_kb_inmemory() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = KnowledgeBaseManager::load(tmp.path()).await.unwrap();

        let kb = mgr
            .create_kb(config::KbConfig {
                name: "test-kb".into(),
                description: "测试知识库".into(),
                embedding_model: config::FastembedModelTypeSerde::AllMiniLML6V2Q,
                chunking: config::ChunkingStrategySerde::FixedSize { size: 100 },
                backend: config::BackendType::InMemory,
                storage_path: None,
                default_storage_mode: Default::default(),
                helix_url: None,
            })
            .await
            .unwrap();

        // 添加文本
        let doc = kb
            .add_text("Test", "Rust is a systems programming language.", "test")
            .await
            .unwrap();
        assert_eq!(doc.title, "Test");
        assert!(doc.kb_id.is_some());

        // 搜索
        let results = kb.search("Rust programming", 3).await.unwrap();
        assert!(!results.is_empty());

        // 列出知识库
        let infos = mgr.list_kbs().await.unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "test-kb");
    }

    fn make_kb_config(name: &str) -> config::KbConfig {
        config::KbConfig {
            name: name.to_string(),
            description: "测试".into(),
            embedding_model: config::FastembedModelTypeSerde::AllMiniLML6V2Q,
            chunking: config::ChunkingStrategySerde::FixedSize { size: 100 },
            backend: config::BackendType::InMemory,
            storage_path: None,
            default_storage_mode: Default::default(),
            helix_url: None,
        }
    }

    /// `add_text` 必须填充 `created_at` 且为可解析的 ISO 8601 时刻，
    /// 作为 TTL 判定的可靠时间源。
    #[tokio::test]
    async fn add_text_fills_created_at_iso8601() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = KnowledgeBaseManager::load(tmp.path()).await.unwrap();
        let kb = mgr
            .create_kb(make_kb_config("created-at-test"))
            .await
            .unwrap();

        let before = chrono::Utc::now();
        let doc = kb
            .add_text("Test", "Rust is a systems programming language.", "test")
            .await
            .unwrap();
        let after = chrono::Utc::now();

        let created = doc
            .metadata
            .created_at
            .expect("add_text 必须填充 metadata.created_at");
        let parsed = chrono::DateTime::parse_from_rfc3339(&created)
            .expect("created_at 应为合法 ISO 8601 / RFC 3339")
            .with_timezone(&chrono::Utc);
        assert!(
            parsed >= before && parsed <= after,
            "created_at ({created}) 应落在写入时刻附近"
        );
    }

    #[tokio::test]
    async fn test_add_facts_and_query() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = KnowledgeBaseManager::load(tmp.path()).await.unwrap();
        let kb = mgr.create_kb(make_kb_config("facts-test")).await.unwrap();

        let facts = vec![
            Fact::new("用户", "prefers", "Rust", 0.9),
            Fact::new("用户", "has_skill", "Axum", 0.85),
            Fact::new("用户", "works_at", "某科技公司", 0.8),
        ];

        let stored = kb.add_facts(&facts, false).await.unwrap();
        assert_eq!(stored.len(), 3);

        // 查询实体事实
        let results = kb.query_entity_facts("用户", 2).await.unwrap();
        assert!(!results.is_empty());

        // 验证能遍历到客体节点
        let node_ids: Vec<&str> = results.iter().map(|s| s.node.id.as_str()).collect();
        let entity_id = compute_entity_id("Axum", "Entity");
        assert!(
            node_ids.contains(&entity_id.as_str()),
            "Should find Axum entity: {node_ids:?}"
        );
    }

    /// 实体 id 口径防漂移：`delete_fact` 匹配的边必须正是 `add_facts` 写进去的那条。
    ///
    /// 这是整个删除链路最容易出错的一点 —— 口径漂移的表现是**静默 no-op**
    /// （删掉 0 条但返回成功），而不是报错。
    #[tokio::test]
    async fn delete_fact_targets_the_edge_add_facts_wrote() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = KnowledgeBaseManager::load(tmp.path()).await.unwrap();
        let kb = mgr
            .create_kb(make_kb_config("fact-delete-test"))
            .await
            .unwrap();

        kb.add_facts(
            &[
                Fact::new("小C", "朋友", "chen", 0.95),
                Fact::new("小C", "年龄", "30", 0.9),
            ],
            false,
        )
        .await
        .unwrap();

        // 先读：确认按 (subject, predicate, object) 能读到，且带真实 weight
        let found = kb.read_fact("小C", "朋友", "chen").await.unwrap();
        assert_eq!(found.len(), 1, "读取必须命中 add_facts 写入的那条边");
        assert_eq!(found[0].source_id, compute_entity_id("小C", "Entity"));
        assert_eq!(found[0].target_id, compute_entity_id("chen", "Entity"));
        assert_eq!(found[0].edge_type, EdgeType::Custom("朋友".into()));
        assert!((found[0].weight - 0.95).abs() < 1e-6, "必须携带真实 weight");

        // 谓词不匹配 → 读不到（谓词是全匹配，无规范化）
        assert!(
            kb.read_fact("小C", "同事", "chen")
                .await
                .unwrap()
                .is_empty()
        );

        assert_eq!(kb.delete_fact("小C", "朋友", "chen").await.unwrap(), 1);

        // 删掉的那条没了，另一条完好
        assert!(
            kb.read_fact("小C", "朋友", "chen")
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(kb.read_fact("小C", "年龄", "30").await.unwrap().len(), 1);

        // 重复删除 → 0（工具层据此报错，而非静默成功）
        assert_eq!(kb.delete_fact("小C", "朋友", "chen").await.unwrap(), 0);
    }

    /// 删边后对端实体节点仍存活（孤儿行为），但它不再作为邻居出现在查询结果里。
    #[tokio::test]
    async fn delete_fact_leaves_the_peer_entity_alive() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = KnowledgeBaseManager::load(tmp.path()).await.unwrap();
        let kb = mgr
            .create_kb(make_kb_config("fact-orphan-test"))
            .await
            .unwrap();

        kb.add_facts(&[Fact::new("小C", "朋友", "chen", 0.95)], false)
            .await
            .unwrap();

        let chen_id = compute_entity_id("chen", "Entity");
        let before: Vec<String> = kb
            .query_entity_facts("小C", 1)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.node.id)
            .collect();
        assert!(before.contains(&chen_id));

        kb.delete_fact("小C", "朋友", "chen").await.unwrap();

        let after: Vec<String> = kb
            .query_entity_facts("小C", 1)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.node.id)
            .collect();
        assert!(!after.contains(&chen_id), "chen 不应再是邻居");

        // 节点本身仍在（删一条事实不该带走实体）
        let graph = kb.graph_store.as_ref().unwrap();
        assert!(
            graph.get_node(&chen_id).await.unwrap().is_some(),
            "对端实体节点应存活"
        );
    }

    /// `delete_entity(cascade: false)` 在实体仍有边时被拒；`cascade: true` 才删。
    #[tokio::test]
    async fn delete_entity_requires_cascade_when_relations_remain() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = KnowledgeBaseManager::load(tmp.path()).await.unwrap();
        let kb = mgr
            .create_kb(make_kb_config("entity-delete-test"))
            .await
            .unwrap();

        kb.add_facts(
            &[
                Fact::new("小C", "朋友", "chen", 0.95),
                Fact::new("小C", "年龄", "30", 0.9),
            ],
            false,
        )
        .await
        .unwrap();

        let err = kb.delete_entity("小C", false).await.unwrap_err();
        assert!(
            matches!(err, KnowledgeError::InvalidInput(_)),
            "有残留边时必须拒绝，实际 {err:?}"
        );
        // 拒绝后一切原样
        assert_eq!(kb.read_fact("小C", "朋友", "chen").await.unwrap().len(), 1);
        assert_eq!(kb.read_fact("小C", "年龄", "30").await.unwrap().len(), 1);

        let (edges, nodes) = kb.delete_entity("小C", true).await.unwrap();
        assert_eq!(edges, 2, "两条出边");
        assert_eq!(nodes, 1);

        assert!(
            kb.read_fact("小C", "朋友", "chen")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(kb.read_fact("小C", "年龄", "30").await.unwrap().is_empty());
        let graph = kb.graph_store.as_ref().unwrap();
        assert!(
            graph
                .get_node(&compute_entity_id("小C", "Entity"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_add_entities_and_relation_path() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = KnowledgeBaseManager::load(tmp.path()).await.unwrap();
        let kb = mgr.create_kb(make_kb_config("entity-test")).await.unwrap();

        // 添加实体（使用统一的 "Entity" 类型以匹配 add_facts）
        let person_id = compute_entity_id("张三", "Entity");
        let dept_id = compute_entity_id("技术部", "Entity");

        let entities = vec![
            Entity {
                id: person_id.clone(),
                name: "张三".into(),
                entity_type: "Entity".into(),
                source_chunk_id: String::new(),
                confidence: 1.0,
                properties: HashMap::new(),
            },
            Entity {
                id: dept_id.clone(),
                name: "技术部".into(),
                entity_type: "Entity".into(),
                source_chunk_id: String::new(),
                confidence: 1.0,
                properties: HashMap::new(),
            },
        ];
        kb.add_entities(&entities).await.unwrap();

        // 添加关系边
        let edges = vec![KnowledgeEdge {
            source_id: person_id.clone(),
            target_id: dept_id.clone(),
            edge_type: EdgeType::Custom("works_for".into()),
            weight: 0.9,
            properties: HashMap::new(),
        }];
        kb.add_relation_edges(&edges).await.unwrap();

        // 查询关系路径
        let path = kb
            .query_relation_path("张三", "技术部")
            .await
            .unwrap()
            .expect("path should exist");

        assert!(!path.is_empty());
        assert_eq!(path[0].node.id, person_id);
        assert_eq!(path.last().unwrap().node.id, dept_id);
    }
}
