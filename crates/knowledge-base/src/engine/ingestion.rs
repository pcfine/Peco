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

    /// 摄入单个文档（默认 Full 模式）。
    ///
    /// 委托给 [`ingest_with_mode`] 以 `StorageMode::Full` 执行。
    pub async fn ingest(&self, doc: Document) -> Result<(), KnowledgeError> {
        self.ingest_with_mode(doc, StorageMode::Full).await
    }

    /// 按指定存储模式摄入文档。
    ///
    /// 不同 mode 跳过不需要的步骤，避免不必要的分块、嵌入或索引操作。
    pub async fn ingest_with_mode(
        &self,
        doc: Document,
        mode: StorageMode,
    ) -> Result<(), KnowledgeError> {
        let doc_id = doc.id.clone();
        let chunker_name = self.chunker.strategy_name();
        info!(
            doc_id = %doc_id,
            title = %doc.title,
            chunker = chunker_name,
            mode = ?mode,
            "Ingesting document"
        );

        // 同 id 重复摄入 = 替换语义：doc_id 由内容哈希派生，逐字重复的内容
        // 会命中同一 doc_id。先级联清掉既有行再写入，否则追加式后端
        // （LanceDB 的 store 为纯 append）会累积重复 chunk 行。
        if self.doc_store.get(&doc_id).await?.is_some() {
            // 并发删除的竞态下文档可能刚好消失 — 此时无需替换，继续摄入
            if let Err(e) = self.delete_document(&doc_id).await
                && !matches!(e, KnowledgeError::NotFound(_))
            {
                return Err(e);
            }
        }

        let need_chunks = mode_requires_chunks(&mode);
        let need_embed = mode_requires_embed(&mode);
        let need_vector = mode_requires_vector(&mode);
        let need_text = mode_requires_text(&mode);
        let need_graph = mode_requires_graph(&mode);

        // 1. 分块（仅在需要时）
        let mut chunks = if need_chunks {
            let c = self.chunker.chunk(&doc);
            if c.is_empty() {
                self.doc_store
                    .store(doc, vec![])
                    .await
                    .map_err(|e| KnowledgeError::StoreError(e.to_string()))?;
                return Ok(());
            }
            c
        } else {
            // 不需要分块时，创建一个代表全文的合成「分块」，使文档存储和图结构边仍能构建
            vec![Chunk {
                id: format!("{doc_id}__full"),
                document_id: doc_id.clone(),
                text: doc.content.clone(),
                sequence_index: 0,
                page_number: None,
                embedding: vec![],
                metadata: ChunkMetadata::default(),
            }]
        };

        // 2. 嵌入（仅在需要时）
        if need_embed && need_chunks {
            let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
            let embeddings = self
                .embedding
                .embed_batch(&texts)
                .await
                .map_err(|e| KnowledgeError::EmbeddingError(e.to_string()))?;

            for (chunk, embedding) in chunks.iter_mut().zip(embeddings.into_iter()) {
                chunk.embedding = embedding;
            }
        }

        // 3. 存储文档 + 分块
        self.doc_store
            .store(doc.clone(), chunks.clone())
            .await
            .map_err(|e| KnowledgeError::StoreError(e.to_string()))?;

        // 4. 更新插入向量
        if need_vector && let Some(ref vi) = self.vector_index {
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

        // 5. 索引文本
        if need_text && let Some(ref ft) = self.fulltext_index {
            let mut entries: Vec<FullTextEntry> = chunks
                .iter()
                .map(|c| FullTextEntry {
                    id: c.id.clone(),
                    document_id: doc_id.clone(),
                    text: c.text.clone(),
                })
                .collect();
            entries.push(FullTextEntry {
                id: format!("{doc_id}__title"),
                document_id: doc_id.clone(),
                text: doc.title.clone(),
            });
            ft.index(&entries)
                .await
                .map_err(|e| KnowledgeError::TextSearchError(e.to_string()))?;
        }

        // 6. 构建结构化图边（CONTAINS + NEXT_CHUNK）
        if need_graph && let Some(ref gs) = self.graph_store {
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
            mode = ?mode,
            "Document ingestion completed"
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
    /// 1. 确认文档存在（重复删除或未知文档直接报错，不误报为成功）
    /// 2. 获取文档的所有分块 ID
    /// 3. 从向量索引中移除
    /// 4. 从全文索引中移除
    /// 5. 从图谱中移除边（如有）
    /// 6. 从文档存储中删除（LanceDB 会级联删除分块行）
    ///
    /// 各后端自行保证内部一致性；部分步骤可能冗余但确保跨后端的正确性。
    pub async fn delete_document(
        &self,
        doc_id: &DocumentId,
    ) -> Result<DeleteReport, KnowledgeError> {
        // 1. 文档必须存在
        if self.doc_store.get(doc_id).await?.is_none() {
            return Err(KnowledgeError::NotFound(format!(
                "Document not found: '{doc_id}'"
            )));
        }

        // 2. 收集与此文档关联的分块 ID
        let chunks = self
            .doc_store
            .chunks(doc_id)
            .await
            .map_err(|e| KnowledgeError::StoreError(e.to_string()))?;
        let chunk_ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();

        // 3. 按分块 ID 移除向量条目
        if let Some(ref vi) = self.vector_index
            && !chunk_ids.is_empty()
        {
            vi.remove(&chunk_ids)
                .await
                .map_err(|e| KnowledgeError::VectorError(e.to_string()))?;
        }

        // 4. 按分块 ID 移除全文条目
        if let Some(ref ft) = self.fulltext_index
            && !chunk_ids.is_empty()
        {
            ft.remove(&chunk_ids)
                .await
                .map_err(|e| KnowledgeError::TextSearchError(e.to_string()))?;
        }

        // 5. 移除此文档节点的图谱边
        if let Some(ref gs) = self.graph_store {
            gs.remove_node_edges(doc_id)
                .await
                .map_err(|e| KnowledgeError::GraphError(e.to_string()))?;
        }

        // 6. 从 doc_store 删除文档及其分块
        self.doc_store
            .delete(doc_id)
            .await
            .map_err(|e| KnowledgeError::StoreError(e.to_string()))?;

        info!(
            doc_id = %doc_id,
            chunk_count = chunk_ids.len(),
            "Document deleted"
        );

        Ok(DeleteReport {
            doc_id: doc_id.clone(),
            removed_chunks: chunk_ids.len(),
        })
    }

    /// 读取单个文档（含原文内容）；文档不存在时返回 `None`。
    pub async fn get_document(
        &self,
        doc_id: &DocumentId,
    ) -> Result<Option<Document>, KnowledgeError> {
        self.doc_store.get(doc_id).await
    }

    /// 对任意文本批量生成向量（重嵌入路径）。
    ///
    /// 供余弦相似度比较等上层逻辑使用。这是文档级全文向量，
    /// 与库内 chunk 级（滑动窗口）粒度不同 — 记忆条目普遍短于窗口，实际等价。
    pub async fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, KnowledgeError> {
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        self.embedding.embed_batch(&refs).await
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

    /// LanceDB 是追加式存储（store 为纯 table.add），同 doc_id 重复摄入
    /// 必须由管道的替换语义兜底：先级联删除旧行再写入，chunk 行数不翻倍。
    /// InMemory 后端本身覆盖写入，测不出该回归，故用 LanceDB 实测。
    #[tokio::test]
    async fn reingest_same_doc_id_on_lancedb_replaces_instead_of_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let backend = Arc::new(
            crate::backends::lancedb::LanceDbBackend::connect(tmp.path(), "reingest", 4)
                .await
                .unwrap(),
        );
        let pipeline = IngestionPipeline::new(
            backend.clone() as Arc<dyn DocumentStore>,
            Some(backend.clone() as Arc<dyn VectorIndex>),
            None,
            Some(backend.clone() as Arc<dyn FullTextIndex>),
            Arc::new(MockEmbedding { ndims: 4 }),
            make_chunker(ChunkingStrategy::FixedSize { size: 50 }),
        );

        let doc = Document {
            content: "a".repeat(150),
            ..test_doc()
        };
        pipeline.ingest(doc.clone()).await.unwrap();
        let first = backend.chunks(&doc.id).await.unwrap().len();
        assert!(first >= 1);

        pipeline.ingest(doc).await.unwrap();
        let second = backend.chunks(&"test-ingest-1".into()).await.unwrap().len();
        assert_eq!(
            second, first,
            "重复摄入不得累积 chunk 行（替换语义，而非追加）"
        );
        assert!(
            pipeline
                .get_document(&"test-ingest-1".into())
                .await
                .unwrap()
                .is_some()
        );
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

    // ── delete_document ─────────────────────────────────────────────────

    /// 150 字符内容 + FixedSize { size: 50 } → 恰好 3 个分块。
    fn three_chunk_doc() -> Document {
        Document {
            kb_id: None,
            id: "delete-me-1".into(),
            title: "Delete Me".into(),
            source_path: "/tmp/delete-me.md".into(),
            content: "a".repeat(150),
            metadata: DocumentMetadata::default(),
        }
    }

    fn pipeline_with_fixed_chunks(backend: &Arc<InMemoryBackend>) -> IngestionPipeline {
        IngestionPipeline::new(
            backend.clone() as Arc<dyn DocumentStore>,
            Some(backend.clone() as Arc<dyn VectorIndex>),
            Some(backend.clone() as Arc<dyn GraphStore>),
            Some(backend.clone() as Arc<dyn FullTextIndex>),
            Arc::new(MockEmbedding { ndims: 384 }),
            make_chunker(ChunkingStrategy::FixedSize { size: 50 }),
        )
    }

    /// 删除报告给出真实移除的分块数，且各索引与文档存储同步清空（无幽灵行）。
    #[tokio::test]
    async fn delete_document_reports_removed_chunks() {
        let backend = Arc::new(InMemoryBackend::new());
        let pipeline = pipeline_with_fixed_chunks(&backend);

        let doc = three_chunk_doc();
        pipeline.ingest(doc.clone()).await.unwrap();
        assert_eq!(backend.chunks(&doc.id).await.unwrap().len(), 3);

        let report = pipeline.delete_document(&doc.id).await.unwrap();
        assert_eq!(report.doc_id, doc.id);
        assert_eq!(report.removed_chunks, 3);

        assert!(pipeline.get_document(&doc.id).await.unwrap().is_none());
        assert!(backend.chunks(&doc.id).await.unwrap().is_empty());
        let stats = backend.stats().await.unwrap();
        assert_eq!(stats.chunk_count, 0);
    }

    /// 重复删除与未知文档一律报 NotFound，不误报为成功。
    #[tokio::test]
    async fn delete_document_twice_and_unknown_report_not_found() {
        let backend = Arc::new(InMemoryBackend::new());
        let pipeline = pipeline_with_fixed_chunks(&backend);

        let doc = three_chunk_doc();
        pipeline.ingest(doc.clone()).await.unwrap();
        pipeline.delete_document(&doc.id).await.unwrap();

        let err = pipeline.delete_document(&doc.id).await.unwrap_err();
        assert!(
            matches!(err, KnowledgeError::NotFound(_)),
            "重复删除应报 NotFound，实际: {err}"
        );

        let err = pipeline
            .delete_document(&"no-such-doc".to_string())
            .await
            .unwrap_err();
        assert!(
            matches!(err, KnowledgeError::NotFound(_)),
            "未知文档应报 NotFound，实际: {err}"
        );
    }

    /// embed_texts 返回与嵌入引擎维度一致的向量，且空批返回空。
    #[tokio::test]
    async fn embed_texts_shapes() {
        let backend = Arc::new(InMemoryBackend::new());
        let pipeline = IngestionPipeline::new(
            backend.clone() as Arc<dyn DocumentStore>,
            None,
            None,
            None,
            Arc::new(MockEmbedding { ndims: 384 }),
            make_chunker(ChunkingStrategy::FixedSize { size: 50 }),
        );

        let texts = vec!["记忆一".to_string(), "记忆二".to_string()];
        let vectors = pipeline.embed_texts(&texts).await.unwrap();
        assert_eq!(vectors.len(), 2);
        assert!(vectors.iter().all(|v| v.len() == 384));

        assert!(pipeline.embed_texts(&[]).await.unwrap().is_empty());
    }

    /// V-2 检查点：FastEmbed 重嵌入确定性 — 同文本两次嵌入 bit 级一致。
    ///
    /// 依赖本地模型缓存（~/.fastembed_cache/）或网络下载；
    /// 环境不可用时跳过（与 mock 无关，此断言针对真实引擎）。
    #[tokio::test]
    async fn embed_texts_is_deterministic_on_fastembed() {
        let engine = match crate::embedding::FastembedEngine::new(
            crate::embedding::FastembedModelType::BGESmallZHV15,
        ) {
            Ok(e) => Arc::new(e),
            Err(_) => return,
        };
        let backend = Arc::new(InMemoryBackend::new());
        let pipeline = IngestionPipeline::new(
            backend as Arc<dyn DocumentStore>,
            None,
            None,
            None,
            engine,
            make_chunker(ChunkingStrategy::FixedSize { size: 50 }),
        );

        let texts = vec!["用户偏好深色主题".to_string(), "喜欢手冲咖啡".to_string()];
        let first = pipeline.embed_texts(&texts).await.unwrap();
        let second = pipeline.embed_texts(&texts).await.unwrap();
        assert_eq!(first, second, "同文本两次嵌入应 bit 级一致");
    }
}
