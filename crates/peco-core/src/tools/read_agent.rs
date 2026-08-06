// ============================================================================
// ReadAgent — 读取 agent.md 原始内容（依赖注入版）
// ============================================================================

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde_json::json;

use super::deps::AgentAccess;
use super::{StringError, ToolDyn, ToolError};

pub struct ReadAgent {
    agent_access: Arc<dyn AgentAccess>,
}

impl ReadAgent {
    pub fn new(agent_access: Arc<dyn AgentAccess>) -> Self {
        Self { agent_access }
    }
}

impl ToolDyn for ReadAgent {
    fn name(&self) -> String {
        "read_agent".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_agent".to_string(),
            description: "Read the full agent.md content of a specified agent. \
                Use this before modifying an existing agent — always read first, \
                then modify, then save_agent with the complete updated content."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The agent name to read (e.g., '@coding-assistant')."
                    }
                },
                "required": ["name"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct ReadAgentArgs {
                name: String,
            }

            let parsed: ReadAgentArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "agent name is required and cannot be empty".into(),
                ))));
            }

            self.agent_access.read_agent(name).map_err(|e| {
                ToolError::ToolCallError(Box::new(StringError(format!(
                    "failed to read agent '{name}': {e}"
                ))))
            })
        })
    }
}
