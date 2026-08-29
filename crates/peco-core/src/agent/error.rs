// ============================================================================
// Agent 错误类型
// ============================================================================

/// Agent 配置和构建过程中的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// 读取文件失败
    #[error("failed to read file '{path}': {source}")]
    Io {
        /// 被读取的文件路径
        path: std::path::PathBuf,
        /// 底层 I/O 错误
        #[source]
        source: std::io::Error,
    },

    /// YAML 解析失败（agent.md 格式）
    #[error("failed to parse YAML: {0}")]
    YamlParse(#[from] serde_yaml::Error),

    /// Frontmatter 格式无效
    #[error("invalid frontmatter: {0}")]
    InvalidFrontmatter(String),

    /// Provider 构建错误
    #[error("provider error: {0}")]
    Provider(#[from] model_provider::ProviderError),

    /// 缺少必填字段
    #[error("missing required field '{0}'")]
    MissingField(String),

    /// 环境变量未设置或无效
    #[error("environment variable '{0}' not set: {1}")]
    EnvVar(String, #[source] std::env::VarError),

    /// 配置错误（provider 未找到、类型不支持等）
    #[error("configuration error: {0}")]
    Config(String),

    /// 上下文压缩失败（摘要生成未完成/为空、会话修剪状态错误等）。
    ///
    /// compaction 是非致命路径 — 调用方仅记录日志，不影响会话继续。
    #[error("compaction error: {0}")]
    Compaction(String),

    /// A tool execution returned an error.
    #[error("tool execution error: tool={tool}, message={message}")]
    ToolExecution { tool: String, message: String },

    /// The agent exceeded its maximum number of ReAct turns.
    #[error("max turns exceeded: {max_turns} turn(s) allowed")]
    MaxTurns { max_turns: usize },

    /// Internal state machine protocol violation.
    #[error("agent protocol error: {0}")]
    AgentProtocol(String),
}
