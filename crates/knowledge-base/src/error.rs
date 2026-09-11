/// 知识模块统一错误类型。
///
/// 覆盖 trait 层、引擎层和后端实现中的所有错误类别。
#[derive(Debug, thiserror::Error)]
pub enum KnowledgeError {
    /// 文档存储操作失败。
    #[error("Document store error: {0}")]
    StoreError(String),

    /// 向量索引操作失败。
    #[error("Vector index error: {0}")]
    VectorError(String),

    /// 图存储操作失败。
    #[error("Graph store error: {0}")]
    GraphError(String),

    /// 全文索引操作失败。
    #[error("Full-text search error: {0}")]
    TextSearchError(String),

    /// 嵌入向量生成失败。
    #[error("Embedding error: {0}")]
    EmbeddingError(String),

    /// 文本分块失败。
    #[error("Chunking error: {0}")]
    ChunkingError(String),

    /// 所有搜索路径均失败 — 未能检索到任何结果。
    #[error("Search failed: all retrieval strategies returned errors")]
    SearchFailed,

    /// 请求的文档或实体未找到。
    #[error("Not found: {0}")]
    NotFound(String),

    /// 无效输入或配置。
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// 内部错误（意外状态）。
    #[error("Internal error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        let err = KnowledgeError::StoreError("磁盘已满".into());
        assert_eq!(format!("{err}"), "Document store error: 磁盘已满");

        let err = KnowledgeError::NotFound("doc-123".into());
        assert_eq!(format!("{err}"), "Not found: doc-123");

        let err = KnowledgeError::SearchFailed;
        assert_eq!(
            format!("{err}"),
            "Search failed: all retrieval strategies returned errors"
        );
    }

    #[test]
    fn error_debug() {
        let err = KnowledgeError::InvalidInput("缺少标题".into());
        let debug = format!("{err:?}");
        assert!(debug.contains("InvalidInput"));
        assert!(debug.contains("缺少标题"));
    }
}
