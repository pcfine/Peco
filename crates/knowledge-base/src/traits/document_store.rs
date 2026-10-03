use async_trait::async_trait;

use crate::error::KnowledgeError;
use crate::types::*;

/// 文档存储抽象 — 负责文档和分块的 CRUD 操作。
///
/// 幂等性：基于确定性 ID，重新摄入相同内容**不得**创建重复项。
#[async_trait]
pub trait DocumentStore: Send + Sync {
    /// 存储文档及其分块。
    ///
    /// 实现应透明地处理插入和更新（通过删除 + 插入）。
    async fn store(&self, doc: Document, chunks: Vec<Chunk>) -> Result<(), KnowledgeError>;

    /// 通过 ID 检索文档（不含分块文本）。
    async fn get(&self, id: &DocumentId) -> Result<Option<Document>, KnowledgeError>;

    /// 删除文档及其所有关联的分块和边。
    ///
    /// 没有 `update` 方法 — 内容变更表示为 `delete(id)` + `store(new_doc, new_chunks)`。
    async fn delete(&self, id: &DocumentId) -> Result<(), KnowledgeError>;

    /// 列出文档（分页）。
    async fn list(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<DocumentSummary>, KnowledgeError>;

    /// 分页列出文档，可选按 `source_path` **精确**过滤（E2）。
    ///
    /// 默认实现是**可移植**的「先过滤、后分页」兜底：以 `RAW_PAGE` 为步长循环
    /// [`Self::list`]，逐条比对 `source_path == want`（**精确**，非前缀），再
    /// `skip(offset)` / `take(limit)` —— 语义与 HelixDB 下推逐位一致
    /// （`offset` 为过滤后的偏移）。HelixDB 覆写为下推查询。
    ///
    /// **终止条件（三者任一即停，防死循环）**：
    /// ① 取回批次为空；② 批次长度 < `RAW_PAGE`（数据耗尽）；③ 已攒满 `limit`。
    async fn list_by_source(
        &self,
        offset: usize,
        limit: usize,
        source: Option<&str>,
    ) -> Result<Vec<DocumentSummary>, KnowledgeError> {
        let Some(want) = source else {
            return self.list(offset, limit).await;
        };
        if limit == 0 {
            return Ok(Vec::new());
        }

        // 兜底分页读取的原始页大小。
        const RAW_PAGE: usize = 200;

        let mut raw_offset = 0usize;
        let mut skipped = 0usize;
        let mut out: Vec<DocumentSummary> = Vec::new();
        loop {
            let batch = self.list(raw_offset, RAW_PAGE).await?;
            let exhausted = batch.len() < RAW_PAGE;
            let batch_len = batch.len();
            for item in batch {
                if item.source_path != want {
                    continue;
                }
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                out.push(item);
                if out.len() >= limit {
                    return Ok(out);
                }
            }
            if exhausted || batch_len == 0 {
                return Ok(out);
            }
            raw_offset += batch_len;
        }
    }

    /// 取回至多 `limit` 条文档（**含正文 + metadata**），供 E4 全量扫描（M9）。
    ///
    /// 默认实现是**可移植**兜底（仅用于非 HelixDB 后端，允许 N+1）：以 `RAW_PAGE`
    /// 为步长循环 [`Self::list`] 取摘要，再逐条 [`Self::get`] 取全文（`None` 跳过），
    /// 累积到 `limit` 条。HelixDB 覆写为单次 `Limit + Project(content, metadata)`。
    ///
    /// **终止条件（任一即停）**：① 取回批次为空；② 批次长度 < `RAW_PAGE`；
    /// ③ 已累积达 `limit`。
    async fn list_all(&self, limit: usize) -> Result<Vec<Document>, KnowledgeError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        // 兜底分页读取的原始页大小。
        const RAW_PAGE: usize = 200;

        let mut raw_offset = 0usize;
        let mut out: Vec<Document> = Vec::new();
        loop {
            if out.len() >= limit {
                return Ok(out);
            }
            let batch = self.list(raw_offset, RAW_PAGE).await?;
            let exhausted = batch.len() < RAW_PAGE;
            let batch_len = batch.len();
            if batch_len == 0 {
                return Ok(out);
            }
            for summary in batch {
                if out.len() >= limit {
                    return Ok(out);
                }
                if let Some(doc) = self.get(&summary.id).await? {
                    out.push(doc);
                }
            }
            if exhausted {
                return Ok(out);
            }
            raw_offset += batch_len;
        }
    }

    /// 获取文档的分块，按 `sequence_index` 排序。
    async fn chunks(&self, doc_id: &DocumentId) -> Result<Vec<Chunk>, KnowledgeError>;

    /// 存储的聚合统计信息。
    async fn stats(&self) -> Result<StoreStats, KnowledgeError>;
}
