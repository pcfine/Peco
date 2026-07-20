//! 知识库管理器 — 多知识库的创建、管理和查询入口。
//!
//! # 架构
//!
//! ```text
//! KnowledgeBaseManager
//! ├── KnowledgeBase "技术文档"     (LanceDB, BGE-small-zh)
//! ├── KnowledgeBase "法律合同"     (LanceDB, BGE-large-zh)
//! └── KnowledgeBase "代码库"       (InMemory, AllMiniLML6V2Q)
//! ```
//!
//! 每个 KnowledgeBase 封装了解析器、分块器、嵌入引擎、存储后端和检索引擎。

pub mod config;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::Digest;
use tokio::sync::RwLock;
use tracing::info;

use crate::chunking::make_chunker;
use crate::embedding::{FastembedEngine, FastembedModelType};
use crate::engine::{HybridSearchEngine, IngestionPipeline};
use crate::error::KnowledgeError;
use crate::parsers::make_parser;
use crate::traits::*;
use crate::types::*;

use self::config::{BackendType, KbConfig, KbConfigsFile, KbInfo};

/// 存储后端组件：(文档存储, 向量索引, 全文索引, 图存储)
type BackendComponents = (
    Arc<dyn DocumentStore>,
    Option<Arc<dyn VectorIndex>>,
    Option<Arc<dyn FullTextIndex>>,
    Option<Arc<dyn GraphStore>>,
);

// ---------------------------------------------------------------------------
// KnowledgeBaseManager
// ---------------------------------------------------------------------------

/// 知识库管理器 — 统一入口，管理多个知识库实例。
pub struct KnowledgeBaseManager {
    /// 知识库实例映射：kb_name → KnowledgeBase
    instances: RwLock<HashMap<String, Arc<KnowledgeBase>>>,
    /// 配置映射
    configs: RwLock<HashMap<String, KbConfig>>,
    /// 数据根目录
    base_dir: PathBuf,
    /// 配置文件路径
    config_path: PathBuf,
}

