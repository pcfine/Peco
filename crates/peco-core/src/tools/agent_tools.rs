// ============================================================================
// Agent tools — Agent CRUD（依赖注入版）
//   ReadAgent   — 读取 agent.md 原始内容
//   SaveAgent   — 创建或更新 Agent
//   DeleteAgent — 删除 Agent（不可逆，需显式确认）
// ============================================================================

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use super::deps::AgentAccess;
use super::{StringError, ToolDyn, ToolError};

// ── ReadAgent ────────────────────────────────────────────────────────────────

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

// ── SaveAgent ────────────────────────────────────────────────────────────────

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
                reasoning_effort), tools, mcp, skills, knowledge_bases, max_turns (default 50).\n\
                \n\
                Available tool names: shell, fetch, show_workspace, read_agent, read_skill, \
                list_skills, list_workflows, list_mcp_servers, list_knowledge_bases, \
                search_knowledge, get_knowledge_base_docs, query_entity_facts, \
                save_agent, delete_agent, save_skill, delete_skill, save_workflow, \
                delete_workflow, save_mcp_server, delete_mcp_server, \
                add_to_knowledge_base, add_facts_to_knowledge_base, sync_knowledge_base, \
                delegate_sub_agent, run_parallel_sub_agents, execute_workflow.\n\
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

            // 安全边界：不能覆盖 @assistant 自身
            if name == "@assistant" {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "Cannot overwrite @assistant — it is the meta-agent managing this workspace."
                        .into(),
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

// ── DeleteAgent ──────────────────────────────────────────────────────────────

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
