use async_trait::async_trait;

use crate::error::KnowledgeError;
use crate::types::SearchFilters;

/// ANN 向量搜索的单条命中结果。
#[derive(Debug, Clone)]
pub struct VectorHit {
    pub chunk_id: String,
    pub document_id: String,
    /// 相似度分数 0.0–1.0，越高越相似。
    pub score: f32,
}

/// 待索引的向量条目。
#[derive(Debug, Clone)]
pub struct VectorEntry {
    /// 分块 ID。
    pub id: String,
    /// 父文档 ID。
    pub document_id: String,
    /// 嵌入向量。
    pub vector: Vec<f32>,
    /// 原始文本（用于结果渲染）。
    pub text: String,
}

/// 向量 ANN（近似最近邻）搜索抽象。
#[async_trait]
pub trait VectorIndex: Send + Sync {
    /// 此索引中存储的向量维度。
    ///
    /// 调用方应在构造时验证 `vector_index.ndims() == embedding_engine.ndims()`。
    fn ndims(&self) -> usize;

    /// 近似最近邻搜索。
    ///
    /// 返回按相似度降序排列的 `(chunk_id, score)` 对。
    async fn search(
        &self,
        query_vec: &[f32],
        top_k: usize,
        filters: Option<&SearchFilters>,
    ) -> Result<Vec<VectorHit>, KnowledgeError>;

    /// 批量更新插入向量（通常在摄入过程中调用）。
    async fn upsert(&self, entries: &[VectorEntry]) -> Result<(), KnowledgeError>;

    /// 按 ID 移除向量。
    async fn remove(&self, ids: &[String]) -> Result<(), KnowledgeError>;
}
