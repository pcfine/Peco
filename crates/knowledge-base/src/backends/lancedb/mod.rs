//! LanceDB 后端 — 本地嵌入式向量数据库。
//!
//! 实现核心 trait：DocumentStore、VectorIndex、FullTextIndex。

mod schema;

use std::path::PathBuf;
use std::sync::Arc;

use arrow_array::{
    ArrayRef, FixedSizeListArray, Float32Array, RecordBatch, StringArray, UInt32Array,
};
use futures::TryStreamExt;
use lancedb::DistanceType;
use lancedb::index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery, QueryBase, QueryExecutionOptions};
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::error::KnowledgeError;
use crate::traits::*;
use crate::types::*;

use self::schema::{DOCUMENT_ID_COL, ID_COL, TEXT_COL, chunk_table_schema};

// ---------------------------------------------------------------------------
// LanceDbBackend
// ---------------------------------------------------------------------------

pub struct LanceDbBackend {
    table: lancedb::Table,
    ndims: usize,
    write_lock: Mutex<()>,
    data_dir: PathBuf,
    table_name: String,
}

impl LanceDbBackend {
    pub async fn connect(
        db_path: &std::path::Path,
        table_name: &str,
        ndims: usize,
    ) -> Result<Self, KnowledgeError> {
        let path_str = db_path.to_str().unwrap_or("/tmp/peco-kb");
        let db = lancedb::connect(path_str)
            .execute()
            .await
            .map_err(|e| KnowledgeError::Internal(format!("LanceDB 连接失败: {e}")))?;

        let table_name_safe = crate::sanitize_kb_name(table_name);

        let table = match db.open_table(&table_name_safe).execute().await {
            Ok(t) => {
                info!(%table_name_safe, "打开已有 LanceDB 表");
                t
            }
            Err(_) => {
                info!(%table_name_safe, ndims, "创建新的 LanceDB 表");
                let arrow_schema = chunk_table_schema(ndims);
                let empty_batch = RecordBatch::new_empty(Arc::new(arrow_schema));
                let t = db
                    .create_table(&table_name_safe, empty_batch)
                    .execute()
                    .await
                    .map_err(|e| KnowledgeError::Internal(format!("LanceDB 建表失败: {e}")))?;
                match t
                    .create_index(&[TEXT_COL], lancedb::index::Index::FTS(Default::default()))
                    .execute()
                    .await
                {
                    Ok(_) => info!(%table_name_safe, "FTS 索引创建成功"),
                    Err(e) => tracing::warn!(%table_name_safe, error = %e, "FTS 索引创建失败"),
                }
                t
            }
        };

        Ok(Self {
            table,
            ndims,
            write_lock: Mutex::new(()),
            data_dir: db_path.to_path_buf(),
            table_name: table_name_safe,
        })
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }
    pub fn table_name(&self) -> &str {
        &self.table_name
    }
}

// 辅助：收集 RecordBatch 流
async fn collect_batches(
    stream: impl futures::Stream<Item = Result<RecordBatch, lancedb::Error>>,
) -> Result<Vec<RecordBatch>, KnowledgeError> {
    stream
        .try_collect()
        .await
        .map_err(|e| KnowledgeError::Internal(format!("读取数据流失败: {e}")))
}

