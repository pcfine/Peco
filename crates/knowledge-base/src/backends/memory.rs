use std::collections::{HashMap, VecDeque};

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::error::KnowledgeError;
use crate::traits::*;
use crate::types::*;

// ---------------------------------------------------------------------------
// 后端结构体
// ---------------------------------------------------------------------------

/// 内存后端 — 适用于测试和小规模场景（< 1000 个文档）。
///
/// 所有数据存储在 `HashMap` 和 `Vec` 中；进程重启会丢弃所有内容。
/// 向量搜索使用暴力余弦相似度。
pub struct InMemoryBackend {
    documents: RwLock<HashMap<DocumentId, Document>>,
    chunks: RwLock<HashMap<String, Chunk>>,
    /// `chunk_id → (document_id, vector)`
    vectors: RwLock<HashMap<String, (String, Vec<f32>)>>,
    edges: RwLock<Vec<KnowledgeEdge>>,
    /// 图节点存储：`node_id → GraphNode`
    nodes: RwLock<HashMap<String, GraphNode>>,
    /// 简单倒排索引：`lowercase_word → [(chunk_id, document_id)]`
    text_index: RwLock<HashMap<String, Vec<(String, String)>>>,
}

impl InMemoryBackend {
    pub fn new() -> Self {
        Self {
            documents: RwLock::new(HashMap::new()),
            chunks: RwLock::new(HashMap::new()),
            vectors: RwLock::new(HashMap::new()),
            edges: RwLock::new(Vec::new()),
            nodes: RwLock::new(HashMap::new()),
            text_index: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 余弦相似度 ∈ [‑1, 1]。对于零模长的向量返回 0.0。
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }
    dot / (mag_a * mag_b)
}

fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}' // CJK Unified Ideographs Extension A
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
        | '\u{2F800}'..='\u{2FA1F}' // CJK Compatibility Ideographs Supplement
    )
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut ascii_buf = String::new();

    for c in text.chars() {
        if is_cjk(c) {
            // 将缓冲区中累积的 ASCII 词元刷新。
            flush_ascii_buffer(&mut ascii_buf, &mut tokens);
            // CJK 字符 → 单字 unigram 词元。
            tokens.push(c.to_lowercase().to_string());
        } else if c.is_alphanumeric() {
            ascii_buf.push(c);
        } else {
            // 非字母数字、非 CJK（空格、标点等）→ 刷新缓冲区。
            flush_ascii_buffer(&mut ascii_buf, &mut tokens);
        }
    }
    flush_ascii_buffer(&mut ascii_buf, &mut tokens);

    tokens
}

/// 将累积的 ASCII 字母数字字符刷新为一个词元。
fn flush_ascii_buffer(buf: &mut String, tokens: &mut Vec<String>) {
    if !buf.is_empty() {
        tokens.push(buf.to_lowercase().clone());
        buf.clear();
    }
}

/// 用于回退全文搜索的简单词重叠评分。
///
/// 使用 CJK 感知的分词器：中文字符按单字 unigram 分词，
/// ASCII 文本按字母数字边界分词（空格、标点作为分隔符）。
/// 评分 = 匹配的查询词元数 / 查询词元总数。
fn bm25_like_score(query_tokens: &[String], doc_tokens: &[String]) -> f32 {
    let doc_set: std::collections::HashSet<_> = doc_tokens.iter().collect();
    let hits = query_tokens.iter().filter(|t| doc_set.contains(*t)).count();
    if query_tokens.is_empty() || hits == 0 {
        return 0.0;
    }
    hits as f32 / query_tokens.len() as f32
}

// ---------------------------------------------------------------------------
// DocumentStore
// ---------------------------------------------------------------------------

