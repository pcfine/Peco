// ============================================================================
// DeleteAgent — 删除 Agent（依赖注入版）
// ============================================================================

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use super::deps::AgentAccess;
use super::{StringError, ToolDyn, ToolError};

pub struct DeleteAgent {
    agent_access: Arc<dyn AgentAccess>,
}

impl DeleteAgent {
    pub fn new(agent_access: Arc<dyn AgentAccess>) -> Self {
        Self { agent_access }
    }
}

impl ToolDyn for DeleteAgent {
    fn name(&self) -> String {
        "delete_agent".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delete_agent".to_string(),
            description: "Delete an agent and its agent.md file. This is irreversible. \
                The agent directory and all its contents will be permanently removed. \
                You CANNOT delete @assistant itself.\n\
                \n\
                Before deleting, explain the impact to the user and require explicit confirmation."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The agent name to delete."
                    },
                    "confirm": {
                        "type": "boolean",
                        "description": "Must be explicitly set to true to confirm deletion. \
                            Set to false or omit to abort."
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
            struct DeleteAgentArgs {
                name: String,
                confirm: bool,
            }

            let parsed: DeleteAgentArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            if !parsed.confirm {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "Deletion not confirmed. Set 'confirm' to true to proceed.".into(),
                ))));
            }

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "agent name is required and cannot be empty".into(),
                ))));
            }

            self.agent_access.delete_agent(name).map_err(|e| {
                ToolError::ToolCallError(Box::new(StringError(format!(
                    "failed to delete agent '{name}': {e}"
                ))))
            })?;

            Ok(format!("Agent '{name}' deleted successfully."))
        })
    }
}
