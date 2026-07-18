use std::sync::Arc;

use crate::error::KnowledgeError;
use crate::graph::builder::KnowledgeGraphBuilder;
use crate::traits::*;
use crate::types::*;
use tracing::info;

// ---------------------------------------------------------------------------
// IngestionPipeline
// ---------------------------------------------------------------------------

/// 文档摄入管道：解析 → 分块 → 嵌入 → 存储。
///
/// 在摄入过程中还会自动构建第一阶段的结构化知识图谱边
///（CONTAINS + NEXT_CHUNK）。
pub struct IngestionPipeline {
    doc_store: Arc<dyn DocumentStore>,
    vector_index: Option<Arc<dyn VectorIndex>>,
    graph_store: Option<Arc<dyn GraphStore>>,
    fulltext_index: Option<Arc<dyn FullTextIndex>>,
    embedding: Arc<dyn EmbeddingEngine>,
    chunker: Box<dyn Chunker>,
}

impl IngestionPipeline {
    pub fn new(
        doc_store: Arc<dyn DocumentStore>,
        vector_index: Option<Arc<dyn VectorIndex>>,
        graph_store: Option<Arc<dyn GraphStore>>,
        fulltext_index: Option<Arc<dyn FullTextIndex>>,
        embedding: Arc<dyn EmbeddingEngine>,
        chunker: Box<dyn Chunker>,
    ) -> Self {
        Self {
            doc_store,
            vector_index,
            graph_store,
            fulltext_index,
            embedding,
            chunker,
        }
    }

    /// 摄入单个文档。
    ///
    /// 步骤：
    /// 1. 对文档文本进行分块。
    /// 2. 批量嵌入所有分块。
    /// 3. 存储文档 + 分块。
    /// 4. 更新插入向量。
    /// 5. 索引文本。
    /// 6. 构建结构化图边（CONTAINS + NEXT_CHUNK）。
    pub async fn ingest(&self, doc: Document) -> Result<(), KnowledgeError> {
        let doc_id = doc.id.clone();
        let chunker_name = self.chunker.strategy_name();
        info!(
            doc_id = %doc_id,
            title = %doc.title,
            chunker = chunker_name,
            "正在摄入文档"
        );

        // 1. 分块
        let mut chunks = self.chunker.chunk(&doc);
        if chunks.is_empty() {
            // 即使分块没有产生任何结果，仍然存储文档。
            self.doc_store
                .store(doc, vec![])
                .await
                .map_err(|e| KnowledgeError::StoreError(e.to_string()))?;
            return Ok(());
        }

        // 2. 嵌入
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        let embeddings = self
            .embedding
            .embed_batch(&texts)
            .await
            .map_err(|e| KnowledgeError::EmbeddingError(e.to_string()))?;

        for (chunk, embedding) in chunks.iter_mut().zip(embeddings.into_iter()) {
            chunk.embedding = embedding;
        }

        // 3. 存储文档 + 分块
        self.doc_store
            .store(doc.clone(), chunks.clone())
            .await
            .map_err(|e| KnowledgeError::StoreError(e.to_string()))?;

        // 4. 更新插入向量
        if let Some(ref vi) = self.vector_index {
            let entries: Vec<VectorEntry> = chunks
                .iter()
                .map(|c| VectorEntry {
                    id: c.id.clone(),
                    document_id: doc_id.clone(),
                    vector: c.embedding.clone(),
                    text: c.text.clone(),
                })
                .collect();
            vi.upsert(&entries)
                .await
                .map_err(|e| KnowledgeError::VectorError(e.to_string()))?;
        }

        // 5. 索引文本（包括文档标题以支持标题关键词搜索）
        if let Some(ref ft) = self.fulltext_index {
            let mut entries: Vec<FullTextEntry> = chunks
                .iter()
                .map(|c| FullTextEntry {
                    id: c.id.clone(),
                    document_id: doc_id.clone(),
                    text: c.text.clone(),
                })
                .collect();
            // 将文档标题作为额外可搜索条目添加，使标题中的
            // 关键词（例如「简历」）即使不出现在分块内容中也能被匹配。
            entries.push(FullTextEntry {
                id: format!("{doc_id}__title"),
                document_id: doc_id.clone(),
                text: doc.title.clone(),
            });
            ft.index(&entries)
                .await
                .map_err(|e| KnowledgeError::TextSearchError(e.to_string()))?;
        }

        // 6. 构建结构化图（第一阶段）
        if let Some(ref gs) = self.graph_store {
            let builder = KnowledgeGraphBuilder::new();
            let edges = builder.build_structural_edges(&doc, &chunks);
            if !edges.is_empty() {
                gs.add_edges(&edges)
                    .await
                    .map_err(|e| KnowledgeError::GraphError(e.to_string()))?;
            }
        }

        info!(
            doc_id = %doc_id,
            chunk_count = chunks.len(),
            "文档摄入完成"
        );

        Ok(())
    }

