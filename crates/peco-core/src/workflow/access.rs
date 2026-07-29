// ============================================================================
// WorkflowAccess trait — 窄接口，遵循 AgentAccess 模式
// ============================================================================

use super::definition::WorkflowDefinition;
use super::error::WorkflowError;

/// Workflow 文件加载接口。
///
/// 遵循 `AgentAccess` 的窄 trait 模式。由 `WorkSpace` 和 `WorkflowManager` 实现。
/// 供 `execute_workflow` 工具和其他需要加载 workflow 定义的组件使用。
pub trait WorkflowAccess: Send + Sync {
    /// 按名称加载 WorkflowDefinition。
    fn load_workflow(&self, name: &str) -> Result<WorkflowDefinition, WorkflowError>;

    /// 列出所有可用 Workflow 名称。
    fn list_workflow_names(&self) -> Vec<String>;

    /// 强制重新加载指定 Workflow（缓存失效时使用）。
    fn reload_workflow(&self, name: &str) -> Result<WorkflowDefinition, WorkflowError>;
}