impl KnowledgeBaseManager {
    /// 从指定目录加载所有知识库配置。
    ///
    /// 若配置文件不存在，将创建空配置。
    pub async fn load(base_dir: &Path) -> Result<Self, KnowledgeError> {
        tokio::fs::create_dir_all(base_dir)
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("无法创建目录: {e}")))?;

        let config_path = base_dir.join("kb_configs.json");
        let configs: KbConfigsFile = if config_path.exists() {
            let data = tokio::fs::read_to_string(&config_path)
                .await
                .map_err(|e| KnowledgeError::InvalidInput(format!("读取配置失败: {e}")))?;
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            KbConfigsFile::default()
        };

        let mut config_map = HashMap::new();
        for cfg in &configs.kb_configs {
            config_map.insert(cfg.name.clone(), cfg.clone());
        }

        info!(base_dir = %base_dir.display(), kb_count = configs.kb_configs.len(), "知识库管理器已加载");

        Ok(Self {
            instances: RwLock::new(HashMap::new()),
            configs: RwLock::new(config_map),
            base_dir: base_dir.to_path_buf(),
            config_path,
        })
    }

    /// 创建新知识库。
    pub async fn create_kb(&self, config: KbConfig) -> Result<Arc<KnowledgeBase>, KnowledgeError> {
        let name = config.name.clone();

        // 检查是否已存在
        {
            let instances = self.instances.read().await;
            if instances.contains_key(&name) {
                return Err(KnowledgeError::InvalidInput(format!(
                    "知识库 '{name}' 已存在"
                )));
            }
        }

        // 构建知识库实例
        let kb = KnowledgeBase::build(&self.base_dir, &config).await?;

        // 保存配置
        {
            let mut configs = self.configs.write().await;
            configs.insert(name.clone(), config.clone());
            self.persist_configs(&configs).await?;
        }

        let kb = Arc::new(kb);
        {
            let mut instances = self.instances.write().await;
            instances.insert(name.clone(), kb.clone());
        }

        info!(%name, "知识库已创建");
        Ok(kb)
    }

    /// 打开已有知识库。
    pub async fn open_kb(&self, name: &str) -> Result<Arc<KnowledgeBase>, KnowledgeError> {
        // 检查是否已加载
        {
            let instances = self.instances.read().await;
            if let Some(kb) = instances.get(name) {
                return Ok(kb.clone());
            }
        }

        // 从配置创建
        let config = {
            let configs = self.configs.read().await;
            configs
                .get(name)
                .cloned()
                .ok_or_else(|| KnowledgeError::NotFound(format!("知识库 '{name}' 未找到")))?
        };

        let kb = KnowledgeBase::build(&self.base_dir, &config).await?;
        let kb = Arc::new(kb);
        {
            let mut instances = self.instances.write().await;
            instances.insert(name.to_string(), kb.clone());
        }

        info!(%name, "知识库已打开");
        Ok(kb)
    }

    /// 删除知识库及其所有数据。
    pub async fn delete_kb(&self, name: &str) -> Result<(), KnowledgeError> {
        {
            let mut instances = self.instances.write().await;
            instances.remove(name);
        }
        {
            let mut configs = self.configs.write().await;
            configs.remove(name);
            self.persist_configs(&configs).await?;
        }

        // 删除数据目录
        let kb_dir = self.base_dir.join(crate::sanitize_kb_name(name));
        if kb_dir.exists() {
            tokio::fs::remove_dir_all(&kb_dir)
                .await
                .map_err(|e| KnowledgeError::Internal(format!("删除知识库目录失败: {e}")))?;
        }

        info!(%name, "知识库已删除");
        Ok(())
    }

    /// 列出所有已配置的知识库。
    pub async fn list_kbs(&self) -> Result<Vec<KbInfo>, KnowledgeError> {
        let configs = self.configs.read().await;
        let instances = self.instances.read().await;

        let mut infos = Vec::new();
        for cfg in configs.values() {
            let (doc_count, chunk_count) = if let Some(kb) = instances.get(&cfg.name) {
                match kb.stats().await {
                    Ok(s) => (s.document_count, s.chunk_count),
                    Err(_) => (0, 0),
                }
            } else {
                (0, 0)
            };

            infos.push(KbInfo {
                name: cfg.name.clone(),
                description: cfg.description.clone(),
                backend: backend_type_str(&cfg.backend),
                embedding_model: format!("{:?}", cfg.embedding_model),
                document_count: doc_count,
                chunk_count,
            });
        }
        Ok(infos)
    }

    /// 获取指定知识库。
    pub async fn get_kb(&self, name: &str) -> Option<Arc<KnowledgeBase>> {
        self.open_kb(name).await.ok()
    }

    /// 跨知识库搜索（并发查询所有 KB，合并结果）。
    pub async fn search_all(&self, query: &str, top_k: usize) -> Vec<(String, Vec<SearchResult>)> {
        let configs = self.configs.read().await;
        let names: Vec<String> = configs.keys().cloned().collect();
        drop(configs);

        let mut results = Vec::new();
        for name in &names {
            if let Ok(kb) = self.open_kb(name).await
                && let Ok(res) = kb.search(query, top_k).await
                && !res.is_empty()
            {
                results.push((name.clone(), res));
            }
        }
        results
    }

    /// 持久化配置到 JSON 文件。
    async fn persist_configs(
        &self,
        configs: &HashMap<String, KbConfig>,
    ) -> Result<(), KnowledgeError> {
        let cf = KbConfigsFile {
            kb_configs: configs.values().cloned().collect(),
        };
        let json = serde_json::to_string_pretty(&cf)
            .map_err(|e| KnowledgeError::Internal(format!("序列化配置失败: {e}")))?;
        tokio::fs::write(&self.config_path, json)
            .await
            .map_err(|e| KnowledgeError::Internal(format!("写入配置失败: {e}")))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// KnowledgeBase
// ---------------------------------------------------------------------------

/// 单个知识库 — 封装完整的读写能力。
pub struct KnowledgeBase {
    config: KbConfig,
    pipeline: IngestionPipeline,
    search_engine: HybridSearchEngine,
    graph_store: Option<Arc<dyn GraphStore>>,
    fulltext_index: Option<Arc<dyn FullTextIndex>>,
}

impl KnowledgeBase {
    /// 根据配置构建知识库实例。
    async fn build(base_dir: &Path, config: &KbConfig) -> Result<Self, KnowledgeError> {
        let kb_dir = base_dir.join(crate::sanitize_kb_name(&config.name));
        tokio::fs::create_dir_all(&kb_dir)
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("无法创建目录: {e}")))?;

        // 构建嵌入引擎
        let model_type: FastembedModelType = config.embedding_model.clone().into();
        let embedding = Arc::new(
            FastembedEngine::new(model_type)
                .map_err(|e| KnowledgeError::EmbeddingError(e.to_string()))?,
        );
        let ndims = embedding.ndims();

        // 构建分块器
        let chunker = make_chunker(config.chunking.clone().into());

        // 构建存储后端
        #[cfg(feature = "lancedb")]
        let (doc_store, vector_index, fulltext_index, graph_store): BackendComponents = match &config.backend {
            BackendType::InMemory => {
                let be = Arc::new(crate::backends::memory::InMemoryBackend::new());
                (
                    be.clone() as Arc<dyn DocumentStore>,
                    Some(be.clone() as Arc<dyn VectorIndex>),
                    Some(be.clone() as Arc<dyn FullTextIndex>),
                    Some(be.clone() as Arc<dyn GraphStore>),
                )
            }
            BackendType::LanceDb => {
                let be = Arc::new(
                    crate::backends::lancedb::LanceDbBackend::connect(
                        &kb_dir,
                        &crate::sanitize_kb_name(&config.name),
                        ndims,
                    )
                    .await?,
                );
                let graph = Arc::new(crate::backends::memory_graph::MemoryGraphStore::new());
                (
                    be.clone() as Arc<dyn DocumentStore>,
                    Some(be.clone() as Arc<dyn VectorIndex>),
                    Some(be.clone() as Arc<dyn FullTextIndex>),
                    Some(graph as Arc<dyn GraphStore>),
                )
            }
            #[cfg(feature = "helixdb")]
            BackendType::HelixDb => {
                return Err(KnowledgeError::InvalidInput(
                    "HelixDB 后端需通过高级 API 配置，请使用 HelixDbBackend::connect()".into(),
                ));
            }
        };

        #[cfg(not(feature = "lancedb"))]
        let (doc_store, vector_index, fulltext_index, graph_store): BackendComponents = match &config.backend {
            BackendType::InMemory => {
                let be = Arc::new(crate::backends::memory::InMemoryBackend::new());
                (
                    be.clone() as Arc<dyn DocumentStore>,
                    Some(be.clone() as Arc<dyn VectorIndex>),
                    Some(be.clone() as Arc<dyn FullTextIndex>),
                    Some(be.clone() as Arc<dyn GraphStore>),
                )
            }
            #[cfg(feature = "helixdb")]
            BackendType::HelixDb => {
                return Err(KnowledgeError::InvalidInput(
                    "HelixDB 后端需通过高级 API 配置".into(),
                ));
            }
            _ => {
                return Err(KnowledgeError::InvalidInput(
                    "LanceDB feature 未启用".into(),
                ));
            }
        };

        // 构建 IngestionPipeline
        let pipeline = IngestionPipeline::new(
            doc_store.clone(),
            vector_index.clone(),
            graph_store.clone(),
            fulltext_index.clone(),
            embedding.clone(),
            chunker,
        );

        // 构建 SearchEngine
        let search_engine = HybridSearchEngine::new(
            doc_store,
            vector_index,
            graph_store.clone(),
            fulltext_index.clone(),
            embedding,
        );

        Ok(Self {
            config: config.clone(),
            pipeline,
            search_engine,
            graph_store,
            fulltext_index,
        })
    }

    /// 从文件添加文档（使用配置的默认存储模式）。
    pub async fn add_file(&self, path: &Path) -> Result<Document, KnowledgeError> {
        self.add_file_with_mode(path, self.config.default_storage_mode)
            .await
    }

    /// 从文件添加文档，可指定存储模式。
    pub async fn add_file_with_mode(
        &self,
        path: &Path,
        mode: StorageMode,
    ) -> Result<Document, KnowledgeError> {
        let parser = make_parser(path)?;
        let parsed = parser.parse_file(path).await?;
        let hash = sha2::Sha256::digest(parsed.content.as_bytes());
        let doc_id = hex::encode(&hash[..8]);

        let doc = Document {
            id: doc_id,
            kb_id: Some(self.config.name.clone()),
            title: parsed.title,
            source_path: parsed.source_path,
            content: parsed.content,
            metadata: parsed.metadata,
        };

        self.pipeline.ingest_with_mode(doc.clone(), mode).await?;
        info!(kb = %self.config.name, doc_id = %doc.id, title = %doc.title, mode = ?mode, "文档已添加");
        Ok(doc)
    }

    /// 批量导入目录中所有支持的文档。
    pub async fn add_directory(&self, dir: &Path) -> Result<Vec<Document>, KnowledgeError> {
        let mut docs = Vec::new();
        let mut entries = tokio::fs::read_dir(dir)
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("读取目录失败: {e}")))?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("遍历目录失败: {e}")))?
        {
            let path = entry.path();
            if path.is_file() {
                match self.add_file(&path).await {
                    Ok(doc) => docs.push(doc),
                    Err(e) => tracing::warn!(path = %path.display(), error = %e, "跳过文件"),
                }
            }
        }
        Ok(docs)
    }

    /// 直接添加文本（使用配置的默认存储模式）。
    pub async fn add_text(
        &self,
        title: &str,
        content: &str,
        source: &str,
    ) -> Result<Document, KnowledgeError> {
        self.add_text_with_mode(title, content, source, self.config.default_storage_mode)
            .await
    }

    /// 直接添加文本，可指定存储模式。
    pub async fn add_text_with_mode(
        &self,
        title: &str,
        content: &str,
        source: &str,
        mode: StorageMode,
    ) -> Result<Document, KnowledgeError> {
        let hash = sha2::Sha256::digest(content.as_bytes());
        let doc_id = hex::encode(&hash[..8]);

        let doc = Document {
            id: doc_id,
            kb_id: Some(self.config.name.clone()),
            title: title.to_string(),
            source_path: source.to_string(),
            content: content.to_string(),
            metadata: DocumentMetadata {
                file_type: Some("txt".into()),
                ..Default::default()
            },
        };

        self.pipeline.ingest_with_mode(doc.clone(), mode).await?;
        Ok(doc)
    }

    /// 搜索知识库。
    pub async fn search(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<SearchResult>, KnowledgeError> {
        self.search_engine
            .search(&SearchRequest {
                query: query.to_string(),
                top_k,
                strategy: SearchStrategy::Auto,
                filters: Some(SearchFilters {
                    kb_id: Some(self.config.name.clone()),
                    ..Default::default()
                }),
                min_confidence: None,
            })
            .await
    }

    /// 删除文档。
    pub async fn remove_document(&self, doc_id: &str) -> Result<(), KnowledgeError> {
        self.pipeline.delete_document(&doc_id.to_string()).await
    }

    /// 获取统计信息。
    pub async fn stats(&self) -> Result<StoreStats, KnowledgeError> {
        self.pipeline.stats().await
    }

    /// 列出文档摘要（分页）。
    pub async fn list_documents(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<DocumentSummary>, KnowledgeError> {
        self.pipeline.list_documents(offset, limit).await
    }

    /// 获取知识库配置。
    pub fn config(&self) -> &KbConfig {
        &self.config
    }

    // ==================== 模式 B：事实摄入 ====================

    /// 批量添加结构化事实，直接写入图谱。
    ///
    /// 每条 Fact 映射为 Entity→Entity 边（predicate 作为 Custom 边类型）。
    /// 若 `index_text` 为 true，同时将事实文本写入全文索引。
    /// 使用 Fact ID 在单次调用内去重；重复调用相同事实不会创建重复边。
    pub async fn add_facts(
        &self,
        facts: &[Fact],
        index_text: bool,
    ) -> Result<Vec<Fact>, KnowledgeError> {
        let gs = self.graph_store.as_ref().ok_or_else(|| {
            KnowledgeError::InvalidInput("当前后端不支持图存储".into())
        })?;

        let mut edges = Vec::with_capacity(facts.len());
        let mut seen_fact_ids: std::collections::HashSet<&str> =
            std::collections::HashSet::with_capacity(facts.len());

        for fact in facts {
            // 单次调用内去重：相同 Fact ID 只添加一次
            if !seen_fact_ids.insert(&fact.id) {
                continue;
            }

            let subject_id = compute_entity_id(&fact.subject, "Entity");
            let object_id = compute_entity_id(&fact.object, "Entity");

            // 确保实体节点存在
            if !gs.node_exists(&subject_id).await.unwrap_or(false) {
                gs.upsert_node(GraphNode {
                    id: subject_id.clone(),
                    labels: vec!["Entity".into()],
                    properties: {
                        let mut props = HashMap::new();
                        props.insert("name".into(), fact.subject.clone());
                        props
                    },
                    distance: 0,
                })
                .await?;
            }
            if !gs.node_exists(&object_id).await.unwrap_or(false) {
                gs.upsert_node(GraphNode {
                    id: object_id.clone(),
                    labels: vec!["Entity".into()],
                    properties: {
                        let mut props = HashMap::new();
                        props.insert("name".into(), fact.object.clone());
                        props
                    },
                    distance: 0,
                })
                .await?;
            }

            edges.push(KnowledgeEdge {
                source_id: subject_id,
                target_id: object_id,
                edge_type: EdgeType::Custom(fact.predicate.clone()),
                weight: fact.confidence,
                properties: {
                    let mut props = fact.metadata.clone();
                    props.insert("fact_id".into(), fact.id.clone());
                    props.insert("source".into(), fact.source.clone());
                    props
                },
            });
        }

        gs.add_edges(&edges).await?;

        // 收集去重后的事实
        let stored: Vec<Fact> = facts
            .iter()
            .filter(|f| seen_fact_ids.contains(f.id.as_str()))
            .cloned()
            .collect();

        // 可选：全文索引事实文本
        if index_text
            && let Some(ref ft) = self.fulltext_index
        {
            let entries: Vec<FullTextEntry> = stored
                .iter()
                .map(|f| FullTextEntry {
                    id: f.id.clone(),
                    document_id: f.id.clone(),
                    text: format!("{} {} {}", f.subject, f.predicate, f.object),
                })
                .collect();
            if !entries.is_empty() {
                ft.index(&entries)
                    .await
                    .map_err(|e| KnowledgeError::TextSearchError(e.to_string()))?;
            }
        }

        Ok(stored)
    }

    // ==================== 模式 C：实体/关系摄入 ====================

    /// 批量添加实体节点到图谱。
    pub async fn add_entities(&self, entities: &[Entity]) -> Result<(), KnowledgeError> {
        let gs = self.graph_store.as_ref().ok_or_else(|| {
            KnowledgeError::InvalidInput("当前后端不支持图存储".into())
        })?;

        for entity in entities {
            let node = GraphNode {
                id: entity.id.clone(),
                labels: vec![entity.entity_type.clone()],
                properties: {
                    let mut props = entity.properties.clone();
                    props.insert("name".into(), entity.name.clone());
                    // 固定格式避免 locale 差异（某些地区用逗号作小数点）
                    props.insert(
                        "confidence".into(),
                        format!("{:.6}", entity.confidence),
                    );
                    props.insert("source_chunk_id".into(), entity.source_chunk_id.clone());
                    props
                },
                distance: 0,
            };
            gs.upsert_node(node).await?;
        }
        Ok(())
    }

    /// 批量添加自定义关系边到图谱。
    pub async fn add_relation_edges(&self, edges: &[KnowledgeEdge]) -> Result<(), KnowledgeError> {
        let gs = self.graph_store.as_ref().ok_or_else(|| {
            KnowledgeError::InvalidInput("当前后端不支持图存储".into())
        })?;

        gs.add_edges(edges).await?;
        Ok(())
    }

    // ==================== 图谱查询 ====================

    /// 查询与指定实体相关的所有事实（遍历相邻边）。
    ///
    /// `entity_type` 默认为 `"Entity"`，与 [`add_facts`] 保持一致。
    pub async fn query_entity_facts(
        &self,
        entity_name: &str,
        max_depth: u32,
    ) -> Result<Vec<TraversalStep>, KnowledgeError> {
        self.query_entity_facts_with_type(entity_name, "Entity", max_depth)
            .await
    }

    /// 查询与指定实体相关的所有事实，可指定实体类型。
    pub async fn query_entity_facts_with_type(
        &self,
        entity_name: &str,
        entity_type: &str,
        max_depth: u32,
    ) -> Result<Vec<TraversalStep>, KnowledgeError> {
        let gs = self.graph_store.as_ref().ok_or_else(|| {
            KnowledgeError::InvalidInput("当前后端不支持图存储".into())
        })?;

        let entity_id = compute_entity_id(entity_name, entity_type);
        gs.traverse(&entity_id, &[], TraversalDirection::Both, max_depth)
            .await
    }

    /// 查询两个实体间的关系路径。
    ///
    /// `entity_type` 默认为 `"Entity"`，与 [`add_facts`] 保持一致。
    pub async fn query_relation_path(
        &self,
        from_entity: &str,
        to_entity: &str,
    ) -> Result<Option<Vec<TraversalStep>>, KnowledgeError> {
        self.query_relation_path_with_type(from_entity, to_entity, "Entity")
            .await
    }

    /// 查询两个实体间的关系路径，可指定实体类型。
    pub async fn query_relation_path_with_type(
        &self,
        from_entity: &str,
        to_entity: &str,
        entity_type: &str,
    ) -> Result<Option<Vec<TraversalStep>>, KnowledgeError> {
        let gs = self.graph_store.as_ref().ok_or_else(|| {
            KnowledgeError::InvalidInput("当前后端不支持图存储".into())
        })?;

        let from_id = compute_entity_id(from_entity, entity_type);
        let to_id = compute_entity_id(to_entity, entity_type);
        gs.shortest_path(&from_id, &to_id, &[], 10).await
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

fn backend_type_str(bt: &BackendType) -> String {
    match bt {
        BackendType::InMemory => "InMemory".into(),
        #[cfg(feature = "lancedb")]
        BackendType::LanceDb => "LanceDB".into(),
        #[cfg(feature = "helixdb")]
        BackendType::HelixDb => "HelixDB".into(),
    }
}

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
    async fn test_add_text_with_mode_graph_only() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = KnowledgeBaseManager::load(tmp.path()).await.unwrap();
        let kb = mgr.create_kb(make_kb_config("graph-test")).await.unwrap();

        // GraphOnly 模式：不嵌入，仅建图谱结构边
        let doc = kb
            .add_text_with_mode(
                "GraphDoc",
                "张三在技术部工作，李四是他的下属。",
                "test",
                StorageMode::GraphOnly,
            )
            .await
            .unwrap();

        assert_eq!(doc.title, "GraphDoc");

        // 验证图谱边存在（CONTAINS: doc → chunk）
        let gs = kb.graph_store.as_ref().expect("graph store should exist");
        let steps = gs
            .traverse(&doc.id, &[EdgeType::Contains], TraversalDirection::Outgoing, 1)
            .await
            .unwrap();

        // 应找到合成 chunk 节点
        assert!(!steps.is_empty());
        assert!(steps.iter().any(|s| s.node.id.ends_with("__full")));
    }

    #[tokio::test]
    async fn test_add_facts_and_query() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = KnowledgeBaseManager::load(tmp.path()).await.unwrap();
        let kb = mgr
            .create_kb(make_kb_config("facts-test"))
            .await
            .unwrap();

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
        let kb = mgr
            .create_kb(make_kb_config("entity-test"))
            .await
            .unwrap();

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

    #[tokio::test]
    async fn test_add_text_backward_compat() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = KnowledgeBaseManager::load(tmp.path()).await.unwrap();
        let kb = mgr
            .create_kb(make_kb_config("compat-test"))
            .await
            .unwrap();

        // 旧 API 仍然可用（使用 default_storage_mode = Full）
        let doc = kb
            .add_text("Compat", "Backward compatible test.", "test")
            .await
            .unwrap();
        assert_eq!(doc.title, "Compat");

        // 搜索也能正常工作
        let results = kb.search("compatible", 5).await.unwrap();
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn test_storage_mode_vector_only_no_graph() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = KnowledgeBaseManager::load(tmp.path()).await.unwrap();
        let kb = mgr
            .create_kb(make_kb_config("vector-test"))
            .await
            .unwrap();

        let doc = kb
            .add_text_with_mode(
                "VectorOnly",
                "Pure vector search content.",
                "test",
                StorageMode::VectorOnly,
            )
            .await
            .unwrap();

        // 文档可以搜索到
        let results = kb.search("vector search", 5).await.unwrap();
        assert!(results.iter().any(|r| r.document_id == doc.id));
    }
}
