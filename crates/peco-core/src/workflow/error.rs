// ============================================================================
// WorkflowError — workflow 模块错误类型
// ============================================================================

/// Workflow 执行过程中的所有错误。
#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    /// 定义文件解析失败（YAML 语法错误、缺失字段等）
    #[error("failed to parse workflow definition: {0}")]
    Parse(String),

    /// DAG 结构不合法（循环依赖、未知引用、自引用等）
    #[error("invalid DAG: {0}")]
    InvalidDag(String),

    /// 步骤执行失败
    #[error("step '{step_id}' failed: {message}")]
    StepExecution { step_id: String, message: String },

    /// 模板渲染错误（变量未定义、语法错误等）
    #[error("template error: {0}")]
    Template(String),

    /// 输入参数校验失败
    #[error("input validation failed: {0}")]
    InputValidation(String),

    /// Workflow 超时
    #[error("workflow timed out after {elapsed_seconds}s")]
    Timeout { elapsed_seconds: u64 },

    /// 被取消
    #[error("workflow cancelled")]
    Cancelled,

    /// 持久化错误（Phase 2）
    #[error("persistence error: {0}")]
    Persist(String),

    /// IO 错误
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Workflow 名称不合法
    #[error("invalid workflow name: {0}")]
    InvalidName(String),

    /// Workflow 已存在（创建时同名冲突）
    #[error("workflow already exists: {0}")]
    AlreadyExists(String),
}
