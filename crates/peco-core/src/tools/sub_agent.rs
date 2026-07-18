// ============================================================================
// sub_agent — Sub-Agent delegation tools
// ============================================================================
//
// Provides two tools for sub-agent orchestration:
//
// - `delegate_sub_agent`       Delegate a single task, block until complete
// - `run_parallel_sub_agents`  Run multiple tasks in parallel, return all results
//
// Both are driven by [`SimpleAgentLooper`], a minimal batch-only ReAct executor.
// No global task registry, no polling, no task_id management — the LLM calls
// the tool and gets the result(s) directly.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::agent::simple_looper::SimpleAgentLooper;
use crate::global_handler::GlobalHandler;
use crate::tools::ToolError;

// ============================================================================
// Shared types for run_parallel_sub_agents
// ============================================================================

/// Input: a single parallel task definition.
#[derive(Debug, Deserialize)]
struct ParallelTaskDef {
    agent_file: String,
    prompt: String,
}

/// Output: the result of a single parallel task.
#[derive(Debug, Serialize)]
struct ParallelTaskResult {
    agent_file: String,
    prompt: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

// ============================================================================
// Tool implementations (via #[peco_tool])
// ============================================================================

use peco_derive::peco_tool;

// ── delegate_sub_agent ─────────────────────────────────────────────────────

/// Delegate a task to a sub-agent and wait for the result (blocking).
///
/// The sub-agent runs a full ReAct loop (model → tools → … → final answer) and
/// the result is returned inline as the tool output. Use this for single
/// subtasks where you need the result before continuing.
///
/// For multiple parallel tasks, use `run_parallel_sub_agents` instead.
#[peco_tool(
    name = "delegate_sub_agent",
    description = "Delegate a task to a sub-agent and wait for the result. The sub-agent runs a full ReAct loop (model → tools → ... → final answer) and returns its output. Use this for single subtasks where you need the result before continuing. For parallel work, use run_parallel_sub_agents instead.",
    params(
        agent_file = "Path to the agent.md configuration file that defines the sub-agent (model, tools, system prompt). Example: 'agents/code-reviewer.md'.",
        prompt = "The task description / user query to send to the sub-agent. Be specific about what you want the sub-agent to do."
    )
)]
pub async fn delegate_sub_agent(
    agent_file: String,
    prompt: String,
) -> Result<String, ToolError> {
    let agent = GlobalHandler::global()
        .create_agent(&agent_file)
        .await
        .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;

    let agent = Arc::new(agent);
    let handle = SimpleAgentLooper::spawn(agent, prompt, None);

    let output = handle
        .wait()
        .await
        .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;

    Ok(output)
}

// ── run_parallel_sub_agents ────────────────────────────────────────────────

