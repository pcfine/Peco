// ============================================================================
// ListWorkflows — 列出所有 Workflow 名称、描述、版本、步骤数
// ============================================================================

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde_json::json;

use crate::tools::{StringError, ToolDyn, ToolError};
use crate::workflow::WorkflowAccess;

pub struct ListWorkflows {
    workflow_access: Arc<dyn WorkflowAccess>,
}

impl ListWorkflows {
    pub fn new(workflow_access: Arc<dyn WorkflowAccess>) -> Self {
        Self { workflow_access }
    }
}

impl ToolDyn for ListWorkflows {
    fn name(&self) -> String {
        "list_workflows".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_workflows".to_string(),
            description: "List all available workflows in the workspace with their names, \
                descriptions, versions, and step counts. \
                Use this to discover what workflows exist before executing or modifying them."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let _ = args;
            let metas = self.workflow_access.list_workflow_meta();
            let workflows: Vec<_> = metas
                .iter()
                .map(|m| {
                    json!({
                        "name": m.name,
                        "description": m.description,
                        "version": m.version,
                        "step_count": m.step_count,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&workflows)
                .map_err(|e| ToolError::ToolCallError(Box::new(StringError(e.to_string()))))
        })
    }
}