#[async_trait]
impl DocumentStore for InMemoryBackend {
    async fn store(&self, doc: Document, chunks: Vec<Chunk>) -> Result<(), KnowledgeError> {
        let mut docs = self.documents.write().await;
        let mut ch = self.chunks.write().await;

        docs.insert(doc.id.clone(), doc);
        for c in &chunks {
            ch.insert(c.id.clone(), c.clone());
        }
        Ok(())
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Document>, KnowledgeError> {
        Ok(self.documents.read().await.get(id).cloned())
    }

    async fn delete(&self, id: &DocumentId) -> Result<(), KnowledgeError> {
        // 移除文档。
        self.documents.write().await.remove(id);
        // 移除关联的分块。
        let chunk_ids: Vec<String> = {
            self.chunks
                .read()
                .await
                .values()
                .filter(|c| &c.document_id == id)
                .map(|c| c.id.clone())
                .collect()
        };
        {
            let mut ch = self.chunks.write().await;
            for cid in &chunk_ids {
                ch.remove(cid);
            }
        }
        // 移除向量。
        {
            let mut v = self.vectors.write().await;
            for cid in &chunk_ids {
                v.remove(cid);
            }
        }
        // 移除文本索引条目。
        {
            let mut ti = self.text_index.write().await;
            for (_word, entries) in ti.iter_mut() {
                entries.retain(|(_cid, did)| did != id);
            }
            ti.retain(|_, v| !v.is_empty());
        }
        // 移除边。
        {
            let mut e = self.edges.write().await;
            e.retain(|edge| edge.source_id != *id && edge.target_id != *id);
        }
        Ok(())
    }

    async fn list(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<DocumentSummary>, KnowledgeError> {
        let docs = self.documents.read().await;
        let chunks = self.chunks.read().await;

        let mut summaries: Vec<_> = docs
            .values()
            .skip(offset)
            .take(limit)
            .map(|d| {
                let chunk_count = chunks.values().filter(|c| c.document_id == d.id).count();
                DocumentSummary {
                    id: d.id.clone(),
                    title: d.title.clone(),
                    source_path: d.source_path.clone(),
                    chunk_count,
                    file_type: d.metadata.file_type.clone(),
                }
            })
            .collect();
        summaries.sort_by(|a, b| a.title.cmp(&b.title));
        Ok(summaries)
    }

    async fn chunks(&self, doc_id: &DocumentId) -> Result<Vec<Chunk>, KnowledgeError> {
        let mut result: Vec<_> = self
            .chunks
            .read()
            .await
            .values()
            .filter(|c| &c.document_id == doc_id)
            .cloned()
            .collect();
        result.sort_by_key(|c| c.sequence_index);
        Ok(result)
    }

    async fn stats(&self) -> Result<StoreStats, KnowledgeError> {
        let docs = self.documents.read().await;
        let chunks = self.chunks.read().await;
        let total_bytes: u64 = docs
            .values()
            .map(|d| d.content.len() as u64)
            .chain(chunks.values().map(|c| c.text.len() as u64))
            .sum();

        Ok(StoreStats {
            document_count: docs.len(),
            chunk_count: chunks.len(),
            total_bytes,
        })
    }
}

// ---------------------------------------------------------------------------
// VectorIndex
// ---------------------------------------------------------------------------

#[async_trait]
impl VectorIndex for InMemoryBackend {
    fn ndims(&self) -> usize {
        // 返回 0 表示"接受任意维度"。
        // 实际实现应返回其固定的 ndims。
        0
    }

    async fn search(
        &self,
        query_vec: &[f32],
        top_k: usize,
        filters: Option<&SearchFilters>,
    ) -> Result<Vec<VectorHit>, KnowledgeError> {
        let vectors = self.vectors.read().await;

        let mut scored: Vec<(f32, String, String)> = vectors
            .iter()
            .filter(|(_, (doc_id, _))| {
                if let Some(f) = filters
                    && let Some(ref dids) = f.document_ids
                {
                    return dids.contains(doc_id);
                }
                true
            })
            .map(|(chunk_id, (doc_id, vec))| {
                (
                    cosine_similarity(query_vec, vec),
                    chunk_id.clone(),
                    doc_id.clone(),
                )
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        Ok(scored
            .into_iter()
            .take(top_k)
            .map(|(score, chunk_id, document_id)| VectorHit {
                chunk_id,
                document_id,
                score,
            })
            .collect())
    }

    async fn upsert(&self, entries: &[VectorEntry]) -> Result<(), KnowledgeError> {
        let mut v = self.vectors.write().await;
        for e in entries {
            v.insert(e.id.clone(), (e.document_id.clone(), e.vector.clone()));
        }
        Ok(())
    }

    async fn remove(&self, ids: &[String]) -> Result<(), KnowledgeError> {
        let mut v = self.vectors.write().await;
        for id in ids {
            v.remove(id);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// GraphStore
// ---------------------------------------------------------------------------

#[async_trait]
impl GraphStore for InMemoryBackend {
    async fn add_edge(&self, edge: KnowledgeEdge) -> Result<(), KnowledgeError> {
        self.edges.write().await.push(edge);
        Ok(())
    }

    async fn add_edges(&self, edges: &[KnowledgeEdge]) -> Result<(), KnowledgeError> {
        self.edges.write().await.extend(edges.iter().cloned());
        Ok(())
    }

    async fn remove_node_edges(&self, node_id: &str) -> Result<(), KnowledgeError> {
        let nid = node_id.to_string();
        let mut e = self.edges.write().await;
        e.retain(|edge| edge.source_id != nid && edge.target_id != nid);
        Ok(())
    }

    async fn traverse(
        &self,
        start_node: &str,
        edge_types: &[EdgeType],
        direction: TraversalDirection,
        max_depth: u32,
    ) -> Result<Vec<TraversalStep>, KnowledgeError> {
        let edges = self.edges.read().await;
        let mut visited: HashMap<String, u32> = HashMap::new();
        let mut results: Vec<TraversalStep> = Vec::new();

        // 插入起始节点。
        visited.insert(start_node.to_string(), 0);
        results.push(TraversalStep {
            node: GraphNode {
                id: start_node.to_string(),
                labels: Vec::new(),
                properties: HashMap::new(),
                distance: 0,
            },
            via_edge: None,
        });

        // BFS
        let mut frontier: VecDeque<(String, u32)> = VecDeque::from([(start_node.to_string(), 0)]);

        while let Some((current, depth)) = frontier.pop_front() {
            if depth >= max_depth {
                continue;
            }

            let next_depth = depth + 1;

            for edge in edges.iter() {
                if !edge_types.is_empty() && !edge_types.contains(&edge.edge_type) {
                    continue;
                }

                let neighbor = match direction {
                    TraversalDirection::Outgoing if edge.source_id == current => &edge.target_id,
                    TraversalDirection::Incoming if edge.target_id == current => &edge.source_id,
                    TraversalDirection::Both if edge.source_id == current => &edge.target_id,
                    TraversalDirection::Both if edge.target_id == current => &edge.source_id,
                    _ => continue,
                };

                if visited.contains_key(neighbor) {
                    continue;
                }

                visited.insert(neighbor.clone(), next_depth);
                results.push(TraversalStep {
                    node: GraphNode {
                        id: neighbor.clone(),
                        labels: Vec::new(),
                        properties: edge.properties.clone(),
                        distance: next_depth,
                    },
                    via_edge: Some(edge.edge_type.clone()),
                });
                frontier.push_back((neighbor.clone(), next_depth));
            }
        }

        Ok(results)
    }

    async fn shortest_path(
        &self,
        from: &str,
        to: &str,
        edge_types: &[EdgeType],
        max_depth: u32,
    ) -> Result<Option<Vec<TraversalStep>>, KnowledgeError> {
        let edges = self.edges.read().await;
        let mut visited: HashMap<String, (u32, Option<String>, Option<EdgeType>)> = HashMap::new();
        // (node, depth, parent, via_edge)

        let mut frontier: VecDeque<(String, u32)> = VecDeque::from([(from.to_string(), 0)]);
        visited.insert(from.to_string(), (0, None, None));

        let mut found = false;

        while let Some((current, depth)) = frontier.pop_front() {
            if current == to {
                found = true;
                break;
            }
            if depth >= max_depth {
                continue;
            }

            let next_depth = depth + 1;
            for edge in edges.iter() {
                if !edge_types.is_empty() && !edge_types.contains(&edge.edge_type) {
                    continue;
                }

                // 最短路径使用无向遍历。
                let neighbor = if edge.source_id == current {
                    &edge.target_id
                } else if edge.target_id == current {
                    &edge.source_id
                } else {
                    continue;
                };

                if visited.contains_key(neighbor) {
                    continue;
                }

                visited.insert(
                    neighbor.clone(),
                    (
                        next_depth,
                        Some(current.clone()),
                        Some(edge.edge_type.clone()),
                    ),
                );
                frontier.push_back((neighbor.clone(), next_depth));
            }
        }

        if !found {
            return Ok(None);
        }

        // 重建路径。
        let mut path: Vec<TraversalStep> = Vec::new();
        let mut cur = to.to_string();
        loop {
            let (dist, parent, via) = visited
                .get(&cur)
                .cloned()
                .expect("target node must be in visited");
            path.push(TraversalStep {
                node: GraphNode {
                    id: cur.clone(),
                    labels: Vec::new(),
                    properties: HashMap::new(),
                    distance: dist,
                },
                via_edge: via,
            });
            match parent {
                Some(p) => cur = p,
                None => break,
            }
        }
        path.reverse();
        Ok(Some(path))
    }

    async fn expand(
        &self,
        start_chunk_ids: &[String],
        edge_types: &[EdgeType],
        max_depth: u32,
    ) -> Result<Vec<GraphNode>, KnowledgeError> {
        let mut all_nodes: Vec<GraphNode> = Vec::new();
        for cid in start_chunk_ids {
            let steps = self
                .traverse(cid, edge_types, TraversalDirection::Both, max_depth)
                .await?;
            all_nodes.extend(steps.into_iter().map(|s| s.node));
        }
        // 按 ID 去重。
        let mut seen = HashMap::new();
        all_nodes.retain(|n| seen.insert(n.id.clone(), ()).is_none());
        Ok(all_nodes)
    }

    async fn upsert_node(&self, node: GraphNode) -> Result<(), KnowledgeError> {
        self.nodes.write().await.insert(node.id.clone(), node);
        Ok(())
    }

    async fn get_node(&self, node_id: &str) -> Result<Option<GraphNode>, KnowledgeError> {
        Ok(self.nodes.read().await.get(node_id).cloned())
    }
}

// ---------------------------------------------------------------------------
// FullTextIndex
// ---------------------------------------------------------------------------

#[async_trait]
impl FullTextIndex for InMemoryBackend {
    async fn search(
        &self,
        query: &str,
        top_k: usize,
        filters: Option<&SearchFilters>,
    ) -> Result<Vec<FullTextHit>, KnowledgeError> {
        let query_tokens = tokenize(query);
        let chunks = self.chunks.read().await;
        let mut scored: Vec<(f32, String, String, String)> = Vec::new();

        for (cid, chunk) in chunks.iter() {
            // 应用过滤器。
            if let Some(f) = filters
                && let Some(ref dids) = f.document_ids
                && !dids.contains(&chunk.document_id)
            {
                continue;
            }

            let doc_tokens = tokenize(&chunk.text);
            let score = bm25_like_score(&query_tokens, &doc_tokens);

            if score > 0.0 {
                scored.push((
                    score,
                    cid.clone(),
                    chunk.document_id.clone(),
                    chunk.text.chars().take(200).collect(),
                ));
            }
        }

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        Ok(scored
            .into_iter()
            .take(top_k)
            .map(|(score, chunk_id, document_id, text_snippet)| FullTextHit {
                chunk_id,
                document_id,
                score,
                text_snippet,
            })
            .collect())
    }

    async fn index(&self, entries: &[FullTextEntry]) -> Result<(), KnowledgeError> {
        let mut ti = self.text_index.write().await;
        for e in entries {
            for word in tokenize(&e.text) {
                ti.entry(word)
                    .or_default()
                    .push((e.id.clone(), e.document_id.clone()));
            }
        }
        Ok(())
    }

    async fn remove(&self, ids: &[String]) -> Result<(), KnowledgeError> {
        let mut ti = self.text_index.write().await;
        let id_set: std::collections::HashSet<&String> = ids.iter().collect();
        for (_word, entries) in ti.iter_mut() {
            entries.retain(|(cid, _)| !id_set.contains(cid));
        }
        ti.retain(|_, v| !v.is_empty());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_doc() -> Document {
        Document {
            kb_id: None,
            id: "doc-1".into(),
            title: "Test Doc".into(),
            source_path: "/tmp/test.md".into(),
            content: "Hello world. This is a test document.".into(),
            metadata: DocumentMetadata {
                file_type: Some("md".into()),
                ..Default::default()
            },
        }
    }

    fn make_test_chunks() -> Vec<Chunk> {
        vec![
            Chunk {
                id: "doc-1-0000-a1b2c3d4".into(),
                document_id: "doc-1".into(),
                text: "Hello world.".into(),
                sequence_index: 0,
                page_number: None,
                embedding: vec![0.1, 0.2, 0.3],
                metadata: ChunkMetadata::default(),
            },
            Chunk {
                id: "doc-1-0001-e5f6g7h8".into(),
                document_id: "doc-1".into(),
                text: "This is a test document.".into(),
                sequence_index: 1,
                page_number: None,
                embedding: vec![0.4, 0.5, 0.6],
                metadata: ChunkMetadata::default(),
            },
        ]
    }

    #[tokio::test]
    async fn document_store_crud() {
        let be = InMemoryBackend::new();
        be.store(make_test_doc(), make_test_chunks()).await.unwrap();

        let doc = be.get(&"doc-1".into()).await.unwrap();
        assert!(doc.is_some());
        assert_eq!(doc.unwrap().title, "Test Doc");

        let stats = be.stats().await.unwrap();
        assert_eq!(stats.document_count, 1);
        assert_eq!(stats.chunk_count, 2);

        be.delete(&"doc-1".into()).await.unwrap();
        let stats = be.stats().await.unwrap();
        assert_eq!(stats.document_count, 0);
        assert_eq!(stats.chunk_count, 0);
    }

    #[tokio::test]
    async fn list_documents() {
        let be = InMemoryBackend::new();
        be.store(make_test_doc(), make_test_chunks()).await.unwrap();
        let list = be.list(0, 10).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].chunk_count, 2);
    }

    #[tokio::test]
    async fn chunks_by_document() {
        let be = InMemoryBackend::new();
        be.store(make_test_doc(), make_test_chunks()).await.unwrap();
        let chunks = be.chunks(&"doc-1".into()).await.unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].sequence_index, 0);
        assert_eq!(chunks[1].sequence_index, 1);
    }

    #[tokio::test]
    async fn vector_index_upsert_and_search() {
        let be = InMemoryBackend::new();
        be.store(make_test_doc(), make_test_chunks()).await.unwrap();

        be.upsert(&[
            VectorEntry {
                id: "doc-1-0000-a1b2c3d4".into(),
                document_id: "doc-1".into(),
                vector: vec![0.1, 0.2, 0.3],
                text: "Hello world.".into(),
            },
            VectorEntry {
                id: "doc-1-0001-e5f6g7h8".into(),
                document_id: "doc-1".into(),
                vector: vec![0.4, 0.5, 0.6],
                text: "This is a test document.".into(),
            },
        ])
        .await
        .unwrap();

        let results = VectorIndex::search(&be, &[0.1, 0.2, 0.3], 5, None)
            .await
            .unwrap();
        assert!(!results.is_empty());
        // 第一个结果应为精确匹配。
        assert_eq!(results[0].chunk_id, "doc-1-0000-a1b2c3d4");
    }

    #[tokio::test]
    async fn vector_index_ndims() {
        let be = InMemoryBackend::new();
        assert_eq!(be.ndims(), 0);
    }

    #[tokio::test]
    async fn fulltext_search() {
        let be = InMemoryBackend::new();
        be.store(make_test_doc(), make_test_chunks()).await.unwrap();
        be.index(&[
            FullTextEntry {
                id: "doc-1-0000-a1b2c3d4".into(),
                document_id: "doc-1".into(),
                text: "Hello world.".into(),
            },
            FullTextEntry {
                id: "doc-1-0001-e5f6g7h8".into(),
                document_id: "doc-1".into(),
                text: "This is a test document.".into(),
            },
        ])
        .await
        .unwrap();

        let results = FullTextIndex::search(&be, "hello", 5, None).await.unwrap();
        assert!(!results.is_empty());

        let results = FullTextIndex::search(&be, "nonexistent", 5, None)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn graph_traverse() {
        let be = InMemoryBackend::new();
        be.store(make_test_doc(), make_test_chunks()).await.unwrap();

        // 创建 CONTAINS 边：doc → chunk0, doc → chunk1
        be.add_edges(&[
            KnowledgeEdge {
                source_id: "doc-1".into(),
                target_id: "doc-1-0000-a1b2c3d4".into(),
                edge_type: EdgeType::Contains,
                weight: 1.0,
                properties: HashMap::new(),
            },
            KnowledgeEdge {
                source_id: "doc-1".into(),
                target_id: "doc-1-0001-e5f6g7h8".into(),
                edge_type: EdgeType::Contains,
                weight: 1.0,
                properties: HashMap::new(),
            },
        ])
        .await
        .unwrap();

        let steps = be
            .traverse(
                "doc-1",
                &[EdgeType::Contains],
                TraversalDirection::Outgoing,
                1,
            )
            .await
            .unwrap();

        // 应包含起始节点 + 2 个分块节点。
        assert_eq!(steps.len(), 3);
        let chunks: Vec<_> = steps.iter().filter(|s| s.via_edge.is_some()).collect();
        assert_eq!(chunks.len(), 2);
    }

    #[tokio::test]
    async fn graph_expand_from_chunks() {
        let be = InMemoryBackend::new();
        be.store(make_test_doc(), make_test_chunks()).await.unwrap();

        be.add_edge(KnowledgeEdge {
            source_id: "doc-1".into(),
            target_id: "doc-1-0000-a1b2c3d4".into(),
            edge_type: EdgeType::Contains,
            weight: 1.0,
            properties: HashMap::new(),
        })
        .await
        .unwrap();

        let nodes = be
            .expand(&["doc-1-0000-a1b2c3d4".into()], &[EdgeType::Contains], 1)
            .await
            .unwrap();

        assert!(!nodes.is_empty());
        // 应至少包含文档节点。
        let doc_node = nodes.iter().find(|n| n.id == "doc-1");
        assert!(doc_node.is_some());
    }

    // ── CJK tokenization tests ──

    #[test]
    fn tokenize_pure_chinese() {
        let tokens = tokenize("武汉大学");
        assert_eq!(tokens, vec!["武", "汉", "大", "学"]);
    }

    #[test]
    fn tokenize_pure_ascii() {
        let tokens = tokenize("Hello World");
        assert_eq!(tokens, vec!["hello", "world"]);
    }

    #[test]
    fn tokenize_mixed_chinese_ascii() {
        let tokens = tokenize("Rust 编程语言");
        assert_eq!(tokens, vec!["rust", "编", "程", "语", "言"]);
    }

    #[test]
    fn tokenize_chinese_with_punctuation() {
        let tokens = tokenize("姓名：彭琛");
        assert_eq!(tokens, vec!["姓", "名", "彭", "琛"]);
    }

    #[test]
    fn tokenize_empty() {
        let tokens = tokenize("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn bm25_chinese_match() {
        let query_tokens = tokenize("武汉大学");
        let doc_tokens = tokenize("毕业于武汉大学计算机系");
        let score = bm25_like_score(&query_tokens, &doc_tokens);
        // "武","汉","大","学" 都应该在文档中出现
        assert!(score > 0.5);
    }

    #[test]
    fn bm25_chinese_no_match() {
        let query_tokens = tokenize("苹果");
        let doc_tokens = tokenize("毕业于武汉大学计算机系");
        let score = bm25_like_score(&query_tokens, &doc_tokens);
        assert_eq!(score, 0.0);
    }

    #[tokio::test]
    async fn fulltext_search_chinese() {
        let be = InMemoryBackend::new();
        let doc = Document {
            id: "cn-1".into(),
            kb_id: None,
            title: "中文测试".into(),
            source_path: "/tmp/cn.md".into(),
            content: "武汉大学计算机科学与技术".into(),
            metadata: DocumentMetadata::default(),
        };
        let chunk = Chunk {
            id: "cn-1-0000-aaaaaaaa".into(),
            document_id: "cn-1".into(),
            text: "武汉大学计算机科学与技术".into(),
            sequence_index: 0,
            page_number: None,
            embedding: vec![],
            metadata: ChunkMetadata::default(),
        };
        be.store(doc, vec![chunk.clone()]).await.unwrap();
        be.index(&[FullTextEntry {
            id: chunk.id.clone(),
            document_id: chunk.document_id.clone(),
            text: chunk.text.clone(),
        }])
        .await
        .unwrap();

        // 中文查询
        let results = FullTextIndex::search(&be, "武汉大学", 5, None)
            .await
            .unwrap();
        assert!(!results.is_empty(), "中文 BM25 查询应返回结果");

        // 不相关的查询
        let results = FullTextIndex::search(&be, "苹果", 5, None).await.unwrap();
        assert!(results.is_empty(), "不相关的中文查询应返回空");
    }
}