    /// 批量摄入多个文档。
    ///
    /// 每个文档独立处理；其中一个失败不会中止整个批次。
    pub async fn ingest_batch(&self, docs: Vec<Document>) -> Vec<Result<(), KnowledgeError>> {
        let mut results = Vec::with_capacity(docs.len());
        for doc in docs {
            results.push(self.ingest(doc).await);
        }
        results
    }

    /// 删除文档及其在所有索引中的关联数据。
    ///
    /// 步骤：
    /// 1. 获取文档的所有分块 ID
    /// 2. 从向量索引中移除
    /// 3. 从全文索引中移除
    /// 4. 从图谱中移除边（如有）
    /// 5. 从文档存储中删除（LanceDB 会级联删除分块行）
    ///
    /// 各后端自行保证内部一致性；部分步骤可能冗余但确保跨后端的正确性。
    pub async fn delete_document(&self, doc_id: &DocumentId) -> Result<(), KnowledgeError> {
        // 1. 收集与此文档关联的分块 ID
        let chunks = self
            .doc_store
            .chunks(doc_id)
            .await
            .map_err(|e| KnowledgeError::StoreError(e.to_string()))?;
        let chunk_ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();

        // 2. 按分块 ID 移除向量条目
        if let Some(ref vi) = self.vector_index {
            if !chunk_ids.is_empty() {
                vi.remove(&chunk_ids)
                    .await
                    .map_err(|e| KnowledgeError::VectorError(e.to_string()))?;
            }
        }

        // 3. 按分块 ID 移除全文条目
        if let Some(ref ft) = self.fulltext_index {
            if !chunk_ids.is_empty() {
                ft.remove(&chunk_ids)
                    .await
                    .map_err(|e| KnowledgeError::TextSearchError(e.to_string()))?;
            }
        }

        // 4. 移除此文档节点的图谱边
        if let Some(ref gs) = self.graph_store {
            gs.remove_node_edges(doc_id)
                .await
                .map_err(|e| KnowledgeError::GraphError(e.to_string()))?;
        }

        // 5. 从 doc_store 删除文档及其分块
        self.doc_store
            .delete(doc_id)
            .await
            .map_err(|e| KnowledgeError::StoreError(e.to_string()))?;

        info!(
            doc_id = %doc_id,
            chunk_count = chunk_ids.len(),
            "文档已删除"
        );

        Ok(())
    }

    /// 返回存储的聚合统计信息。
    pub async fn stats(&self) -> Result<StoreStats, KnowledgeError> {
        self.doc_store.stats().await
    }

