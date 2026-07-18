use async_trait::async_trait;

use crate::error::KnowledgeError;

/// 文本向量化抽象。
///
/// 此 trait 属于"引擎"层 — 它提供计算能力，而非持久化。
/// 将其与 `VectorIndex` 分离，使调用方能够独立于存储后端
/// 在本地模型和远程 API 之间进行选择。
#[async_trait]
pub trait EmbeddingEngine: Send + Sync {
    /// 此引擎生成的向量维度。
    fn ndims(&self) -> usize;

    /// 嵌入单个文本（用于查询）。
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, KnowledgeError>;

    /// 批量嵌入多个文本（用于摄入过程）。
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, KnowledgeError>;
}
