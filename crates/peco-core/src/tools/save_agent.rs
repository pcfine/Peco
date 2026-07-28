// ============================================================================
// SaveAgent — 创建或更新 Agent 的工具（依赖注入版）
// ============================================================================

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use super::deps::AgentAccess;

use super::{StringError, ToolDyn, ToolError};

pub struct SaveAgent {
    agent_access: Arc<dyn AgentAccess>,
}

impl SaveAgent {
    pub fn new(agent_access: Arc<dyn AgentAccess>) -> Self {
        Self { agent_access }
    }
}

impl ToolDyn for SaveAgent {
    fn name(&self) -> String {
        "save_agent".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "save_agent".to_string(),
            description: "Create or update an agent by writing its agent.md configuration file. \
                The content must start with YAML frontmatter between '---' delimiters, \
                followed by the system prompt in Markdown.\n\
                \n\
                Required frontmatter fields:\n\
                  agent.name: unique agent identifier\n\
                  agent.description: what this agent does\n\
                \n\
                Optional fields: llm (provider, model, temperature, max_tokens, stream, \
                reasoning_effort), tools, mcp, skills, knowledge_bases, max_turns (default 20).\n\
                \n\
                Available tool names: shell, fetch, read_skill, delegate_sub_agent, \
                run_parallel_sub_agents, save_agent, search_knowledge, list_knowledge_bases, \
                add_to_knowledge_base, sync_knowledge_base, get_knowledge_base_docs, \
                add_facts_to_knowledge_base, query_entity_facts.\n\
                \n\
                Example:\n\
                ---\n\
                agent:\n\
                  name: \"code-reviewer\"\n\
                  description: \"Reviews code for bugs and style issues\"\n\
                llm:\n\
                  provider: \"deepseek\"\n\
                  model: \"deepseek-v4-flash\"\n\
                tools: [\"shell\", \"fetch\", \"read_skill\"]\n\
                max_turns: 15\n\
                ---\n\
                You are a code reviewer. When given code, analyze it for...\n\
                \n\
                The agent becomes immediately available for delegate_sub_agent after creation."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Unique agent name. Used as the directory name and identifier for delegate_sub_agent."
                    },
                    "content": {
                        "type": "string",
                        "description": "Complete agent.md file content: YAML frontmatter (between '---' delimiters) followed by Markdown system prompt."
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
            struct SaveAgentArgs {
                name: String,
                content: String,
            }

            let parsed: SaveAgentArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "agent name is required and cannot be empty".into(),
                ))));
            }

            if parsed.content.trim().is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "agent content is required and cannot be empty".into(),
                ))));
            }

            self.agent_access
                .save_agent(name, &parsed.content)
                .map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "failed to save agent '{name}': {e}"
                    ))))
                })?;

            Ok(format!("Agent '{name}' saved successfully."))
        })
    }
}