/// Run multiple sub-agent tasks in parallel and return all results.
///
/// Each task runs in its own tokio task using [`SimpleAgentLooper`]. All tasks
/// are spawned concurrently, then awaited in order. Results are returned as a
/// JSON array with status, output, and (on failure) error for each task.
///
/// # Example input
///
/// ```json
/// [
///   {"agent_file": "agents/reviewer.md", "prompt": "Review auth.rs"},
///   {"agent_file": "agents/reviewer.md", "prompt": "Review api.rs"}
/// ]
/// ```
#[peco_tool(
    name = "run_parallel_sub_agents",
    description = "Run multiple sub-agent tasks in parallel and return all results. Each task is defined by an agent_file (path to agent.md) and a prompt. All tasks run concurrently. Results are returned as a JSON array with status, output, and optional error for each task. Use this when you need to perform multiple independent subtasks at the same time.",
    params(
        tasks = "JSON array of task objects, each with 'agent_file' and 'prompt' fields. Example: [{\"agent_file\": \"agents/reviewer.md\", \"prompt\": \"Review auth.rs for security issues\"}, {\"agent_file\": \"agents/researcher.md\", \"prompt\": \"Research Rust async patterns\"}]"
    )
)]
pub async fn run_parallel_sub_agents(tasks: String) -> Result<String, ToolError> {
    // ── Parse tasks ──────────────────────────────────────────────────────────
    let task_defs: Vec<ParallelTaskDef> = serde_json::from_str(&tasks)
        .map_err(|e| ToolError::ToolCallError(
            format!("Failed to parse tasks JSON: {e}. Expected format: [{{\"agent_file\": \"...\", \"prompt\": \"...\"}}]").into()
        ))?;

    if task_defs.is_empty() {
        return Err(ToolError::ToolCallError(
            "tasks array is empty — provide at least one task".into(),
        ));
    }

    // ── Load all agents ──────────────────────────────────────────────────────
    let mut agents = Vec::with_capacity(task_defs.len());
    for td in &task_defs {
        let agent = GlobalHandler::global()
            .create_agent(&td.agent_file)
            .await
            .map_err(|e| ToolError::ToolCallError(
                format!("Failed to load agent '{}': {e}", td.agent_file).into()
            ))?;
        agents.push(Arc::new(agent));
    }

    // ── Spawn all tasks concurrently ─────────────────────────────────────────
    struct IndexedHandle {
        agent_file: String,
        prompt: String,
        handle: crate::agent::SimpleLooperHandle,
    }

    let mut handles = Vec::with_capacity(task_defs.len());
    for (td, agent) in task_defs.iter().zip(agents.into_iter()) {
        let handle = SimpleAgentLooper::spawn(agent, td.prompt.clone(), None);
        handles.push(IndexedHandle {
            agent_file: td.agent_file.clone(),
            prompt: td.prompt.clone(),
            handle,
        });
    }

    // ── Collect results in order ─────────────────────────────────────────────
    let mut results: Vec<ParallelTaskResult> = Vec::with_capacity(handles.len());

    for h in handles {
        match h.handle.wait().await {
            Ok(output) => {
                results.push(ParallelTaskResult {
                    agent_file: h.agent_file,
                    prompt: h.prompt,
                    status: "completed",
                    output: Some(output),
                    error: None,
                });
            }
            Err(e) => {
                results.push(ParallelTaskResult {
                    agent_file: h.agent_file,
                    prompt: h.prompt,
                    status: "failed",
                    output: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    // Sort by original definition order. Build a position map for O(n log n) sort.
    let positions: std::collections::HashMap<(&str, &str), usize> = task_defs
        .iter()
        .enumerate()
        .map(|(i, td)| ((td.agent_file.as_str(), td.prompt.as_str()), i))
        .collect();
    results.sort_by_key(|r| {
        positions
            .get(&(r.agent_file.as_str(), r.prompt.as_str()))
            .copied()
            .unwrap_or(usize::MAX)
    });

    Ok(serde_json::to_string_pretty(&results).map_err(ToolError::JsonError)?)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_tasks() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(run_parallel_sub_agents("[]".to_string()));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn test_parse_invalid_json() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(run_parallel_sub_agents("not json".to_string()));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Failed to parse"));
    }

    #[test]
    fn test_parse_missing_fields() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result =
            rt.block_on(run_parallel_sub_agents(r#"[{"agent_file": "a.md"}]"#.to_string()));
        // Missing 'prompt' field
        assert!(result.is_err());
    }

    #[test]
    fn test_result_serialization() {
        let result = ParallelTaskResult {
            agent_file: "agents/test.md".to_string(),
            prompt: "hello".to_string(),
            status: "completed",
            output: Some("world".to_string()),
            error: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("completed"));
        assert!(json.contains("world"));
        assert!(!json.contains("error"));
    }

    #[test]
    fn test_result_serialization_error() {
        let result = ParallelTaskResult {
            agent_file: "agents/test.md".to_string(),
            prompt: "hello".to_string(),
            status: "failed",
            output: None,
            error: Some("something broke".to_string()),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("failed"));
        assert!(json.contains("something broke"));
        // output is None, so it should be skipped
        assert!(!json.contains("output"));
    }
}
