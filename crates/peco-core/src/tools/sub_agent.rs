// ============================================================================
// sub_agent — Sub-Agent delegation tools (dependency-injected)
// ============================================================================
//
// Both CLI and Web use the same tool implementations.
// Difference is only in how the AgentLoader is constructed:
// - CLI: Workspace reads agents/ from local filesystem
// - Web: Workspace reads agents/ from user workspace directory

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use crate::agent::simple_looper::SimpleAgentLooper;
use crate::workspace::AgentLoader;

use super::{StringError, ToolDyn, ToolError};

// ============================================================================
// DelegateSubAgent
// ============================================================================

pub struct DelegateSubAgent {
    agent_loader: Arc<dyn AgentLoader>,
}

impl DelegateSubAgent {
    pub fn new(agent_loader: Arc<dyn AgentLoader>) -> Self {
        Self { agent_loader }
    }
}

impl ToolDyn for DelegateSubAgent {
    fn name(&self) -> String {
        "delegate_sub_agent".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delegate_sub_agent".to_string(),
            description: "Delegate a task to a sub-agent by name and wait for the result. \
                The sub-agent runs a full ReAct loop (model → tools → ... → final answer) \
                and returns its output. Use this for single subtasks."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent_name": {
                        "type": "string",
                        "description": "Name of the sub-agent to delegate to. Must match an existing agent name exactly."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The task description to send to the sub-agent."
                    }
                },
                "required": ["agent_name", "prompt"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct SubAgentArgs {
                agent_name: String,
                prompt: String,
            }

            let parsed: SubAgentArgs = serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let agent_name = parsed.agent_name.trim();
            if agent_name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "agent_name is required".into(),
                ))));
            }

            let agent = self.agent_loader.load_agent(agent_name).map_err(|e| {
                ToolError::ToolCallError(Box::new(StringError(format!(
                    "failed to load agent '{agent_name}': {e}"
                ))))
            })?;

            let handle = SimpleAgentLooper::spawn(agent, parsed.prompt, None);
            let output = handle.wait().await.map_err(|e| {
                ToolError::ToolCallError(Box::new(StringError(format!(
                    "sub-agent '{agent_name}' execution failed: {e}"
                ))))
            })?;

            Ok(output)
        })
    }
}

// ============================================================================
// RunParallelSubAgents
// ============================================================================

pub struct RunParallelSubAgents {
    agent_loader: Arc<dyn AgentLoader>,
}

impl RunParallelSubAgents {
    pub fn new(agent_loader: Arc<dyn AgentLoader>) -> Self {
        Self { agent_loader }
    }
}

#[derive(Debug, Deserialize)]
struct ParallelTaskDef {
    agent_name: String,
    prompt: String,
}

impl ToolDyn for RunParallelSubAgents {
    fn name(&self) -> String {
        "run_parallel_sub_agents".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_parallel_sub_agents".to_string(),
            description: "Run multiple sub-agent tasks in parallel and return all results. \
                Each task is defined by an agent_name and a prompt. All tasks run concurrently."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "string",
                        "description": "JSON array of task objects, each with 'agent_name' and 'prompt'. \
                            Example: [{\"agent_name\": \"reviewer\", \"prompt\": \"Review auth.rs\"}]"
                    }
                },
                "required": ["tasks"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct TasksWrapper {
                tasks: String,
            }

            let wrapper: TasksWrapper =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let task_defs: Vec<ParallelTaskDef> =
                serde_json::from_str(&wrapper.tasks).map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "Failed to parse tasks JSON: {e}"
                    ))))
                })?;

            if task_defs.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "tasks array is empty".into(),
                ))));
            }

            // Load all agents
            let mut agent_pairs = Vec::with_capacity(task_defs.len());
            for td in &task_defs {
                let agent = self.agent_loader.load_agent(&td.agent_name).map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "failed to load agent '{}': {e}",
                        td.agent_name
                    ))))
                })?;
                agent_pairs.push((td.agent_name.clone(), td.prompt.clone(), agent));
            }

            // Spawn all concurrently
            struct IndexedHandle {
                agent_name: String,
                prompt: String,
                handle: crate::agent::SimpleLooperHandle,
            }

            let mut handles = Vec::with_capacity(agent_pairs.len());
            for (name, prompt, agent) in agent_pairs {
                let handle = SimpleAgentLooper::spawn(agent, prompt.clone(), None);
                handles.push(IndexedHandle {
                    agent_name: name,
                    prompt,
                    handle,
                });
            }

            // Collect results
            let mut results: Vec<serde_json::Value> = Vec::with_capacity(handles.len());
            for h in handles {
                match h.handle.wait().await {
                    Ok(output) => {
                        results.push(json!({
                            "agent_name": h.agent_name,
                            "prompt": h.prompt,
                            "status": "completed",
                            "output": output,
                        }));
                    }
                    Err(e) => {
                        results.push(json!({
                            "agent_name": h.agent_name,
                            "prompt": h.prompt,
                            "status": "failed",
                            "error": e.to_string(),
                        }));
                    }
                }
            }

            serde_json::to_string_pretty(&results).map_err(ToolError::JsonError)
        })
    }
}
