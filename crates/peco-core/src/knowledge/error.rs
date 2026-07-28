//! 知识库模块专属错误类型。
//!
//! # 错误传播链
//!
//! ```text
//! 用户调用 KnowledgeManager 方法
//!   → KnowledgeModuleError (本模块)
//!     ├─ Knowledge(KnowledgeError)   ← knowledge_base::KnowledgeBaseManager / KnowledgeBase
//!     │   ├─ StoreError        ← DocumentStore 后端
//!     │   ├─ VectorError       ← VectorIndex 后端
//!     │   ├─ EmbeddingError    ← FastembedEngine
//!     │   ├─ NotFound          ← KB/文档未找到
//!     │   └─ ...
//!     ├─ Io(std::io::Error)          ← 文件读写、目录扫描
//!     ├─ Json(serde_json::Error)     ← 配置/清单序列化
//!     ├─ NotFound(String)            ← peco-core 层面的业务语义
//!     ├─ AlreadyExists(String)
//!     ├─ InvalidConfig(String)
//!     └─ NotInitialized              ← 忘记调用 ensure_loaded()
//! ```
//!
//! 所有 `knowledge_base::KnowledgeError` 变体通过 `#[from]` 自动转换为
//! `KnowledgeModuleError::Knowledge`，调用方可使用 `?` 运算符在两种错误类型间传播。

use knowledge_base::KnowledgeError;

/// 知识库模块错误类型。
///
/// 三层错误结构：
/// 1. **底层引擎错误** — `Knowledge(KnowledgeError)`：来自 `knowledge-base` 的
///    存储后端、嵌入引擎、分块器、解析器等组件的错误。
/// 2. **系统 I/O 错误** — `Io` / `Json`：来自文件系统操作和序列化。
/// 3. **业务逻辑错误** — `NotFound` / `AlreadyExists` / `InvalidConfig` /
///    `NotInitialized`：peco-core 层特有的前置条件检查失败。
///
/// # 使用方式
///
/// ```ignore
/// fn do_something() -> Result<(), KnowledgeModuleError> {
///     // knowledge_base::KnowledgeError 自动通过 ? 转换
///     let mgr = KnowledgeBaseManager::load(&path).await?;
///     // std::io::Error 自动通过 ? 转换
///     let data = tokio::fs::read_to_string(&file).await?;
///     // 业务语义手动构造
///     if data.is_empty() {
///         return Err(KnowledgeModuleError::InvalidConfig("配置为空".into()));
///     }
///     Ok(())
/// }
/// ```
#[derive(Debug, thiserror::Error)]
pub enum KnowledgeModuleError {
    /// 底层 knowledge-base 引擎错误。
    ///
    /// 来自 `knowledge_base::KnowledgeError`，可能包含：
    /// - `StoreError` — 文档存储后端操作失败
    /// - `VectorError` — 向量索引操作失败
    /// - `EmbeddingError` — 嵌入模型推理失败
    /// - `SearchFailed` — 搜索执行异常
    /// - `NotFound` / `InvalidInput` / `Internal` — 其他引擎错误
    #[error("知识库错误: {0}")]
    Knowledge(#[from] KnowledgeError),

    /// 文件系统 I/O 错误。
    ///
    /// 常见场景：读取配置/清单文件失败、创建 KB 目录失败、扫描 docs/ 时目录不存在。
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    /// JSON 序列化/反序列化错误。
    ///
    /// 常见场景：`knowledge_config.json` 或 `file_hashes.json` 格式损坏。
    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),

    /// 指定的知识库不存在。
    ///
    /// 触发条件：`search_kb` / `sync_kb` / `open_kb` 时指定了未创建的知识库名称。
    #[error("知识库 '{0}' 不存在")]
    NotFound(String),

    /// 知识库已存在，无法重复创建。
    #[error("知识库 '{0}' 已存在")]
    AlreadyExists(String),

    /// 配置无效（参数校验失败）。
    #[error("无效配置: {0}")]
    InvalidConfig(String),

    /// 知识库管理器尚未初始化。
    ///
    /// 所有公共方法在内部自动调用 [`ensure_loaded()`]，
    /// 此错误仅在直接访问内部状态时出现。
    ///
    /// [`ensure_loaded()`]: super::KnowledgeManager::ensure_loaded
    #[error("知识库管理器尚未初始化，请先调用 ensure_loaded()")]
    NotInitialized,
}
