// ============================================================================
// WorkSpace 模块 — 用户隔离的核心抽象
// ============================================================================
//
// 提供：
// - [`WorkSpace`] — 用户工作空间，持有所有用户级资源
// - [`WorkspaceError`] — WorkSpace 相关错误类型
//
// Tool 组装相关的 trait 和工厂已迁移到 [`crate::tools`] 模块：
// - [`crate::tools::ToolRegister`] — 工具组装器
// - [`crate::tools::ToolDependencies`] — 工具构造时的窄 trait 依赖集合
// - [`crate::tools::AgentLoader`] — Agent 加载能力
// - [`crate::tools::SkillProvider`] — Skill 读取能力
// - [`crate::tools::KnowledgeAccess`] — 知识库操作

mod error;
#[allow(clippy::module_inception)]
mod workspace;

// 向后兼容：重新导出已迁移到 tools 的符号
pub use crate::tools::{AgentLoader, KnowledgeAccess, SkillProvider, ToolDependencies, ToolRegister};
pub use error::WorkspaceError;
pub use workspace::{TemplateInitReport, WorkSpace};
