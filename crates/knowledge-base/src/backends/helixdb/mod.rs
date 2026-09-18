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

/// `GraphNode.labels` 为空时的兜底节点标签。
///
/// `KnowledgeBase::add_facts` 写入的实体节点用 `"Entity"`，HelixDB 又要求
/// 每个节点有且只有一个 label，因此这里以它作为缺省值。
const DEFAULT_NODE_LABEL: &str = "Entity";

/// `EdgeType::Mentions` 对应的边标签 —— 该类型没有 schema 字段。
const MENTIONS_EDGE: &str = "MENTIONS";

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

/// 解析 Count 步骤的结果，缺失或形态不符时返回 0。
fn count_value(response: &Value, key: &str) -> usize {
    response
        .get(key)
        .and_then(|v| v.get("count"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize
}

/// 解析 [`queries::traverse_graph`] 返回的分层结果（`d1`..`dN`）。
///
/// `node_id` 是 `schema.id_property`（稳定身份）而非内部 `$id`；`name` 回填进
/// `properties`。`N(start) → Repeat` 会把起点自身也 emit 出来，这里按稳定 id 过滤掉。
///
/// `via_edge` 一律留空 —— 节点侧遍历拿不到边标签，由调用方在能拿到时回填。
///
/// 层从浅到深扫描，`seen` 保留首次出现 —— 也就是最小跳数：深层里的重复节点不会
/// 覆盖浅层给出的更小距离。某层缺失时跳过，不影响其余层。
fn parse_traversed_nodes(
    response: &Value,
    start_node_id: &str,
    max_depth: u32,
) -> Vec<TraversalStep> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut steps: Vec<TraversalStep> = Vec::new();

    for depth in 1..=max_depth {
        let Some(items) = extract_properties(response, &queries::depth_query_name(depth)) else {
            continue;
        };
        for item in items {
            let Some(node_id) = item.get("node_id").and_then(parse_id_value) else {
                continue;
            };
            if node_id == start_node_id {
                continue;
            }
            if !seen.insert(node_id.clone()) {
                continue;
            }

            let labels = item
                .get("label")
                .and_then(|v| v.as_str())
                .map(|s| vec![s.to_string()])
                .unwrap_or_default();
            let properties = item
                .get("name")
                .and_then(|v| v.as_str())
                .map(|name| HashMap::from([("name".to_string(), name.to_string())]))
                .unwrap_or_default();

            steps.push(TraversalStep {
                node: GraphNode {
                    id: node_id,
                    labels,
                    properties,
                    distance: depth,
                },
                via_edge: None,
            });
        }
    }

    steps
}

/// 把 `GraphStore::traverse` 的边类型入参展开成一组遍历查询计划。
///
/// 每个元素是一次遍历：`None` 表示通配（不限标签），`Some(et)` 表示只走该标签。
///
/// 空 `edge_types` 是「不限定边类型」而不是「只走几个预定义类型」。这里发**一次
/// 通配遍历**，而不是枚举固定标签集合：`EdgeType::Custom(predicate)` 的标签就是
/// 谓词文本本身（`add_facts` 写入的「朋友」「性别」等），取值空间不受控，
/// 任何枚举都必然漏 —— 漏掉的部分正是 `query_entity_facts` 查不到的根因。
fn traversal_plan(edge_types: &[EdgeType]) -> Vec<Option<EdgeType>> {
    if edge_types.is_empty() {
        vec![None]
    } else {
        edge_types.iter().cloned().map(Some).collect()
    }
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
            EdgeType::Mentions => MENTIONS_EDGE.to_string(),
            EdgeType::BelongsTo => self.schema.belongs_to_edge.clone(),
            EdgeType::NextChunk => self.schema.next_fragment_edge.clone(),
            EdgeType::Custom(s) => s.clone(),
        }
    }

    /// [`Self::edge_label`] 的逆向映射：把 HelixDB 返回的边标签还原成 `EdgeType`。
    ///
    /// 未命中任何固定标签的一律归为 `EdgeType::Custom` —— `add_facts` 写入的
    /// 关系边就是这样（标签即谓词文本）。
    fn edge_type_from_label(&self, label: &str) -> EdgeType {
        let s = &self.schema;
        if label == s.contains_edge {
            EdgeType::Contains
        } else if label == s.related_edge {
            EdgeType::RelatedTo
        } else if label == s.next_fragment_edge {
            EdgeType::NextChunk
        } else if label == s.belongs_to_edge {
            EdgeType::BelongsTo
        } else if label == MENTIONS_EDGE {
            EdgeType::Mentions
        } else {
            EdgeType::Custom(label.to_string())
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

    /// 不限边类型的遍历（`traverse` 收到空 `edge_types` 时走这里）。
    ///
    /// 分两路取数，因为没有任何单条查询能同时给出「多跳」和「边标签」：
    ///
    /// 1. [`Self::adjacent_steps`] —— 边流 `EdgeProperties` + 节点流投影双查询，
    ///    按内部 `$id` 关联，拿回直连边的**真实**标签（`via_edge` 还原成谓词）与
    ///    邻接点的稳定 id + name；
    /// 2. [`queries::traverse_graph`] 的通配形态（`{"Out"|"In"|"Both": null}`）——
    ///    覆盖 `max_depth` 以内的更远节点（稳定 id + name），但拿不到边标签
    ///    （`Repeat` 不能在边流上续接，`$distance` 也不返回），故 `via_edge` 为空。
    ///
    /// 第 1 路排在前，配合 `traverse` 末尾的按 node_id 去重，让同一个邻接点优先
    /// 保留带标签的那一条。
    async fn wildcard_steps(
        &self,
        start_node: &str,
        direction: TraversalDirection,
        max_depth: u32,
    ) -> Result<Vec<TraversalStep>, KnowledgeError> {
        // `max_depth == 0` 表示不遍历：直连边本身也算一跳，且分层查询会退化成
        // 空 batch（HelixDB 不接受空 queries），故直接返回空。
        if max_depth == 0 {
            return Ok(Vec::new());
        }

        let mut steps = self.adjacent_steps(start_node, direction).await?;

        let query = queries::traverse_graph(
            &self.schema,
            start_node,
            None,
            Self::direction_str(direction),
            max_depth,
        );
        let response = self.client.execute_read(query).await?;
        steps.extend(parse_traversed_nodes(&response, start_node, max_depth));

        Ok(steps)
    }

    /// 起始节点的直连边 → `TraversalStep`，`via_edge` 为边的真实标签。
    ///
    /// HelixDB 不允许在边流上做 `Project`，而 `EdgeProperties` 又是终结步骤，所以
    /// 用 [`queries::adjacent_labeled_nodes`] 的双查询拿「边标签」与「邻接点身份」，
    /// 再按内部 `$id` 关联：`OutE` 的邻接点是 `$to`（`OutN` 落到目标端点），
    /// `InE` 的邻接点是 `$from`（`InN` 落到源端点）。`Both` 拆成两次查询 ——
    /// 单次 `BothE` 无法判断哪一端才是起始节点。
    async fn adjacent_steps(
        &self,
        start_node: &str,
        direction: TraversalDirection,
    ) -> Result<Vec<TraversalStep>, KnowledgeError> {
        let dirs: &[(&str, &str, &str)] = match direction {
            TraversalDirection::Outgoing => &[("OutE", "OutN", "$to")],
            TraversalDirection::Incoming => &[("InE", "InN", "$from")],
            TraversalDirection::Both => &[("OutE", "OutN", "$to"), ("InE", "InN", "$from")],
        };

        let mut steps = Vec::new();
        for (edge_step, node_step, neighbor_key) in dirs {
            let query = queries::adjacent_labeled_nodes(
                &self.schema,
                start_node,
                edge_step,
                node_step,
            );
            let response = self.client.execute_read(query).await?;

            // 邻接点身份表：内部 `$id` → (稳定 id, name)。`nodes` 与 `edges` 覆盖同一批
            // 直连边，按内部 `$id` 关联，与返回行序无关。
            let identity: HashMap<String, (String, String)> =
                extract_properties(&response, "nodes")
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|row| {
                                let internal = parse_id_value(row.get("internal_id")?)?;
                                let node_id = parse_id_value(row.get("node_id")?)?;
                                let name = row
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string();
                                Some((internal, (node_id, name)))
                            })
                            .collect()
                    })
                    .unwrap_or_default();

            let rows = extract_properties(&response, "edges")
                .map(|arr| arr.as_slice())
                .unwrap_or_default();

            for row in rows {
                // `$from`/`$to` 是内部自增整数 id；`$label` 是边标签（即谓词文本）。
                let neighbor = row.get(*neighbor_key).and_then(parse_id_value);
                let label = row.get("$label").and_then(|v| v.as_str());
                let (Some(neighbor), Some(label)) = (neighbor, label) else {
                    continue;
                };

                // 用内部 `$id` 反查稳定 id 与 name。查不到时退化为内部 id，保证既不
                // 丢边、也不给错配的标签。
                let (node_id, name) = identity
                    .get(&neighbor)
                    .cloned()
                    .unwrap_or_else(|| (neighbor.clone(), String::new()));
                let mut properties = HashMap::new();
                if !name.is_empty() {
                    properties.insert("name".to_string(), name);
                }

                steps.push(TraversalStep {
                    node: GraphNode {
                        id: node_id,
                        labels: Vec::new(),
                        properties,
                        distance: 1,
                    },
                    via_edge: Some(self.edge_type_from_label(label)),
                });
            }
        }

        Ok(steps)
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
        Ok(count_value(&response, "count"))
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

    /// 插入节点（按 ID 幂等）。
    ///
    /// HelixDB 没有 MERGE/upsert 语义，AddN 对同一 ID 重复调用会留下多个
    /// 同 ID 顶点 —— 之后按 ID 匹配与遍历都会重复计数。因此这里先查后建：
    /// 已存在的节点直接返回，不重建也不改写属性。
    async fn upsert_node(&self, node: GraphNode) -> Result<(), KnowledgeError> {
        if self.node_exists(&node.id).await? {
            debug!(node_id = %node.id, "Node already exists, skipping insert");
            return Ok(());
        }

        let label = node
            .labels
            .first()
            .cloned()
            .unwrap_or_else(|| DEFAULT_NODE_LABEL.to_string());
        let query = queries::create_node(&self.schema, &label, &node.id, &node.properties);
        self.client.execute_write(query).await?;
        debug!(node_id = %node.id, %label, "Node inserted");
        Ok(())
    }

    async fn get_node(&self, node_id: &str) -> Result<Option<GraphNode>, KnowledgeError> {
        let query = queries::get_node_by_id(&self.schema, node_id);
        let response = self.client.execute_read(query).await?;

        let node = extract_properties(&response, "node")
            .and_then(|arr| arr.first())
            .map(|item| {
                let id = item
                    .get("id")
                    .and_then(parse_id_value)
                    .unwrap_or_else(|| node_id.to_string());
                let labels = item
                    .get("label")
                    .and_then(|v| v.as_str())
                    .map(|s| vec![s.to_string()])
                    .unwrap_or_default();
                let properties = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|name| HashMap::from([("name".to_string(), name.to_string())]))
                    .unwrap_or_default();

                GraphNode {
                    id,
                    labels,
                    properties,
                    distance: 0,
                }
            });

        Ok(node)
    }

    async fn node_exists(&self, node_id: &str) -> Result<bool, KnowledgeError> {
        let query = queries::count_node_by_id(&self.schema, node_id);
        let response = self.client.execute_read(query).await?;
        Ok(count_value(&response, "count") > 0)
    }

    async fn traverse(
        &self,
        start_node: &str,
        edge_types: &[EdgeType],
        direction: TraversalDirection,
        max_depth: u32,
    ) -> Result<Vec<TraversalStep>, KnowledgeError> {
        // 见 `wildcard_steps` 的说明：0 跳即不遍历；上限见 `MAX_TRAVERSAL_DEPTH`。
        if max_depth == 0 {
            return Ok(Vec::new());
        }
        let max_depth = max_depth.min(MAX_TRAVERSAL_DEPTH);

        let dir_str = Self::direction_str(direction);
        let mut all_steps: Vec<TraversalStep> = Vec::new();

        // 空 edge_types 表示「不限定边类型」，见 `traversal_plan` 的说明。
        for plan in traversal_plan(edge_types) {
            match plan {
                Some(et) => {
                    let label = self.edge_label(&et);
                    let query = queries::traverse_graph(
                        &self.schema,
                        start_node,
                        Some(&label),
                        dir_str,
                        max_depth,
                    );
                    let response = self.client.execute_read(query).await?;

                    let mut steps = parse_traversed_nodes(&response, start_node, max_depth);
                    for step in &mut steps {
                        step.via_edge = Some(et.clone());
                    }
                    all_steps.extend(steps);
                }
                None => {
                    all_steps.extend(
                        self.wildcard_steps(start_node, direction, max_depth)
                            .await?,
                    );
                }
            }
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
        edge_types: &[EdgeType],
        _max_depth: u32,
    ) -> Result<Option<Vec<TraversalStep>>, KnowledgeError> {
        // HelixDB 当前版本不直接支持 shortestPath 查询步骤。
        // 作为简化实现，通过双向 BFS 在应用层实现最短路径搜索。
        // 对于生产使用，当 HelixDB 添加原生 shortestPath 支持后
        // 可替换为原生查询。
        info!(%from, %to, "Computing shortest path (application-level BFS)");

        // 检查终点是否存在。这里必须问「节点」而非「文档」：`to` 是
        // `compute_entity_id(...)` 的内容哈希实体 id，`get_document_by_id` 按
        // `id_property` 匹配但不按 label 过滤，拿实体 id 去问会命中实体顶点，
        // 属于巧合式放行。`node_exists` 才是语义正确的存在性判定。
        if !self.node_exists(to).await? {
            return Ok(None);
        }

        // 从 from 出发做双向 BFS，最大深度 5。
        // `edge_types` 原样透传给 `traverse`：空切片表示不限定边类型（通配），
        // 否则只走调用方指定的标签。硬编码一个固定标签子集会漏掉
        // `EdgeType::Custom` 关系边，使两个实体间的路径永远查不到。
        let max_depth = 5u32;
        let from_steps = self
            .traverse(from, edge_types, TraversalDirection::Both, max_depth)
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

    /// ② 空 `edge_types` 必须展开成一次**通配**遍历，而不是枚举固定标签集合。
    ///
    /// `add_facts` 写入的边全部是 `EdgeType::Custom(predicate)`，标签即谓词文本
    /// （「朋友」「性别」…），取值空间不受控。任何固定枚举都必然漏掉它们，
    /// 这正是 `query_entity_facts` 只写不读的根因。
    #[test]
    fn empty_edge_types_plan_is_a_single_wildcard() {
        let plan = traversal_plan(&[]);
        assert_eq!(plan, vec![None], "空 edge_types 应展开为一次通配遍历");
        assert_eq!(plan.len(), 1, "通配只需一次请求，不应逐个枚举标签");

        // 显式指定时不通配，逐个走对应标签
        let plan = traversal_plan(&[EdgeType::RelatedTo, EdgeType::Custom("朋友".into())]);
        assert_eq!(
            plan,
            vec![
                Some(EdgeType::RelatedTo),
                Some(EdgeType::Custom("朋友".into()))
            ]
        );
    }

    /// ② 通配遍历实际发出的方向步骤是 `{"<dir>": null}` —— `null` 才是通配，
    /// 空串和 `"*"` 都会被当成字面标签从而查不到任何边。
    #[test]
    fn wildcard_traversal_emits_null_label() {
        let schema = HelixSchema::default();

        // 方向步骤嵌在每条层查询的 Repeat.traversal 里（queries[0] 即 d1 层）
        let direction_step = |q: &serde_json::Value| {
            q["query"]["queries"][0]["Query"]["steps"][1]["Repeat"]["traversal"]["steps"][0].clone()
        };

        for dir in ["Out", "In", "Both"] {
            let q = queries::traverse_graph(&schema, "entity:Entity:abc", None, dir, 2);
            let step = direction_step(&q);
            assert_eq!(step, serde_json::json!({ dir: null }));
            assert_ne!(step, serde_json::json!({ dir: "" }));
            assert_ne!(step, serde_json::json!({ dir: "*" }));
        }

        // 显式标签仍是字符串
        let q = queries::traverse_graph(&schema, "entity:Entity:abc", Some("朋友"), "Out", 2);
        assert_eq!(direction_step(&q), serde_json::json!({"Out": "朋友"}));
    }

    /// ⑦ 分层遍历：`returns` 为 `d1..dN`，第 k 条子查询的 `max_depth` 恰为 k，
    /// 起点内联在每条查询里（不再单发 `start` 查询）。
    #[test]
    fn traverse_graph_is_layered_by_depth() {
        let schema = HelixSchema::default();
        let q = queries::traverse_graph(&schema, "entity:Entity:abc", None, "Both", 3);

        assert_eq!(q["request_type"], "read");
        assert_eq!(q["query"]["returns"], serde_json::json!(["d1", "d2", "d3"]));

        let queries = q["query"]["queries"].as_array().unwrap();
        assert_eq!(queries.len(), 3, "层数 = max_depth");
        for (idx, depth) in (1..=3u32).enumerate() {
            let query = &queries[idx];
            assert_eq!(query["Query"]["name"], queries::depth_query_name(depth));
            assert_eq!(
                query["Query"]["steps"][0],
                serde_json::json!({"NWhere": {"Eq": ["id", {"String": "entity:Entity:abc"}]}})
            );
            assert_eq!(query["Query"]["steps"][1]["Repeat"]["max_depth"], depth);
        }
    }

    /// ⑦ 跳数由层号推出：第 k 层的节点 distance 为 k，起点自身不入结果，
    /// 跨层重复只保留最小跳数。
    #[test]
    fn distance_comes_from_the_layer_index() {
        let response = serde_json::json!({
            "d1": {"properties": [
                {"node_id": "entity:Entity:start", "name": "小C"},
                {"node_id": "entity:Entity:chen", "name": "chen"}
            ]},
            "d2": {"properties": [
                {"node_id": "entity:Entity:start", "name": "小C"},
                {"node_id": "entity:Entity:chen", "name": "chen"},
                {"node_id": "entity:Entity:beauty", "name": "美女"}
            ]}
        });

        let steps = parse_traversed_nodes(&response, "entity:Entity:start", 2);
        let by_id: HashMap<_, _> = steps.iter().map(|s| (s.node.id.as_str(), s)).collect();

        assert_eq!(steps.len(), 2, "起点不入结果 + 跨层重复只留一次");
        assert_eq!(by_id["entity:Entity:chen"].node.distance, 1);
        assert_eq!(by_id["entity:Entity:beauty"].node.distance, 2);
        assert_eq!(by_id["entity:Entity:beauty"].node.properties["name"], "美女");
        assert!(!by_id.contains_key("entity:Entity:start"));
    }

    /// ⑦ 缺层不致命：某一层没返回时跳过该层，其余层照常给出结果。
    #[test]
    fn missing_layer_is_skipped() {
        let response = serde_json::json!({
            "d1": {"properties": [{"node_id": "entity:Entity:chen", "name": "chen"}]}
        });

        let steps = parse_traversed_nodes(&response, "entity:Entity:start", 3);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].node.distance, 1);
    }

    /// ② 直连边双查询：边流取 `EdgeProperties`（`$label`/`$to`/`$from`），
    /// 节点流取 `OutN` 投影（内部 `$id` + 稳定 id + name），两者按内部 `$id`
    /// 关联。断言三种 query 的结构、通配 `null` 标签与 `EdgeProperties` 终结。
    #[test]
    fn adjacent_labeled_nodes_batch_shape() {
        let schema = HelixSchema::default();
        let q = queries::adjacent_labeled_nodes(&schema, "entity:Entity:abc", "OutE", "OutN");

        assert_eq!(q["request_type"], "read");
        let queries_arr = q["query"]["queries"].as_array().unwrap();
        assert_eq!(queries_arr.len(), 3, "应包含 start/edges/nodes 三条查询");

        // start：按稳定 id 定位起始节点
        let start_steps = &queries_arr[0]["Query"]["steps"];
        assert_eq!(
            start_steps[0],
            serde_json::json!({"NWhere": {"Eq": [schema.id_property, {"String": "entity:Entity:abc"}]}})
        );

        // edges：边流以 EdgeProperties 终结，拿 $label/$from/$to
        let edges_steps = &queries_arr[1]["Query"]["steps"];
        assert_eq!(edges_steps[0], serde_json::json!({"N": {"Var": "start"}}));
        assert_eq!(edges_steps[1], serde_json::json!({"OutE": null}));
        assert_eq!(edges_steps[2], serde_json::json!({"EdgeProperties": null}));
        assert!(
            !edges_steps
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s.get("Project").is_some()),
            "边流上 Project 会报错，不能出现"
        );

        // nodes：同一批直连边，投影内部 $id + 稳定 id + name
        let nodes_steps = &queries_arr[2]["Query"]["steps"];
        assert_eq!(nodes_steps[0], serde_json::json!({"N": {"Var": "start"}}));
        assert_eq!(nodes_steps[1], serde_json::json!({"OutE": null}));
        assert_eq!(nodes_steps[2], serde_json::json!({"OutN": null}));
        assert_eq!(
            nodes_steps[3],
            serde_json::json!({"Project": [
                {"source": "$id", "alias": "internal_id"},
                {"source": schema.id_property, "alias": "node_id"},
                {"source": "name", "alias": "name"}
            ]})
        );

        assert_eq!(q["query"]["returns"], serde_json::json!(["edges", "nodes"]));
    }

    /// ① `GraphStore` 的默认实现是「空操作 / 永远不存在」—— 正是顶点写不进去的
    /// 原因。真实实现必须真的发请求：在死端点上三种操作都应报错，而不是
    /// 静默返回 `Ok(())` / `Ok(None)` / `Ok(false)`。
    #[tokio::test]
    async fn node_ops_hit_the_backend_instead_of_trait_defaults() {
        let backend = HelixDbBackend::connect(DEAD_ENDPOINT, 4).await.unwrap();

        let node = GraphNode {
            id: "entity:Entity:abcd1234".into(),
            labels: vec!["Entity".into()],
            properties: HashMap::from([("name".to_string(), "小C".to_string())]),
            distance: 0,
        };

        let err = backend
            .upsert_node(node.clone())
            .await
            .expect_err("upsert_node 不应静默成功");
        assert!(
            !matches!(err, KnowledgeError::DimensionMismatch { .. }),
            "upsert_node 应在 HTTP 层失败，实际为 {err:?}"
        );

        backend
            .get_node(&node.id)
            .await
            .expect_err("get_node 不应静默返回 Ok(None)");

        backend
            .node_exists(&node.id)
            .await
            .expect_err("node_exists 不应静默返回 Ok(false)");
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
