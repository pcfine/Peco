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

    /// 获取文档的分块，按 `sequence_index` 排序。
    async fn chunks(&self, doc_id: &DocumentId) -> Result<Vec<Chunk>, KnowledgeError>;

    /// 存储的聚合统计信息。
    async fn stats(&self) -> Result<StoreStats, KnowledgeError>;
}
