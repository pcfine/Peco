// ============================================================================
// WorkflowAccess trait — 窄接口，遵循 AgentAccess 模式
// ============================================================================

use super::definition::WorkflowDefinition;
use super::error::WorkflowError;
use super::manager::WorkflowMeta;

/// Workflow 文件加载接口。
///
/// 遵循 `AgentAccess` 的窄 trait 模式。由 `WorkSpace` 和 `WorkflowManager` 实现。
/// 供 `execute_workflow` 工具和其他需要加载 workflow 定义的组件使用。
pub trait WorkflowAccess: Send + Sync {
    /// 按名称加载 WorkflowDefinition。
    fn load_workflow(&self, name: &str) -> Result<WorkflowDefinition, WorkflowError>;

    /// 列出所有可用 Workflow 名称。
    fn list_workflow_names(&self) -> Vec<String>;

    /// 列出所有 Workflow 元数据（名称、描述、版本、步骤数）。
    fn list_workflow_meta(&self) -> Vec<WorkflowMeta>;

    /// 强制重新加载指定 Workflow（缓存失效时使用）。
    fn reload_workflow(&self, name: &str) -> Result<WorkflowDefinition, WorkflowError>;

    /// 创建或更新 workflow.md 文件。
    /// `content` 必须是完整的 workflow.md 内容（YAML frontmatter + Markdown body）。
    fn save_workflow(&self, name: &str, content: &str) -> Result<(), String>;

    /// 删除 Workflow 目录（不可逆操作）。
    fn delete_workflow(&self, name: &str) -> Result<(), String>;
}