// ---------------------------------------------------------------------------
// DocumentStore
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl DocumentStore for LanceDbBackend {
    async fn store(&self, doc: Document, chunks: Vec<Chunk>) -> Result<(), KnowledgeError> {
        let _lock = self.write_lock.lock().await;

        if chunks.is_empty() {
            let batch = build_batch(&doc, &[], self.ndims)?;
            self.table
                .add(batch)
                .execute()
                .await
                .map_err(|e| KnowledgeError::StoreError(format!("写入失败: {e}")))?;
            return Ok(());
        }

        let batch = build_batch(&doc, &chunks, self.ndims)?;
        self.table
            .add(batch)
            .execute()
            .await
            .map_err(|e| KnowledgeError::StoreError(format!("写入失败: {e}")))?;

        debug!(doc_id = %doc.id, chunks = chunks.len(), "文档存储完成");
        Ok(())
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Document>, KnowledgeError> {
        let filter = format!("{} = '{}'", DOCUMENT_ID_COL, id.replace('\'', "''"));
        let stream = self
            .table
            .query()
            .only_if(&filter)
            .limit(1)
            .execute_with_options(QueryExecutionOptions::default())
            .await
            .map_err(|e| KnowledgeError::StoreError(format!("查询失败: {e}")))?;

        let batches = collect_batches(stream).await?;
        if batches.is_empty() || batches[0].num_rows() == 0 {
            return Ok(None);
        }

        let b = &batches[0];
        Ok(Some(Document {
            id: id.clone(),
            kb_id: None,
            title: str_col(b, "title", 0).unwrap_or_default().to_string(),
            source_path: str_col(b, "source_path", 0).unwrap_or_default().to_string(),
            content: str_col(b, "content", 0).unwrap_or_default().to_string(),
            metadata: DocumentMetadata {
                file_type: str_col(b, "file_type", 0).map(|s| s.to_string()),
                ..Default::default()
            },
        }))
    }

    async fn delete(&self, id: &DocumentId) -> Result<(), KnowledgeError> {
        let _lock = self.write_lock.lock().await;
        self.table
            .delete(&format!(
                "{} = '{}'",
                DOCUMENT_ID_COL,
                id.replace('\'', "''")
            ))
            .await
            .map_err(|e| KnowledgeError::StoreError(format!("删除失败: {e}")))?;
        Ok(())
    }

    async fn list(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<DocumentSummary>, KnowledgeError> {
        let stream = self
            .table
            .query()
            .limit(limit + offset)
            .execute_with_options(QueryExecutionOptions::default())
            .await
            .map_err(|e| KnowledgeError::StoreError(format!("列表查询失败: {e}")))?;

        let batches = collect_batches(stream).await?;
        let mut seen = std::collections::HashSet::new();
        let mut summaries = Vec::new();

        for b in &batches {
            for i in 0..b.num_rows() {
                let doc_id = str_col(b, DOCUMENT_ID_COL, i)
                    .unwrap_or_default()
                    .to_string();
                if seen.contains(&doc_id) {
                    continue;
                }
                seen.insert(doc_id.clone());
                summaries.push(DocumentSummary {
                    id: doc_id,
                    title: str_col(b, "title", i).unwrap_or_default().to_string(),
                    source_path: str_col(b, "source_path", i).unwrap_or_default().to_string(),
                    chunk_count: 0,
                    file_type: str_col(b, "file_type", i).map(|s| s.to_string()),
                });
            }
        }
        let start = offset.min(summaries.len());
        let end = (start + limit).min(summaries.len());
        Ok(summaries[start..end].to_vec())
    }

    async fn chunks(&self, doc_id: &DocumentId) -> Result<Vec<Chunk>, KnowledgeError> {
        let filter = format!("{} = '{}'", DOCUMENT_ID_COL, doc_id.replace('\'', "''"));
        let stream = self
            .table
            .query()
            .only_if(&filter)
            .execute_with_options(QueryExecutionOptions::default())
            .await
            .map_err(|e| KnowledgeError::StoreError(format!("分块查询失败: {e}")))?;

        let batches = collect_batches(stream).await?;
        let mut result = Vec::new();
        for b in &batches {
            for i in 0..b.num_rows() {
                result.push(Chunk {
                    id: str_col(b, ID_COL, i).unwrap_or_default().to_string(),
                    document_id: doc_id.clone(),
                    text: str_col(b, TEXT_COL, i).unwrap_or_default().to_string(),
                    sequence_index: u32_col(b, "sequence_index", i).unwrap_or(0),
                    page_number: u32_col(b, "page_number", i),
                    embedding: vec![],
                    metadata: ChunkMetadata::default(),
                });
            }
        }
        Ok(result)
    }

    async fn stats(&self) -> Result<StoreStats, KnowledgeError> {
        let stream = self
            .table
            .query()
            .execute_with_options(QueryExecutionOptions::default())
            .await
            .map_err(|e| KnowledgeError::StoreError(format!("统计查询失败: {e}")))?;

        let batches = collect_batches(stream).await?;
        let mut doc_ids = std::collections::HashSet::new();
        let mut chunk_count = 0usize;
        let mut total_bytes = 0u64;

        for b in &batches {
            chunk_count += b.num_rows();
            for i in 0..b.num_rows() {
                if let Some(d) = str_col(b, DOCUMENT_ID_COL, i) {
                    doc_ids.insert(d.to_string());
                }
                total_bytes += str_col(b, "text", i).map(|s| s.len() as u64).unwrap_or(0);
            }
        }

        Ok(StoreStats {
            document_count: doc_ids.len(),
            chunk_count,
            total_bytes,
        })
    }
}

// ---------------------------------------------------------------------------
// VectorIndex
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl VectorIndex for LanceDbBackend {
    fn ndims(&self) -> usize {
        self.ndims
    }

    async fn search(
        &self,
        query_vec: &[f32],
        top_k: usize,
        filters: Option<&SearchFilters>,
    ) -> Result<Vec<VectorHit>, KnowledgeError> {
        let qv: Vec<f64> = query_vec.iter().map(|&f| f as f64).collect();

        let mut vq = self
            .table
            .query()
            .nearest_to(qv)
            .map_err(|e| KnowledgeError::VectorError(format!("nearest_to 失败: {e}")))?
            .distance_type(DistanceType::Cosine)
            .limit(top_k);

        if let Some(f) = filters
            && let Some(ref ids) = f.document_ids
            && !ids.is_empty()
        {
            let list = ids
                .iter()
                .map(|d| format!("'{}'", d.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(",");
            vq = vq.only_if(format!("document_id IN ({})", list));
        }

        let stream = vq
            .execute()
            .await
            .map_err(|e| KnowledgeError::VectorError(format!("向量搜索失败: {e}")))?;
        let batches = collect_batches(stream).await?;

        let mut hits = Vec::new();
        for b in &batches {
            for i in 0..b.num_rows() {
                let distance = f32_col(b, "_distance", i).unwrap_or(1.0);
                hits.push(VectorHit {
                    chunk_id: str_col(b, ID_COL, i).unwrap_or_default().to_string(),
                    document_id: str_col(b, DOCUMENT_ID_COL, i)
                        .unwrap_or_default()
                        .to_string(),
                    score: 1.0 / (1.0 + distance),
                });
            }
        }
        Ok(hits)
    }

    async fn upsert(&self, _entries: &[VectorEntry]) -> Result<(), KnowledgeError> {
        Ok(())
    }
    async fn remove(&self, ids: &[String]) -> Result<(), KnowledgeError> {
        let _lock = self.write_lock.lock().await;
        for id in ids {
            let _ = self
                .table
                .delete(&format!("{} = '{}'", ID_COL, id.replace('\'', "''")))
                .await;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FullTextIndex
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl FullTextIndex for LanceDbBackend {
    async fn search(
        &self,
        query: &str,
        top_k: usize,
        filters: Option<&SearchFilters>,
    ) -> Result<Vec<FullTextHit>, KnowledgeError> {
        let mut q = self.table.query();
        q = q.full_text_search(FullTextSearchQuery::new(query.to_string()));
        q = q.limit(top_k);

        if let Some(f) = filters
            && let Some(ref ids) = f.document_ids
            && !ids.is_empty()
        {
            let list = ids
                .iter()
                .map(|d| format!("'{}'", d.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(",");
            q = q.only_if(format!("document_id IN ({})", list));
        }

        let stream = q
            .execute_with_options(QueryExecutionOptions::default())
            .await
            .map_err(|e| KnowledgeError::TextSearchError(format!("全文搜索失败: {e}")))?;
        let batches = collect_batches(stream).await?;

        let mut hits = Vec::new();
        for b in &batches {
            for i in 0..b.num_rows() {
                hits.push(FullTextHit {
                    chunk_id: str_col(b, ID_COL, i).unwrap_or_default().to_string(),
                    document_id: str_col(b, DOCUMENT_ID_COL, i)
                        .unwrap_or_default()
                        .to_string(),
                    score: f32_col(b, "_score", i).unwrap_or(0.0),
                    text_snippet: str_col(b, TEXT_COL, i).unwrap_or_default().to_string(),
                });
            }
        }
        Ok(hits)
    }

    async fn index(&self, _entries: &[FullTextEntry]) -> Result<(), KnowledgeError> {
        Ok(())
    }
    async fn remove(&self, ids: &[String]) -> Result<(), KnowledgeError> {
        let _lock = self.write_lock.lock().await;
        for id in ids {
            let _ = self
                .table
                .delete(&format!("{} = '{}'", ID_COL, id.replace('\'', "''")))
                .await;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

fn build_batch(
    doc: &Document,
    chunks: &[Chunk],
    ndims: usize,
) -> Result<RecordBatch, KnowledgeError> {
    let schema = chunk_table_schema(ndims);
    let ft = doc.metadata.file_type.clone().unwrap_or_default();

    let entries: Vec<_> = if chunks.is_empty() {
        vec![(
            doc.id.clone(),
            doc.id.clone(),
            doc.content.clone(),
            doc.title.clone(),
            doc.source_path.clone(),
            doc.content.clone(),
            doc.id.clone(),
            0u32,
            None as Option<u32>,
            ft.clone(),
            vec![0.0f32; ndims],
        )]
    } else {
        chunks
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    c.document_id.clone(),
                    c.text.clone(),
                    doc.title.clone(),
                    doc.source_path.clone(),
                    doc.content.clone(),
                    doc.id.clone(),
                    c.sequence_index,
                    c.page_number,
                    ft.clone(),
                    c.embedding.clone(),
                )
            })
            .collect()
    };

    let n = entries.len();
    let ids: Vec<String> = entries.iter().map(|e| e.0.clone()).collect();
    let doc_ids: Vec<String> = entries.iter().map(|e| e.1.clone()).collect();
    let texts: Vec<String> = entries.iter().map(|e| e.2.clone()).collect();
    let titles: Vec<String> = entries.iter().map(|e| e.3.clone()).collect();
    let sources: Vec<String> = entries.iter().map(|e| e.4.clone()).collect();
    let contents: Vec<String> = entries.iter().map(|e| e.5.clone()).collect();
    let parent_ids: Vec<String> = entries.iter().map(|e| e.6.clone()).collect();
    let seqs: Vec<u32> = entries.iter().map(|e| e.7).collect();
    let pages: Vec<u32> = entries.iter().map(|e| e.8.unwrap_or(0)).collect();
    let fts: Vec<String> = entries.iter().map(|e| e.9.clone()).collect();
    let mut embs = Vec::with_capacity(n * ndims);
    for e in &entries {
        let v = &e.10;
        if v.len() == ndims {
            embs.extend_from_slice(v);
        } else {
            embs.extend(vec![0.0f32; ndims]);
        }
    }

    let emb_arr: ArrayRef = Arc::new(
        FixedSizeListArray::try_new(
            Arc::new(arrow_schema::Field::new(
                "item",
                arrow_schema::DataType::Float32,
                true,
            )),
            ndims as i32,
            Arc::new(Float32Array::from(embs)),
            None,
        )
        .map_err(|e| KnowledgeError::Internal(format!("嵌入数组构建失败: {e}")))?,
    );

    RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(StringArray::from(ids)) as ArrayRef,
            Arc::new(StringArray::from(doc_ids)) as ArrayRef,
            Arc::new(StringArray::from(texts)) as ArrayRef,
            Arc::new(StringArray::from(titles)) as ArrayRef,
            Arc::new(StringArray::from(sources)) as ArrayRef,
            Arc::new(StringArray::from(contents)) as ArrayRef,
            Arc::new(StringArray::from(parent_ids)) as ArrayRef,
            Arc::new(UInt32Array::from(seqs)) as ArrayRef,
            Arc::new(UInt32Array::from(pages)) as ArrayRef,
            Arc::new(StringArray::from(fts)) as ArrayRef,
            emb_arr,
        ],
    )
    .map_err(|e| KnowledgeError::Internal(format!("RecordBatch 构建失败: {e}")))
}

fn str_col<'a>(batch: &'a RecordBatch, col: &str, row: usize) -> Option<&'a str> {
    batch
        .column_by_name(col)?
        .as_any()
        .downcast_ref::<StringArray>()?
        .value(row)
        .into()
}

fn f32_col(batch: &RecordBatch, col: &str, row: usize) -> Option<f32> {
    batch
        .column_by_name(col)?
        .as_any()
        .downcast_ref::<arrow_array::Float32Array>()?
        .value(row)
        .into()
}

fn u32_col(batch: &RecordBatch, col: &str, row: usize) -> Option<u32> {
    batch
        .column_by_name(col)?
        .as_any()
        .downcast_ref::<UInt32Array>()?
        .value(row)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize() {
        assert_eq!(crate::sanitize_kb_name("my-kb"), "my-kb");
        assert_eq!(crate::sanitize_kb_name(""), "kb_default");
        // 纯中文名称 — 使用 hex 摘要回退
        assert_eq!(
            crate::sanitize_kb_name("个人档案"),
            "kb_e4b8aae4babae6a1a3e6a188"
        );
        // 混合 — 去除非 ASCII 后保留有效部分
        assert_eq!(crate::sanitize_kb_name("简历_v1.0"), "v1.0");
        assert_eq!(crate::sanitize_kb_name("test.db"), "test.db");
        // 两个不同的中文名称不会冲突
        assert_ne!(
            crate::sanitize_kb_name("个人档案"),
            crate::sanitize_kb_name("我的文档")
        );
    }

    #[test]
    fn schema_is_valid() {
        let schema = chunk_table_schema(384);
        assert!(schema.column_with_name("id").is_some());
        assert!(schema.column_with_name("embedding").is_some());
    }
}
