//! HelixDB 后端实现。
//!
//! 将 HelixDB 的图-向量数据库作为 `knowledge-base` 的存储后端。
//! 实现所有核心 trait：`DocumentStore`、`VectorIndex`、`FullTextIndex`、
//! `GraphStore`，以及可选的 `CombinedSearch`。
//!
//! # 架构
//!
//! ```text
//! HelixDbBackend
//!   ├── HelixDbClient (HTTP → POST /v1/query)
//!   ├── HelixSchema  (节点/边标签、索引配置)
//!   └── 5 个 trait 实现
//! ```
//!
//! # 使用示例
//!
//! ```ignore
//! use knowledge_base::backends::helixdb::HelixDbBackend;
//!
//! let backend = HelixDbBackend::connect("http://localhost:6969", 1024).await?;
//! backend.init_schema().await?;
//! ```

mod client;
mod queries;
mod schema;
mod types;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tracing::{debug, info};

use crate::engine::fusion;
use crate::error::KnowledgeError;
use crate::traits::*;
use crate::types::*;

use client::HelixDbClient;
use queries::prop_f64;
pub use types::{HelixIndexSpec, HelixSchema, IndexType};

// ── 分数转换 ──────────────────────────────────────────────────────────────

/// 将 HelixDB 的距离值转换为相似度分数。
///
/// 公式：`score = 1.0 / (1.0 + distance)`
///
/// * 余弦距离 ∈ [0, 2] → 分数 ∈ [1.0, 0.333]
/// * BM25 反相关分数 → 同理
/// * 图跳数 → 同理（越近分数越高）
fn distance_to_score(distance: f64) -> f32 {
    (1.0 / (1.0 + distance)) as f32
}

/// 将 HelixDB 响应中的 `$distance` JSON 值转换为 f64。
fn parse_distance(val: &serde_json::Value) -> f64 {
    val.as_f64().unwrap_or(0.0)
}

/// 将 HelixDB 返回的 ID 值（可能是数字 `$id` 或字符串属性）转换为 String。
///
/// HelixDB 内置 `$id` 是自增整数，以 JSON number 返回；
/// 用户自定义属性（如 `document_id`）以 JSON string 返回。
fn parse_id_value(val: &serde_json::Value) -> Option<String> {
    if let Some(s) = val.as_str() {
        Some(s.to_string())
    } else if let Some(n) = val.as_u64() {
        Some(n.to_string())
    } else if let Some(n) = val.as_i64() {
        Some(n.to_string())
    } else {
        // 自增 ID 不会是浮点数，但做保守处理
        val.as_f64().map(|n| format!("{n}"))
    }
}

/// 从 HelixDB 读查询响应中提取 `properties` 数组。
///
/// HelixDB 返回 `{"name": {"properties": [...]}}` 而非 `{"name": [...]}`。
/// 此辅助函数解包 properties 包装器，返回内部数组的引用。
/// 从 HelixDB 读查询响应中提取结果数组。
///
/// HelixDB 将投影结果包装为 `{"name": {"properties": [...]}}` 格式。
/// Count 步骤返回 `{"name": {"count": N}}`，需用 [`count_value`] 解析。
fn extract_properties<'r>(response: &'r Value, key: &str) -> Option<&'r Vec<Value>> {
    response
        .get(key)
        .and_then(|v| v.get("properties"))
        .and_then(|v| v.as_array())
}

// ── 后端结构体 ────────────────────────────────────────────────────────────

/// HelixDB 后端 — 将 HelixDB 的图-向量数据库作为知识存储后端。
///
/// 通过 HTTP 与 HelixDB 通信（POST /v1/query），使用原始 JSON 构建查询。
/// Schema 通过 `HelixSchema` 配置，支持文档 RAG、代码知识库、概念图谱
/// 等多种 AI Agent 场景。
pub struct HelixDbBackend {
    client: Arc<HelixDbClient>,
    ndims: usize,
    schema: HelixSchema,
}

impl HelixDbBackend {
    /// 连接到 HelixDB 并使用默认 Document-Chunk RAG schema。
    ///
    /// # 参数
    /// * `base_url` — HelixDB 服务器 URL（例如 `http://localhost:6969`）。
    /// * `ndims` — 向量维度，应与 `EmbeddingEngine::ndims()` 对齐。
    pub async fn connect(base_url: &str, ndims: usize) -> Result<Self, KnowledgeError> {
        Self::connect_with_schema(base_url, ndims, HelixSchema::default()).await
    }

