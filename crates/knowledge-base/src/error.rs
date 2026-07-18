/// 知识模块统一错误类型。
///
/// 覆盖 trait 层、引擎层和后端实现中的所有错误类别。
#[derive(Debug, thiserror::Error)]
pub enum KnowledgeError {
    /// 文档存储操作失败。
    #[error("文档存储错误: {0}")]
    StoreError(String),

    /// 向量索引操作失败。
    #[error("向量索引错误: {0}")]
    VectorError(String),

    /// 图存储操作失败。
    #[error("图存储错误: {0}")]
    GraphError(String),

    /// 全文索引操作失败。
    #[error("全文搜索错误: {0}")]
    TextSearchError(String),

    /// 嵌入向量生成失败。
    #[error("嵌入错误: {0}")]
    EmbeddingError(String),

    /// 文本分块失败。
    #[error("分块错误: {0}")]
    ChunkingError(String),

    /// 所有搜索路径均失败 — 未能检索到任何结果。
    #[error("搜索失败: 所有检索策略均返回错误")]
    SearchFailed,

    /// 请求的文档或实体未找到。
    #[error("未找到: {0}")]
    NotFound(String),

    /// 无效输入或配置。
    #[error("无效输入: {0}")]
    InvalidInput(String),

    /// 内部错误（意外状态）。
    #[error("内部错误: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        let err = KnowledgeError::StoreError("磁盘已满".into());
        assert_eq!(format!("{err}"), "文档存储错误: 磁盘已满");

        let err = KnowledgeError::NotFound("doc-123".into());
        assert_eq!(format!("{err}"), "未找到: doc-123");

        let err = KnowledgeError::SearchFailed;
        assert_eq!(format!("{err}"), "搜索失败: 所有检索策略均返回错误");
    }

    #[test]
    fn error_debug() {
        let err = KnowledgeError::InvalidInput("缺少标题".into());
        let debug = format!("{err:?}");
        assert!(debug.contains("InvalidInput"));
        assert!(debug.contains("缺少标题"));
    }
}
