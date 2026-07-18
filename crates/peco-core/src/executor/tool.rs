// ============================================================================
// AgentExecutorTool — 将 AgentExecutor 包装为 ToolDyn
// ============================================================================
//
// 实现 agent-as-tool 模式：让一个 agent 把另一个 agent 当工具调用。
// 被调用的 Agent 完全走自己的 AgentLooper，不绕过任何层级。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use model_provider::ToolDefinition;

use crate::agent::agent::Agent;
use crate::tools::{ToolDyn, ToolError};

use super::{AgentExecutor, ExecutorInput};

/// 将任意 AgentExecutor 包装为 ToolDyn，实现 agent-as-tool。
///
/// # 调用链
///
/// ```text
/// Agent A 的 Looper
///   → tool_call: { name: "run_translator", args: {prompt: "..."} }
///   → AgentExecutorTool.call(args)
///     → executor.execute(ExecutorInput { prompt, ... })
///       → SimpleAgentLooper::spawn(target_agent, prompt)
///         → ReAct loop (model → tools → … → final answer)
///   → 返回 output.content 作为 tool result
/// ```
///
/// # 限制
///
/// - **子 agent 无状态**：每次调用独立运行，不支持跨调用的上下文传递。
/// - **自动支持 tool calling**：SingleTurnExecutor 和 ReActExecutor 内部均使用
///   SimpleAgentLooper，自动处理模型的 tool 调用。
pub struct AgentExecutorTool {
    target_agent: Arc<Agent>,
    executor: Box<dyn AgentExecutor>,
    definition: ToolDefinition,
}

impl AgentExecutorTool {
    /// 创建新的 AgentExecutorTool。
    ///
    /// `target_agent` 是将被作为工具调用的 agent。
    /// `executor` 是驱动该 agent 的执行器（通常为 `SingleTurnExecutor` 或 `ReActExecutor`）。
    pub fn new(target_agent: Arc<Agent>, executor: Box<dyn AgentExecutor>) -> Self {
        let agent_name = &target_agent.config().agent.name;
        let agent_desc = &target_agent.config().agent.description;

        let name = format!("run_{}", agent_name);
        let definition = ToolDefinition {
            name,
            description: format!("Call the '{}' agent: {}", agent_name, agent_desc),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "The task for this agent to perform"
                    }
                },
                "required": ["prompt"]
            }),
        };

        Self {
            target_agent,
            executor,
            definition,
        }
    }

    /// 返回内部 Agent 的引用。
    pub fn target_agent(&self) -> &Arc<Agent> {
        &self.target_agent
    }

    /// 返回内部 Executor 的引用。
    pub fn executor(&self) -> &dyn AgentExecutor {
        self.executor.as_ref()
    }
}

impl ToolDyn for AgentExecutorTool {
    fn name(&self) -> String {
        self.definition.name.clone()
    }

    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            // 1. 解析参数
            let parsed: serde_json::Value =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let prompt = parsed["prompt"].as_str().unwrap_or(&args).to_string();

            // 2. 构建 ExecutorInput
            let input = ExecutorInput {
                prompt,
                context: Vec::new(),
                output_schema: None,
            };

            // 3. 执行
            let output = self
                .executor
                .execute(input)
                .await
                .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;

            // 4. 返回结果：优先 structured_data，否则 content
            if let Some(data) = output.structured_data {
                Ok(data.to_string())
            } else {
                Ok(output.content)
            }
        })
    }
}