    /// 连接到 HelixDB 并使用自定义 schema。
    pub async fn connect_with_schema(
        base_url: &str,
        ndims: usize,
        schema: HelixSchema,
    ) -> Result<Self, KnowledgeError> {
        let client = Arc::new(HelixDbClient::connect(base_url).await?);
        Ok(Self {
            client,
            ndims,
            schema,
        })
    }

    /// 幂等初始化 schema（根据 `HelixSchema` 创建索引）。
    ///
    /// 应在 `connect` 之后调用一次。重复调用是安全的。
    pub async fn init_schema(&self) -> Result<(), KnowledgeError> {
        schema::init_schema(&self.client, &self.schema).await
    }

    /// 返回当前使用的 schema 配置（只读）。
    pub fn schema(&self) -> &HelixSchema {
        &self.schema
    }
}

// ── 辅助：维度守卫 + EdgeType → HelixDB 边标签映射 ───────────────────────

impl HelixDbBackend {
    /// 向量维度守卫 —— 在任何 HTTP 请求之前 fail-closed。
    ///
    /// HelixDB 的向量索引维度由 schema 声明且建库后固定。喂进长度不符的向量
    /// 不会报错，只会把索引写坏，或让 ANN 搜索返回看似正常实则错配的结果。
    /// 因此任何离开本进程的向量都要先核对维度。
    ///
    /// `label` 是承载该向量的节点标签（HelixDB 的向量集合即「节点标签 +
    /// 属性」，对应关系库的表名）。
    fn ensure_dims(&self, label: &str, vector: &[f32]) -> Result<(), KnowledgeError> {
        if vector.len() == self.ndims {
            return Ok(());
        }
        Err(KnowledgeError::DimensionMismatch {
            table_name: label.to_string(),
            expected: self.ndims,
            actual: vector.len(),
        })
    }

    /// 写路径守卫：空向量表示「本次未提供向量」——非 Full 存储模式
    /// （`StorageMode::TextOnly` / `MetadataOnly` 等）下 `IngestionPipeline`
    /// 会显式传入空 embedding，这类调用放行，只有维度不符的实向量才拒绝。
    fn ensure_write_dims(&self, label: &str, vector: &[f32]) -> Result<(), KnowledgeError> {
        if vector.is_empty() {
            return Ok(());
        }
        self.ensure_dims(label, vector)
    }

    /// 将 knowledge-base 的 `EdgeType` 映射到 HelixDB 的边标签字符串。
    fn edge_label(&self, et: &EdgeType) -> String {
        match et {
            EdgeType::Contains => self.schema.contains_edge.clone(),
            EdgeType::RelatedTo => self.schema.related_edge.clone(),
            EdgeType::Mentions => "MENTIONS".into(),
            EdgeType::BelongsTo => self.schema.belongs_to_edge.clone(),
            EdgeType::NextChunk => self.schema.next_fragment_edge.clone(),
            EdgeType::Custom(s) => s.clone(),
        }
    }

