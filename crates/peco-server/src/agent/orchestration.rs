// ============================================================================
// Web-Aware Sub-Agent Tools — DB-based agent resolution (replaces file-based)
// ============================================================================
//
// 标准 `delegate_sub_agent` 工具接受 `agent_file`（文件路径）参数。
// Web 环境中 Agent 存储在 SQLite 中，LLM 不应知道文件路径。
//
// 这两个 `ToolDyn` 实现替换了标准工具：
// - `WebDelegateSubAgentTool`     — 按 agent_name 查找并委托单个任务
// - `WebRunParallelSubAgentsTool` — 按 agent_name 查找并并行执行多个任务
//
// 两者都使用 `AgentRegistry` 进行 DB 解析和 Agent 构建，
// 使用 `SimpleAgentLooper` 执行子 Agent 的 ReAct 循环。

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use peco_core::agent::{SimpleAgentLooper, SimpleLooperHandle};
use peco_core::tools::{StringError, ToolDyn, ToolError};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;

use crate::agent::AgentRegistry;
use crate::db::agents;

// ============================================================================
// WebDelegateSubAgentTool
// ============================================================================

/// Web 版 `delegate_sub_agent` 工具。
///
/// 按 `agent_name` 从 DB 查找 Agent → 通过 `AgentRegistry` 构建 →
/// `SimpleAgentLooper` 执行 → 返回结果。
pub struct WebDelegateSubAgentTool {
    pool: SqlitePool,
    registry: Arc<AgentRegistry>,
    data_dir: PathBuf,
    user_id: String,
}

impl WebDelegateSubAgentTool {
    pub fn new(
        pool: SqlitePool,
        registry: Arc<AgentRegistry>,
        data_dir: PathBuf,
        user_id: String,
    ) -> Self {
        Self {
            pool,
            registry,
            data_dir,
            user_id,
        }
    }
}

impl ToolDyn for WebDelegateSubAgentTool {
    fn name(&self) -> String {
        "delegate_sub_agent".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delegate_sub_agent".to_string(),
            description: "Delegate a task to a sub-agent by name and wait for the result. \
                The sub-agent runs a full ReAct loop and returns its output. \
                Use this for single subtasks where you need the result before continuing. \
                For parallel work, use run_parallel_sub_agents instead."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent_name": {
                        "type": "string",
                        "description": "Name of the sub-agent to delegate to (e.g. '代码审查员'). \
                            Must match an existing agent name exactly."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The task description / user query to send to the sub-agent. \
                            Be specific about what you want the sub-agent to do."
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
            // ── 1. 解析参数 ──────────────────────────────────────────────────
            let parsed: SubAgentArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let agent_name = parsed.agent_name.trim();
            if agent_name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "agent_name is required".into(),
                ))));
            }

            // ── 2. 从 DB 查找 Agent ──────────────────────────────────────────
            let agent_row = agents::find_by_name_and_user(&self.pool, agent_name, &self.user_id)
                .await
                .map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "database error looking up agent '{agent_name}': {e}"
                    ))))
                })?;

            let agent_row = match agent_row {
                Some(row) => row,
                None => {
                    // 列出所有可用 Agent 名称以帮助 LLM 自我修正
                    let available = match agents::list_by_user(&self.pool, &self.user_id).await {
                        Ok(rows) => {
                            let names: Vec<String> =
                                rows.iter().map(|r: &crate::db::agents::AgentRow| r.name.clone()).collect();
                            names.join(", ")
                        }
                        Err(_) => "(unavailable)".to_string(),
                    };
                    return Err(ToolError::ToolCallError(Box::new(StringError(format!(
                        "Agent '{agent_name}' not found. Available agents: [{available}]"
                    )))));
                }
            };

            // ── 3. 通过 AgentRegistry 构建（含 LRU 缓存）─────────────────────
            let agent = self
                .registry
                .get_or_build(self.registry.clone(), &self.pool, &self.user_id, &agent_row.id, &self.data_dir)
                .await
                .map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "failed to build agent '{agent_name}': {e}"
                    ))))
                })?;

            // ── 4. 执行子 Agent（SimpleAgentLooper）──────────────────────────
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
// WebRunParallelSubAgentsTool
// ============================================================================

/// Web 版 `run_parallel_sub_agents` 工具。
///
/// 接受 JSON 数组 `[{agent_name, prompt}]`，并发执行所有子 Agent 任务，
/// 返回 JSON 数组结果（含 status / output / error）。
pub struct WebRunParallelSubAgentsTool {
    pool: SqlitePool,
    registry: Arc<AgentRegistry>,
    data_dir: PathBuf,
    user_id: String,
}

