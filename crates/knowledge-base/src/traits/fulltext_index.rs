use async_trait::async_trait;

use crate::error::KnowledgeError;
use crate::types::SearchFilters;

/// 单条全文搜索命中结果。
#[derive(Debug, Clone)]
pub struct FullTextHit {
    pub chunk_id: String,
    pub document_id: String,
    pub score: f32,
    /// 匹配的上下文片段。
    pub text_snippet: String,
}

/// 待索引的文本条目。
#[derive(Debug, Clone)]
pub struct FullTextEntry {
    pub id: String,
    pub document_id: String,
    pub text: String,
}

/// 全文搜索抽象（BM25 或简单的关键词匹配）。
#[async_trait]
pub trait FullTextIndex: Send + Sync {
    /// 全文搜索，返回匹配的分块。
    async fn search(
        &self,
        query: &str,
        top_k: usize,
        filters: Option<&SearchFilters>,
    ) -> Result<Vec<FullTextHit>, KnowledgeError>;

    /// 索引文本条目（通常与 `DocumentStore::store` 一起调用）。
    async fn index(&self, entries: &[FullTextEntry]) -> Result<(), KnowledgeError>;

    /// 从索引中移除条目。
    async fn remove(&self, ids: &[String]) -> Result<(), KnowledgeError>;
}
