// ============================================================================
// DeleteWorkflow — 删除 Workflow
// ============================================================================

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use crate::tools::{StringError, ToolDyn, ToolError};
use crate::workflow::WorkflowAccess;

pub struct DeleteWorkflow {
    workflow_access: Arc<dyn WorkflowAccess>,
}

impl DeleteWorkflow {
    pub fn new(workflow_access: Arc<dyn WorkflowAccess>) -> Self {
        Self { workflow_access }
    }
}

impl ToolDyn for DeleteWorkflow {
    fn name(&self) -> String {
        "delete_workflow".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delete_workflow".to_string(),
            description: "Delete a workflow and its workflow.md file. This is irreversible. \
                The workflow directory and all its contents will be permanently removed. \
                Requires explicit confirmation."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The workflow name to delete."
                    },
                    "confirm": {
                        "type": "boolean",
                        "description": "Must be explicitly set to true to confirm deletion."
                    }
                },
                "required": ["name", "confirm"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct DeleteWorkflowArgs {
                name: String,
                confirm: bool,
            }

            let parsed: DeleteWorkflowArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            if !parsed.confirm {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "Deletion not confirmed. Set 'confirm' to true to proceed.".into(),
                ))));
            }

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "workflow name is required and cannot be empty".into(),
                ))));
            }

            self.workflow_access.delete_workflow(name).map_err(|e| {
                ToolError::ToolCallError(Box::new(StringError(format!(
                    "failed to delete workflow '{name}': {e}"
                ))))
            })?;

            Ok(format!("Workflow '{name}' deleted successfully."))
        })
    }
}
