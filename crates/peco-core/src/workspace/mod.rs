// ============================================================================
// WorkSpace 模块 — 用户隔离的核心抽象
// ============================================================================
//
// 提供：
// - [`WorkSpace`] — 用户工作空间，持有所有用户级资源
// - [`ToolRegister`] — 工具组装器，基于依赖注入一次构建到位
// - [`ToolDependencies`] — 工具构造时的窄 trait 依赖集合
// - [`WorkspaceError`] — WorkSpace 相关错误类型
//
// 窄 trait 接口（`deps` 模块）：
// - [`AgentLoader`] — Agent 加载能力
// - [`SkillProvider`] — Skill 读取能力
// - [`KnowledgeAccess`] — 知识库操作

mod deps;
mod error;
mod tool_register;
#[allow(clippy::module_inception)]
mod workspace;

pub use deps::{AgentLoader, KnowledgeAccess, SkillProvider, ToolDependencies};
pub use error::WorkspaceError;
pub use tool_register::ToolRegister;
pub use workspace::WorkSpace;