    /// 将 TraversalDirection 映射为 HelixDB 方向字符串。
    fn direction_str(dir: TraversalDirection) -> &'static str {
        match dir {
            TraversalDirection::Outgoing => "Out",
            TraversalDirection::Incoming => "In",
            TraversalDirection::Both => "Both",
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// DocumentStore
// ═══════════════════════════════════════════════════════════════════════════

#[async_trait]
impl DocumentStore for HelixDbBackend {
    async fn store(&self, doc: Document, chunks: Vec<Chunk>) -> Result<(), KnowledgeError> {
        info!(
            doc_id = %doc.id,
            title = %doc.title,
            chunk_count = chunks.len(),
            "Storing document to HelixDB"
        );

        // 维度守卫：先于任何 HTTP 请求校验全部 embedding，避免写到一半
        // 才发现维度不符，留下半截数据。
        for chunk in &chunks {
            self.ensure_write_dims(&self.schema.fragment_node_label, &chunk.embedding)?;
        }

        let metadata_json = serde_json::to_string(&doc.metadata).unwrap_or_default();
        // 文档级 embedding：取所有分块 embedding 的平均值
        let doc_embedding = if !chunks.is_empty() {
            let ndims = chunks[0].embedding.len();
            let mut avg = vec![0.0f32; ndims];
            for chunk in &chunks {
                for (i, &v) in chunk.embedding.iter().enumerate() {
                    avg[i] += v;
                }
            }
            for v in &mut avg {
                *v /= chunks.len() as f32;
            }
            avg
        } else {
            Vec::new()
        };

        self.ensure_write_dims(&self.schema.content_node_label, &doc_embedding)?;

        // 1. 创建 Document 节点
        let doc_query =
            queries::create_document_node(&self.schema, &doc, &metadata_json, &doc_embedding);
        self.client.execute_write(doc_query).await?;

        // 2. 创建 Chunk 节点 + CONTAINS + NEXT_CHUNK 边
        for chunk in &chunks {
            let chunk_query = queries::create_chunk_node(&self.schema, chunk);
            self.client.execute_write(chunk_query).await?;

            let edge_query = queries::create_contains_edge(&self.schema, &doc.id, &chunk.id);
            self.client.execute_write(edge_query).await?;
        }

        // 3. NEXT_CHUNK 边
        for window in chunks.windows(2) {
            if !self.schema.next_fragment_edge.is_empty() {
                let edge_query =
                    queries::create_next_chunk_edge(&self.schema, &window[0].id, &window[1].id);
                let _ = self.client.execute_write(edge_query).await;
            }
        }

        info!(doc_id = %doc.id, "Document stored");
        Ok(())
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Document>, KnowledgeError> {
        debug!(%id, "Getting document");
        let query = queries::get_document_by_id(&self.schema, id);
        let response = self.client.execute_read(query).await?;

        let doc = extract_properties(&response, "doc")
            .and_then(|arr| arr.first())
            .map(|item| {
                let doc_id = item
                    .get("id")
                    .and_then(parse_id_value)
                    .unwrap_or_else(|| id.to_string());
                let title = item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let source_path = item
                    .get("source_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = item
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let metadata: DocumentMetadata = item
                    .get("metadata")
                    .and_then(|v| v.as_str())
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();

                Document {
                    kb_id: None,
                    id: doc_id,
                    title,
                    source_path,
                    content,
                    metadata,
                }
            });

        Ok(doc)
    }

    async fn delete(&self, id: &DocumentId) -> Result<(), KnowledgeError> {
        info!(%id, "Deleting document");
        let query = queries::delete_document_cascade(&self.schema, id);
        self.client.execute_write(query).await?;
        info!(%id, "Document deleted");
        Ok(())
    }

    async fn list(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<DocumentSummary>, KnowledgeError> {
        debug!(offset, limit, "Listing documents");
        let query = queries::list_documents(&self.schema, offset, limit);
        let response = self.client.execute_read(query).await?;

        let summaries = extract_properties(&response, "docs")
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let id = parse_id_value(item.get("id")?)?;
                        let title = item
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let source_path = item
                            .get("source_path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let file_type = item
                            .get("metadata")
                            .and_then(|v| v.as_str())
                            .and_then(|s| serde_json::from_str::<DocumentMetadata>(s).ok())
                            .and_then(|m| m.file_type);
                        // chunk_count 需要通过图遍历获取（CONTAINS 出边数）
                        // 此处设为 0，调用方可自行查询
                        Some(DocumentSummary {
                            id,
                            title,
                            source_path,
                            chunk_count: 0,
                            file_type,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(summaries)
    }

    async fn chunks(&self, doc_id: &DocumentId) -> Result<Vec<Chunk>, KnowledgeError> {
        debug!(%doc_id, "Getting document chunks");
        let query = queries::get_document_chunks(&self.schema, doc_id);
        let response = self.client.execute_read(query).await?;

        let chunks = extract_properties(&response, "chunks")
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let id = parse_id_value(item.get("chunk_id")?)?;
                        let text = item
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let document_id = item
                            .get("document_id")
                            .and_then(parse_id_value)
                            .unwrap_or_else(|| doc_id.to_string());
                        let sequence_index = item
                            .get("sequence_index")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32;
                        let page_number = item
                            .get("page_number")
                            .and_then(|v| v.as_u64())
                            .map(|n| n as u32);

                        Some(Chunk {
                            id,
                            document_id,
                            text,
                            sequence_index,
                            page_number,
                            embedding: Vec::new(), // 不通过此接口返回 embedding
                            metadata: ChunkMetadata::default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(chunks)
    }

    async fn stats(&self) -> Result<StoreStats, KnowledgeError> {
        debug!("Getting storage stats");

        let doc_count = self
            .count_nodes(&self.schema.content_node_label)
            .await
            .unwrap_or(0);
        let chunk_count = self
            .count_nodes(&self.schema.fragment_node_label)
            .await
            .unwrap_or(0);

        Ok(StoreStats {
            document_count: doc_count,
            chunk_count,
            total_bytes: 0, // HelixDB 不直接暴露字节统计
        })
    }
}

impl HelixDbBackend {
    async fn count_nodes(&self, label: &str) -> Result<usize, KnowledgeError> {
        let query = queries::count_nodes(&self.schema, label);
        let response = self.client.execute_read(query).await?;
        // Count 步骤返回 {"name": {"count": N}}，不是 {"name": {"properties": [N]}}
        let count = response
            .get("count")
            .and_then(|v| v.get("count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        Ok(count)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// VectorIndex
// ═══════════════════════════════════════════════════════════════════════════

#[async_trait]
impl VectorIndex for HelixDbBackend {
    fn ndims(&self) -> usize {
        self.ndims
    }

    async fn search(
        &self,
        query_vec: &[f32],
        top_k: usize,
        filters: Option<&SearchFilters>,
    ) -> Result<Vec<VectorHit>, KnowledgeError> {
        // 查询向量必须存在且维度精确匹配 —— 空向量在这里不是「未提供」，
        // 而是调用方的 bug。
        self.ensure_dims(&self.schema.fragment_node_label, query_vec)?;

        let query = queries::vector_search_chunks(&self.schema, query_vec, top_k as u32, filters);
        let response = self.client.execute_read(query).await?;

        let hits = extract_properties(&response, "results")
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let chunk_id = parse_id_value(item.get("chunk_id")?)?;
                        let document_id = item
                            .get("document_id")
                            .and_then(parse_id_value)
                            .unwrap_or_default();
                        let distance = item.get("score").map_or(0.0, parse_distance);
                        let score = distance_to_score(distance);
                        Some(VectorHit {
                            chunk_id,
                            document_id,
                            score,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(hits)
    }

    async fn upsert(&self, entries: &[VectorEntry]) -> Result<(), KnowledgeError> {
        for entry in entries {
            self.ensure_write_dims(&self.schema.fragment_node_label, &entry.vector)?;
        }
        for entry in entries {
            let query = queries::update_chunk_embedding(&self.schema, &entry.id, &entry.vector);
            self.client.execute_write(query).await?;
        }
        Ok(())
    }

    async fn remove(&self, ids: &[String]) -> Result<(), KnowledgeError> {
        let mut failures = Vec::new();
        for id in ids {
            let query = queries::delete_chunk_by_id(&self.schema, id);
            if let Err(e) = self.client.execute_write(query).await {
                failures.push((id.clone(), e.to_string()));
            }
        }
        super::aggregate_remove_failures(failures, KnowledgeError::VectorError)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// FullTextIndex
// ═══════════════════════════════════════════════════════════════════════════

#[async_trait]
impl FullTextIndex for HelixDbBackend {
    async fn search(
        &self,
        query: &str,
        top_k: usize,
        filters: Option<&SearchFilters>,
    ) -> Result<Vec<FullTextHit>, KnowledgeError> {
        let query_json = queries::text_search_chunks(&self.schema, query, top_k as u32, filters);
        let response = self.client.execute_read(query_json).await?;

        let hits = extract_properties(&response, "results")
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let chunk_id = parse_id_value(item.get("chunk_id")?)?;
                        let document_id = item
                            .get("document_id")
                            .and_then(parse_id_value)
                            .unwrap_or_default();
                        let distance = item.get("score").map_or(0.0, parse_distance);
                        let score = distance_to_score(distance);
                        let text_snippet = item
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .chars()
                            .take(200)
                            .collect();
                        Some(FullTextHit {
                            chunk_id,
                            document_id,
                            score,
                            text_snippet,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(hits)
    }

    async fn index(&self, _entries: &[FullTextEntry]) -> Result<(), KnowledgeError> {
        // HelixDB 在 Chunk 节点存储时自动建立文本索引（text 属性上已有全文索引）。
        // 如果需要在存储后单独更新文本，使用 SetProperty。
        Ok(())
    }

    async fn remove(&self, ids: &[String]) -> Result<(), KnowledgeError> {
        // 通过 VectorIndex::remove 或 DocumentStore::delete 级联处理。
        let mut failures = Vec::new();
        for id in ids {
            let query = queries::delete_chunk_by_id(&self.schema, id);
            if let Err(e) = self.client.execute_write(query).await {
                failures.push((id.clone(), e.to_string()));
            }
        }
        super::aggregate_remove_failures(failures, KnowledgeError::TextSearchError)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// GraphStore
// ═══════════════════════════════════════════════════════════════════════════

#[async_trait]
impl GraphStore for HelixDbBackend {
    async fn add_edge(&self, edge: KnowledgeEdge) -> Result<(), KnowledgeError> {
        self.add_edges(&[edge]).await
    }

    async fn add_edges(&self, edges: &[KnowledgeEdge]) -> Result<(), KnowledgeError> {
        for edge in edges {
            let edge_label = self.edge_label(&edge.edge_type);
            let query = queries::create_related_to_edge(
                &self.schema,
                &edge.source_id,
                &edge.target_id,
                edge.weight as f64,
            );
            // 如果边标签不是 RELATED_TO，需要构建对应的查询
            if edge_label != self.schema.related_edge {
                // 使用通用 create_contains_edge 的模式构建自定义边
                let custom_query = serde_json::json!({
                    "request_type": "write",
                    "query": {
                        "queries": [
                            {"Query": {"name": "src", "steps": [
                                {"NWhere": {"Eq": [self.schema.id_property, {"String": edge.source_id}]}}
                            ], "condition": null}},
                            {"Query": {"name": "tgt", "steps": [
                                {"NWhere": {"Eq": [self.schema.id_property, {"String": edge.target_id}]}}
                            ], "condition": null}},
                            {"Query": {"name": "edge", "steps": [
                                {"N": {"Var": "src"}},
                                {"AddE": {
                                    "label": edge_label,
                                    "to": {"Var": "tgt"},
                                    "properties": [["weight", prop_f64(edge.weight as f64)]]
                                }},
                                {"Count": null}
                            ], "condition": null}}
                        ],
                        "returns": ["edge"]
                    }
                });
                self.client.execute_write(custom_query).await?;
            } else {
                self.client.execute_write(query).await?;
            }
        }
        Ok(())
    }

    async fn remove_node_edges(&self, node_id: &str) -> Result<(), KnowledgeError> {
        // HelixDB 删除节点时自动级联删除关联边。
        // 如果只需要删边而不删节点，需要单独处理。
        let _ = node_id;
        Ok(())
    }

    async fn traverse(
        &self,
        start_node: &str,
        edge_types: &[EdgeType],
        direction: TraversalDirection,
        max_depth: u32,
    ) -> Result<Vec<TraversalStep>, KnowledgeError> {
        // 空 edge_types 表示匹配所有已知固定边类型，与 MemoryGraphStore /
        // InMemoryBackend 保持一致。
        let edge_types: Vec<EdgeType> = if edge_types.is_empty() {
            vec![
                EdgeType::RelatedTo,
                EdgeType::Contains,
                EdgeType::BelongsTo,
                EdgeType::NextChunk,
                EdgeType::Mentions,
            ]
        } else {
            edge_types.to_vec()
        };

        let dir_str = Self::direction_str(direction);
        let mut all_steps: Vec<TraversalStep> = Vec::new();

        // 对每个边类型分别遍历
        for et in &edge_types {
            let label = self.edge_label(et);
            let query =
                queries::traverse_graph(&self.schema, start_node, &label, dir_str, max_depth);
            let response = self.client.execute_read(query).await?;

            let steps = extract_properties(&response, "traversed")
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            let node_id = parse_id_value(item.get("node_id")?)?;
                            let labels = item
                                .get("label")
                                .and_then(|v| v.as_str())
                                .map(|s| vec![s.to_string()])
                                .unwrap_or_default();
                            let distance =
                                item.get("distance").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

                            Some(TraversalStep {
                                node: GraphNode {
                                    id: node_id,
                                    labels,
                                    properties: HashMap::new(),
                                    distance,
                                },
                                via_edge: Some(et.clone()),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            all_steps.extend(steps);
        }

        // 去重（按 node_id）
        let mut seen = HashSet::new();
        all_steps.retain(|s| seen.insert(s.node.id.clone()));

        Ok(all_steps)
    }

    async fn shortest_path(
        &self,
        from: &str,
        to: &str,
        _edge_types: &[EdgeType],
        _max_depth: u32,
    ) -> Result<Option<Vec<TraversalStep>>, KnowledgeError> {
        // HelixDB 当前版本不直接支持 shortestPath 查询步骤。
        // 作为简化实现，通过双向 BFS 在应用层实现最短路径搜索。
        // 对于生产使用，当 HelixDB 添加原生 shortestPath 支持后
        // 可替换为原生查询。
        info!(%from, %to, "Computing shortest path (application-level BFS)");

        // 检查两端节点是否存在
        let to_query = queries::get_document_by_id(&self.schema, to);
        if let Ok(resp) = self.client.execute_read(to_query).await
            && extract_properties(&resp, "doc").is_none_or(|a| a.is_empty())
        {
            return Ok(None);
        }

        // 从 from 出发做双向 BFS，最大深度 5
        let max_depth = 5u32;
        let from_steps = self
            .traverse(
                from,
                &[EdgeType::RelatedTo, EdgeType::Contains, EdgeType::BelongsTo],
                TraversalDirection::Both,
                max_depth,
            )
            .await?;

        // 在结果中查找 to 节点
        let path = from_steps.iter().find(|s| s.node.id == to).map(|s| {
            vec![
                TraversalStep {
                    node: GraphNode {
                        id: from.to_string(),
                        labels: vec![],
                        properties: HashMap::new(),
                        distance: 0,
                    },
                    via_edge: None,
                },
                s.clone(),
            ]
        });

        Ok(path)
    }

    async fn expand(
        &self,
        start_chunk_ids: &[String],
        edge_types: &[EdgeType],
        max_depth: u32,
    ) -> Result<Vec<GraphNode>, KnowledgeError> {
        if start_chunk_ids.is_empty() {
            return Ok(vec![]);
        }

        let edge_labels: Vec<String> = edge_types.iter().map(|et| self.edge_label(et)).collect();
        let query =
            queries::expand_from_chunks(&self.schema, start_chunk_ids, &edge_labels, max_depth);
        let response = self.client.execute_read(query).await?;

        let nodes: Vec<GraphNode> = extract_properties(&response, "expanded")
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let id = parse_id_value(item.get("document_id")?)?;
                        let distance =
                            item.get("distance").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let labels: Vec<String> = item
                            .get("label")
                            .and_then(|v| v.as_str())
                            .map(|s| vec![s.to_string()])
                            .unwrap_or_default();

                        Some(GraphNode {
                            id,
                            labels,
                            properties: HashMap::new(),
                            distance,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // 去重
        let mut seen = HashSet::new();
        let deduped: Vec<GraphNode> = nodes
            .into_iter()
            .filter(|n| seen.insert(n.id.clone()))
            .collect();

        Ok(deduped)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// CombinedSearch
// ═══════════════════════════════════════════════════════════════════════════

#[async_trait]
impl CombinedSearch for HelixDbBackend {
    async fn combined_search(
        &self,
        query: &CombinedQuery,
    ) -> Result<Vec<SearchResult>, KnowledgeError> {
        info!(
            query_text = %query.query_text,
            vec_k = query.vector_top_k,
            txt_k = query.text_top_k,
            graph_depth = query.graph_expansion_depth,
            "Executing HelixDB combined search"
        );

        // 组合搜索是 `HybridSearchEngine` 的首选路径，同样要在发 HTTP 之前
        // 守住查询向量维度，否则它会绕过 `VectorIndex::search` 的守卫。
        self.ensure_dims(&self.schema.fragment_node_label, &query.query_vector)?;

        // 1. 构建并发送单次 readBatch
        let batch = queries::combined_search_query(&self.schema, query);
        let response = self.client.execute_read(batch).await?;

        // 2. 解析 vector_path
        let (vec_doc_scores, vec_graph_nodes) = parse_path_results(&response, "vector_path");

        // 3. 解析 text_path
        let (txt_doc_scores, txt_graph_nodes) = parse_path_results(&response, "text_path");

        // 4. 合并每条路径的 chunk 命中 + 图扩展结果
        let mut vector_list: Vec<(String, f32)> = Vec::new();
        for (doc_id, score) in vec_doc_scores {
            vector_list.push((doc_id, score));
        }
        for node in vec_graph_nodes {
            let score = distance_to_score(node.distance as f64);
            vector_list.push((node.document_id, score));
        }

        let mut text_list: Vec<(String, f32)> = Vec::new();
        for (doc_id, score) in txt_doc_scores {
            text_list.push((doc_id, score));
        }
        for node in txt_graph_nodes {
            let score = distance_to_score(node.distance as f64);
            text_list.push((node.document_id, score));
        }

        // 5. 按 document_id 去重（保留最高分）
        let vector_list = dedup_by_doc_id(vector_list);
        let text_list = dedup_by_doc_id(text_list);

        // 6. RRF 融合
        let vec_weight: f32 = 0.5;
        let txt_weight: f32 = 0.5;

        let vec_refs: Vec<(String, f32)> = vector_list.clone();
        let txt_refs: Vec<(String, f32)> = text_list.clone();

        let ranked_lists: Vec<(f32, Vec<(String, f32)>)> =
            vec![(vec_weight, vec_refs), (txt_weight, txt_refs)];

        let refs: Vec<(f32, &[(String, f32)])> = ranked_lists
            .iter()
            .map(|(w, v)| (*w, v.as_slice()))
            .collect();

        let fused = fusion::rrf_fuse(&refs, &query.fusion);

        if fused.is_empty() {
            return Ok(vec![]);
        }

        // 7. 取 top-K 并获取文档内容
        let top_k = query.vector_top_k.min(query.text_top_k).max(10);
        let mut results = Vec::new();

        for (doc_id, score) in fused.into_iter().take(top_k) {
            let doc = self.get(&doc_id).await?;
            if let Some(doc) = doc {
                let snippet: String = doc.content.chars().take(500).collect();
                results.push(SearchResult {
                    document_id: doc_id,
                    title: doc.title,
                    snippet,
                    score,
                    source_path: doc.source_path,
                    match_sources: Vec::new(),
                    confidence: ConfidenceLevel::Medium,
                    diagnostic: None,
                });
            }
        }

        info!(result_count = results.len(), "Combined search completed");
        Ok(results)
    }
}

// ── CombinedSearch 辅助类型和函数 ────────────────────────────────────────

/// 从 HelixDB 响应中解析的图扩展节点（内部类型）。
struct ParsedGraphNode {
    document_id: String,
    distance: u32,
}

/// 解析一条 path（vector_path 或 text_path）的结果。
///
/// 返回：
/// * `doc_scores` — 分块命中行中的 (document_id, score) 集合
/// * `graph_nodes` — 图扩展行中的文档节点集合
fn parse_path_results(
    response: &serde_json::Value,
    path_name: &str,
) -> (HashMap<String, f32>, Vec<ParsedGraphNode>) {
    let mut doc_scores: HashMap<String, f32> = HashMap::new();
    let mut graph_nodes: Vec<ParsedGraphNode> = Vec::new();

    let rows = extract_properties(response, path_name)
        .map(|arr| arr.to_vec())
        .unwrap_or_default();

    for row in rows {
        // 如果行包含 chunk_id → 分块命中行
        if row.get("chunk_id").and_then(parse_id_value).is_some() {
            if let Some(doc_id) = row.get("document_id").and_then(parse_id_value) {
                let distance = row.get("score").map_or(0.0, parse_distance);
                let score = distance_to_score(distance);
                let entry = doc_scores.entry(doc_id.to_string()).or_insert(0.0);
                if score > *entry {
                    *entry = score;
                }
            }
        }
        // 如果行包含 graph_distance → 图扩展文档行
        else if row.get("graph_distance").is_some()
            && let Some(doc_id) = row.get("document_id").and_then(parse_id_value)
        {
            let distance = row
                .get("graph_distance")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            graph_nodes.push(ParsedGraphNode {
                document_id: doc_id.to_string(),
                distance,
            });
        }
    }

    (doc_scores, graph_nodes)
}

/// 按 document_id 去重，保留每个 doc_id 的最高分数。
fn dedup_by_doc_id(items: Vec<(String, f32)>) -> Vec<(String, f32)> {
    let mut map: HashMap<String, f32> = HashMap::new();
    for (id, score) in items {
        let entry = map.entry(id).or_insert(0.0);
        if score > *entry {
            *entry = score;
        }
    }
    let mut result: Vec<_> = map.into_iter().collect();
    result.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    result
}

// ═══════════════════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 未监听任何服务的端点 —— 守卫若在 HTTP 之后，这里拿到的会是
    /// `Internal("HTTP request failed: ...")` 而不是 `DimensionMismatch`，
    /// 因此该用例同时验证了「守卫先于网络」。
    const DEAD_ENDPOINT: &str = "http://localhost:19999";

    fn chunk(dims: usize) -> Chunk {
        Chunk {
            id: "doc-0001-abcd".into(),
            document_id: "doc-0001".into(),
            text: "测试分块".into(),
            sequence_index: 0,
            page_number: None,
            embedding: vec![0.5; dims],
            metadata: ChunkMetadata::default(),
        }
    }

    fn document() -> Document {
        Document {
            id: "doc-0001".into(),
            kb_id: None,
            title: "测试文档".into(),
            source_path: "test.txt".into(),
            content: "测试内容".into(),
            metadata: DocumentMetadata::default(),
        }
    }

    /// 查询向量维度不符时必须在发 HTTP 之前拒绝。
    #[tokio::test]
    async fn search_rejects_mismatched_query_dim() {
        let backend = HelixDbBackend::connect(DEAD_ENDPOINT, 4).await.unwrap();

        let err = VectorIndex::search(&backend, &[0.1, 0.2, 0.3], 5, None)
            .await
            .expect_err("3 维查询向量不应被 4 维索引接受");

        match err {
            KnowledgeError::DimensionMismatch {
                table_name,
                expected,
                actual,
            } => {
                assert_eq!(table_name, "Chunk");
                assert_eq!(expected, 4);
                assert_eq!(actual, 3);
            }
            other => panic!("期望 DimensionMismatch，实际为 {other:?}"),
        }
    }

    /// 空查询向量不是「未提供向量」而是调用方 bug，同样拒绝。
    #[tokio::test]
    async fn search_rejects_empty_query_vec() {
        let backend = HelixDbBackend::connect(DEAD_ENDPOINT, 4).await.unwrap();

        let err = VectorIndex::search(&backend, &[], 5, None)
            .await
            .expect_err("空查询向量不应被接受");

        assert!(matches!(
            err,
            KnowledgeError::DimensionMismatch { actual: 0, .. }
        ));
    }

    /// 组合搜索走的是独立入口，必须同样守住查询向量维度。
    #[tokio::test]
    async fn combined_search_rejects_mismatched_query_dim() {
        let backend = HelixDbBackend::connect(DEAD_ENDPOINT, 4).await.unwrap();

        let err = backend
            .combined_search(&CombinedQuery {
                query_text: "测试".into(),
                query_vector: vec![0.1, 0.2],
                vector_top_k: 5,
                text_top_k: 5,
                graph_expansion_depth: 0,
                graph_edge_types: vec![],
                fusion: RrfConfig::default(),
                filters: None,
            })
            .await
            .expect_err("2 维查询向量不应被 4 维索引接受");

        match err {
            KnowledgeError::DimensionMismatch {
                expected, actual, ..
            } => {
                assert_eq!((expected, actual), (4, 2));
            }
            other => panic!("期望 DimensionMismatch，实际为 {other:?}"),
        }
    }

    /// 摄入写路径上的分块 embedding 维度不符时，在发出任何写请求之前拒绝。
    #[tokio::test]
    async fn store_rejects_mismatched_chunk_embedding() {
        let backend = HelixDbBackend::connect(DEAD_ENDPOINT, 4).await.unwrap();

        let err = backend
            .store(document(), vec![chunk(3)])
            .await
            .expect_err("3 维分块 embedding 不应被 4 维索引接受");

        match err {
            KnowledgeError::DimensionMismatch {
                table_name,
                expected,
                actual,
            } => {
                assert_eq!(table_name, "Chunk");
                assert_eq!((expected, actual), (4, 3));
            }
            other => panic!("期望 DimensionMismatch，实际为 {other:?}"),
        }
    }

    /// 向量写路径同样 fail-closed。
    #[tokio::test]
    async fn upsert_rejects_mismatched_embedding() {
        let backend = HelixDbBackend::connect(DEAD_ENDPOINT, 4).await.unwrap();

        let err = backend
            .upsert(&[VectorEntry {
                id: "doc-0001-abcd".into(),
                document_id: "doc-0001".into(),
                vector: vec![0.1, 0.2, 0.3, 0.4, 0.5],
                text: "测试分块".into(),
            }])
            .await
            .expect_err("5 维 embedding 不应被 4 维索引接受");

        match err {
            KnowledgeError::DimensionMismatch {
                expected, actual, ..
            } => {
                assert_eq!((expected, actual), (4, 5));
            }
            other => panic!("期望 DimensionMismatch，实际为 {other:?}"),
        }
    }

    /// 回归保护：空 embedding 表示「本次未提供向量」（非 Full 存储模式的
    /// 正常形态），守卫放行 —— 此时失败发生在 HTTP 层而非维度守卫。
    #[tokio::test]
    async fn store_allows_absent_embedding() {
        let backend = HelixDbBackend::connect(DEAD_ENDPOINT, 4).await.unwrap();

        let err = backend
            .store(document(), vec![chunk(0)])
            .await
            .expect_err("死端点上的写入必然失败");

        assert!(
            !matches!(err, KnowledgeError::DimensionMismatch { .. }),
            "空 embedding 不应触发维度守卫，实际为 {err:?}"
        );
    }

    /// 回归保护：维度一致时守卫放行（同样落在 HTTP 层失败）。
    #[tokio::test]
    async fn store_allows_matching_embedding() {
        let backend = HelixDbBackend::connect(DEAD_ENDPOINT, 4).await.unwrap();

        let err = backend
            .store(document(), vec![chunk(4)])
            .await
            .expect_err("死端点上的写入必然失败");

        assert!(
            !matches!(err, KnowledgeError::DimensionMismatch { .. }),
            "维度一致的 embedding 不应触发维度守卫，实际为 {err:?}"
        );
    }
}
