//! 单个知识库 — 封装完整的文档摄入、搜索和图谱查询能力。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use sha2::Digest;
use tracing::info;

use crate::chunking::make_chunker;
use crate::embedding::{FastembedEngine, FastembedModelType};
use crate::engine::{HybridSearchEngine, IngestionPipeline};
use crate::error::KnowledgeError;
use crate::parsers::make_parser;
use crate::traits::*;
use crate::types::*;

use super::config::{BackendType, KbConfig};

/// 存储后端组件：(文档存储, 向量索引, 全文索引, 图存储)
type BackendComponents = (
    Arc<dyn DocumentStore>,
    Option<Arc<dyn VectorIndex>>,
    Option<Arc<dyn FullTextIndex>>,
    Option<Arc<dyn GraphStore>>,
);

// ---------------------------------------------------------------------------
// KnowledgeBase
// ---------------------------------------------------------------------------

/// 单个知识库 — 封装完整的读写能力。
pub struct KnowledgeBase {
    pub(super) config: KbConfig,
    pub(super) pipeline: IngestionPipeline,
    search_engine: HybridSearchEngine,
    pub(super) graph_store: Option<Arc<dyn GraphStore>>,
    pub(super) fulltext_index: Option<Arc<dyn FullTextIndex>>,
}

impl KnowledgeBase {
    /// 根据配置构建知识库实例。
    pub(super) async fn build(base_dir: &Path, config: &KbConfig) -> Result<Self, KnowledgeError> {
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
