// ============================================================================
// WorkspaceError — WorkSpace 操作的所有错误类型
// ============================================================================

use std::io;

/// WorkSpace 操作产生的错误。
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// 配置加载或合并错误。
    #[error("config error: {0}")]
    Config(#[from] crate::config::ConfigError),

    /// 文件 I/O 错误。
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// Agent 文件格式校验失败。
    #[error("invalid agent format: {0}")]
    InvalidAgentFormat(String),

    /// Skill 扫描/加载错误。
    #[error("skill error: {0}")]
    Skill(#[from] crate::skills::SkillError),

    /// Agent 相关错误。
    #[error("agent error: {0}")]
    Agent(#[from] crate::agent::AgentError),

    /// 工作空间目录创建失败。
    #[error("workspace directory error: {0}")]
    WorkspaceDir(String),
}
