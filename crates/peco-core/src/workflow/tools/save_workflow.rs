// ============================================================================
// SaveWorkflow — 创建或更新 workflow.md
// ============================================================================

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use crate::tools::{StringError, ToolDyn, ToolError};
use crate::workflow::WorkflowAccess;

pub struct SaveWorkflow {
    workflow_access: Arc<dyn WorkflowAccess>,
}

impl SaveWorkflow {
    pub fn new(workflow_access: Arc<dyn WorkflowAccess>) -> Self {
        Self { workflow_access }
    }
}

impl ToolDyn for SaveWorkflow {
    fn name(&self) -> String {
        "save_workflow".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "save_workflow".to_string(),
            description: "Create or update a workflow by writing its workflow.md file. \
                If the workflow already exists, it is updated in place. \
                The content must be a complete workflow.md file with YAML frontmatter \
                (workflow.name, workflow.description, workflow.steps) followed by optional Markdown body.\n\
                \n\
                Required frontmatter:\n\
                  workflow.name: unique workflow identifier\n\
                  workflow.description: what the workflow does\n\
                  workflow.steps: array of step definitions\n\
                \n\
                Each step requires: id, name, type (shell/agent), config.\n\
                \n\
                Step config by type:\n\
                  shell: { command: \"shell command to run\" }\n\
                  agent: { agent: \"@agent-name\", prompt: \"what the agent should do\" }\n\
                    Optional: max_turns (integer, max ReAct loop iterations)\n\
                \n\
                Example:\n\
                  steps:\n\
                    - id: \"step1\"\n\
                      name: \"Run analysis\"\n\
                      type: agent\n\
                      config:\n\
                        agent: \"@analyst\"\n\
                        prompt: \"Analyze the data\"\n\
                \n\
                The workflow becomes immediately available for execute_workflow after creation."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Unique workflow name (ASCII alphanumeric, underscores, hyphens). 1-128 chars."
                    },
                    "content": {
                        "type": "string",
                        "description": "Complete workflow.md content: YAML frontmatter + optional Markdown body."
                    }
                },
                "required": ["name", "content"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct SaveWorkflowArgs {
                name: String,
                content: String,
            }

            let parsed: SaveWorkflowArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "workflow name is required and cannot be empty".into(),
                ))));
            }
            if parsed.content.trim().is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "workflow content is required and cannot be empty".into(),
                ))));
            }

            self.workflow_access
                .save_workflow(name, &parsed.content)
                .map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "failed to save workflow '{name}': {e}"
                    ))))
                })?;

            Ok(format!("Workflow '{name}' saved successfully."))
        })
    }
}
