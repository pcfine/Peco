pub mod config;
pub mod kb_manager;
pub mod knowledge_base;

pub use kb_manager::KnowledgeBaseManager;
pub use knowledge_base::KnowledgeBase;

// 测试所需导入（通过 `use super::*` 在 test 模块中可用）
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
        }
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
