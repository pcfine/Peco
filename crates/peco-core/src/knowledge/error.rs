//! 知识库模块专属错误类型。

use knowledge_base::KnowledgeError;

/// 知识库模块错误类型。
///
/// 封装底层 `knowledge-base` 错误以及本模块特有的 I/O、JSON 和业务逻辑错误。
#[derive(Debug, thiserror::Error)]
pub enum KnowledgeModuleError {
    /// 底层 knowledge-base 错误
    #[error("知识库错误: {0}")]
    Knowledge(#[from] KnowledgeError),

    /// I/O 错误
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    /// JSON 序列化/反序列化错误
    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),

    /// 知识库不存在
    #[error("知识库 '{0}' 不存在")]
    NotFound(String),

    /// 知识库已存在
    #[error("知识库 '{0}' 已存在")]
    AlreadyExists(String),

    /// 无效配置
    #[error("无效配置: {0}")]
    InvalidConfig(String),

    /// 尚未初始化 — 请先调用 ensure_loaded()
    #[error("知识库管理器尚未初始化，请先调用 ensure_loaded()")]
    NotInitialized,
}