    /// 列出文档摘要（分页）。
    pub async fn list_documents(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<DocumentSummary>, KnowledgeError> {
        self.doc_store.list(offset, limit).await
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::memory::InMemoryBackend;
    use crate::chunking::make_chunker;
    use crate::traits::ChunkingStrategy;

    struct MockEmbedding {
        ndims: usize,
    }

    #[async_trait::async_trait]
    impl EmbeddingEngine for MockEmbedding {
        fn ndims(&self) -> usize {
            self.ndims
        }

        async fn embed_query(&self, _text: &str) -> Result<Vec<f32>, KnowledgeError> {
            Ok(vec![0.1; self.ndims])
        }

        async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, KnowledgeError> {
            Ok(texts.iter().map(|_| vec![0.1; self.ndims]).collect())
        }
    }

    fn test_doc() -> Document {
        Document {
            kb_id: None,
            id: "test-ingest-1".into(),
            title: "Test Ingestion".into(),
            source_path: "/tmp/test.md".into(),
            content: "Rust is a systems programming language. It provides memory safety. It is fast and concurrent.".into(),
            metadata: DocumentMetadata {
                file_type: Some("md".into()),
                ..Default::default()
            },
        }
    }

    #[tokio::test]
    async fn ingest_document() {
        let backend = Arc::new(InMemoryBackend::new());
        let chunker = make_chunker(ChunkingStrategy::OverlappingWindow {
            size: 50,
            overlap: 10,
        });
        let embedding = Arc::new(MockEmbedding { ndims: 384 });

        let pipeline = IngestionPipeline::new(
            backend.clone() as Arc<dyn DocumentStore>,
            Some(backend.clone() as Arc<dyn VectorIndex>),
            Some(backend.clone() as Arc<dyn GraphStore>),
            Some(backend.clone() as Arc<dyn FullTextIndex>),
            embedding,
            chunker,
        );

        pipeline.ingest(test_doc()).await.unwrap();

        let stats = backend.stats().await.unwrap();
        assert!(stats.document_count >= 1);
        assert!(stats.chunk_count >= 1);

        let doc = backend.get(&"test-ingest-1".into()).await.unwrap();
        assert!(doc.is_some());
    }

    #[tokio::test]
    async fn ingest_idempotent() {
        let backend = Arc::new(InMemoryBackend::new());
        let chunker = make_chunker(ChunkingStrategy::FixedSize { size: 100 });
        let embedding = Arc::new(MockEmbedding { ndims: 384 });

        let pipeline = IngestionPipeline::new(
            backend.clone() as Arc<dyn DocumentStore>,
            Some(backend.clone() as Arc<dyn VectorIndex>),
            None,
            None,
            embedding,
            chunker,
        );

        let doc = test_doc();
        pipeline.ingest(doc.clone()).await.unwrap();
        let stats1 = backend.stats().await.unwrap();

        // 重新摄入 — 分块具有确定性 ID，因此更新插入不应增加计数
        //（尽管 InMemory 后端的 store() 会替换）。
        pipeline.ingest(doc).await.unwrap();
        let stats2 = backend.stats().await.unwrap();

        assert_eq!(stats1.document_count, stats2.document_count);
        assert_eq!(stats1.chunk_count, stats2.chunk_count);
    }

    #[tokio::test]
    async fn ingest_batch() {
        let backend = Arc::new(InMemoryBackend::new());
        let chunker = make_chunker(ChunkingStrategy::default());
        let embedding = Arc::new(MockEmbedding { ndims: 384 });

        let pipeline = IngestionPipeline::new(
            backend.clone() as Arc<dyn DocumentStore>,
            None,
            None,
            None,
            embedding,
            chunker,
        );

        let docs = vec![
            Document {
                kb_id: None,
                id: "batch-1".into(),
                title: "Batch 1".into(),
                source_path: "/tmp/1.md".into(),
                content: "Content one.".into(),
                metadata: DocumentMetadata::default(),
            },
            Document {
                kb_id: None,
                id: "batch-2".into(),
                title: "Batch 2".into(),
                source_path: "/tmp/2.md".into(),
                content: "Content two.".into(),
                metadata: DocumentMetadata::default(),
            },
        ];

        let results = pipeline.ingest_batch(docs).await;
        for r in &results {
            assert!(r.is_ok());
        }

        let stats = backend.stats().await.unwrap();
        assert_eq!(stats.document_count, 2);
    }
}