impl WebRunParallelSubAgentsTool {
    pub fn new(
        pool: SqlitePool,
        registry: Arc<AgentRegistry>,
        data_dir: PathBuf,
        user_id: String,
    ) -> Self {
        Self {
            pool,
            registry,
            data_dir,
            user_id,
        }
    }
}

impl ToolDyn for WebRunParallelSubAgentsTool {
    fn name(&self) -> String {
        "run_parallel_sub_agents".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_parallel_sub_agents".to_string(),
            description: "Run multiple sub-agent tasks in parallel and return all results. \
                Each task is defined by an agent_name and a prompt. \
                All tasks run concurrently. Results are returned as a JSON array \
                with status, output, and optional error for each task. \
                Use this when you need to perform multiple independent subtasks at the same time."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "string",
                        "description": "JSON array of task objects, each with 'agent_name' and 'prompt' fields. \
                            Example: [{\"agent_name\": \"代码审查员\", \"prompt\": \"Review auth.rs\"}, \
                            {\"agent_name\": \"文档编写员\", \"prompt\": \"Write docs\"}]"
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
            // ── 1. 解析参数 ──────────────────────────────────────────────────
            let wrapper: ParallelTasksWrapper =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let task_defs: Vec<ParallelTaskDef> =
                serde_json::from_str(&wrapper.tasks).map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "Failed to parse tasks JSON: {e}. \
                         Expected format: [{{\"agent_name\": \"...\", \"prompt\": \"...\"}}]"
                    ))))
                })?;

            if task_defs.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "tasks array is empty — provide at least one task".into(),
                ))));
            }

            // ── 2. 并行构建所有 Agent ────────────────────────────────────────
            let mut agent_handles: Vec<(String, String, Arc<peco_core::agent::Agent>)> =
                Vec::with_capacity(task_defs.len());

            for td in &task_defs {
                let name = td.agent_name.trim();
                let row = agents::find_by_name_and_user(&self.pool, name, &self.user_id)
                    .await
                    .map_err(|e| {
                        ToolError::ToolCallError(Box::new(StringError(format!(
                            "database error looking up agent '{name}': {e}"
                        ))))
                    })?
                    .ok_or_else(|| {
                        ToolError::ToolCallError(Box::new(StringError(format!(
                            "Agent '{name}' not found"
                        ))))
                    })?;

                let agent = self
                    .registry
                    .get_or_build(self.registry.clone(), &self.pool, &self.user_id, &row.id, &self.data_dir)
                    .await
                    .map_err(|e| {
                        ToolError::ToolCallError(Box::new(StringError(format!(
                            "failed to build agent '{name}': {e}"
                        ))))
                    })?;

                agent_handles.push((name.to_string(), td.prompt.clone(), agent));
            }

            // ── 3. 并发执行所有任务 ──────────────────────────────────────────
            let mut handles: Vec<(String, String, SimpleLooperHandle)> =
                Vec::with_capacity(agent_handles.len());

            for (name, prompt, agent) in agent_handles {
                let handle = SimpleAgentLooper::spawn(agent, prompt.clone(), None);
                handles.push((name, prompt, handle));
            }

            // ── 4. 按顺序收集结果 ────────────────────────────────────────────
            let mut results: Vec<serde_json::Value> = Vec::with_capacity(handles.len());

            for (name, prompt, handle) in handles {
                match handle.wait().await {
                    Ok(output) => {
                        results.push(json!({
                            "agent_name": name,
                            "prompt": prompt,
                            "status": "completed",
                            "output": output,
                        }));
                    }
                    Err(e) => {
                        results.push(json!({
                            "agent_name": name,
                            "prompt": prompt,
                            "status": "failed",
                            "error": e.to_string(),
                        }));
                    }
                }
            }

            Ok(serde_json::to_string_pretty(&results)
                .map_err(ToolError::JsonError)?)
        })
    }
}

// ============================================================================
// Shared types
// ============================================================================

/// `delegate_sub_agent` 的参数。
#[derive(Debug, Deserialize)]
struct SubAgentArgs {
    agent_name: String,
    prompt: String,
}

/// `run_parallel_sub_agents` 任务列表的包装结构。
#[derive(Debug, Deserialize)]
struct ParallelTasksWrapper {
    tasks: String,
}

/// 单个并行任务定义。
#[derive(Debug, Deserialize)]
struct ParallelTaskDef {
    agent_name: String,
    prompt: String,
}
