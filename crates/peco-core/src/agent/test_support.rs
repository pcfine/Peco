// ============================================================================
// test_support — 子 agent 路径测试共用的桩件
// ============================================================================
//
// 放在 `agent` 下而不是各测试模块里：scripted provider 被 simple_looper 与
// tools::sub_agent 两处的端到端测试共用，复制一份会让「脚本消费顺序」这类
// 语义在两处漂移。

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures::Future;
use model_provider::{
    Content, ContentBlock, FinishReason, GenerateRequest, GenerateResult, GenerateStream,
    ModelProvider, ProviderError, ResponseStatus, ToolDefinition, Usage,
};
use tokio::sync::Notify;

use crate::agent::{Agent, AgentError};
use crate::tools::{AgentAccess, DefaultToolsExecutor, ToolDyn, ToolError};

/// 脚本化批量 provider：按顺序吐出预置响应，并记录每次请求收到的
/// `max_output_tokens`（用于断言截断重试确实抬了预算）。
pub(crate) struct ScriptedBatchProvider {
    scripts: Mutex<VecDeque<GenerateResult>>,
    budgets: Mutex<Vec<Option<u32>>>,
}

impl ScriptedBatchProvider {
    pub(crate) fn new(scripts: Vec<GenerateResult>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            budgets: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn budgets(&self) -> Vec<Option<u32>> {
        self.budgets.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ModelProvider for ScriptedBatchProvider {
    fn name(&self) -> &str {
        "scripted-batch"
    }

    async fn generate_full(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateResult, ProviderError> {
        self.budgets.lock().unwrap().push(request.max_output_tokens);
        if let Some(r) = self.scripts.lock().unwrap().pop_front() {
            return Ok(r);
        }
        // 脚本耗尽（不应发生）：返回一个正常完成的空响应。
        Ok(completed(""))
    }

    async fn generate_stream(
        &self,
        _request: &GenerateRequest,
    ) -> Result<GenerateStream, ProviderError> {
        Err(ProviderError::Request(
            "stream not used in batch-only tests".into(),
        ))
    }
}

/// 正常完成的文本响应。
pub(crate) fn completed(text: &str) -> GenerateResult {
    GenerateResult {
        id: "r-completed".into(),
        output: vec![ContentBlock::Text { text: text.into() }],
        usage: Usage::default(),
        status: ResponseStatus::Completed,
        finish_reason: Some(FinishReason::Stop),
        error: None,
    }
}

/// 截断响应：只有 reasoning、可见输出为空，output 顶到 4096。
pub(crate) fn truncated() -> GenerateResult {
    GenerateResult {
        id: "r-truncated".into(),
        output: vec![ContentBlock::Reasoning {
            text: "long reasoning that consumed the whole budget".into(),
        }],
        usage: Usage {
            output_tokens: 4096,
            ..Default::default()
        },
        status: ResponseStatus::Incomplete,
        finish_reason: Some(FinishReason::MaxTokens),
        error: None,
    }
}

/// 单个工具调用的响应（无可见文本）。
pub(crate) fn tool_call_response(
    call_id: &str,
    name: &str,
    arguments: serde_json::Value,
) -> GenerateResult {
    GenerateResult {
        id: format!("r-tool-{call_id}"),
        output: vec![ContentBlock::ToolCall {
            call_id: call_id.into(),
            name: name.into(),
            arguments: arguments.to_string(),
        }],
        usage: Usage::default(),
        status: ResponseStatus::Completed,
        finish_reason: Some(FinishReason::ToolCalls),
        error: None,
    }
}

fn scripted_agent_impl(
    scripts: Vec<GenerateResult>,
    max_tokens: Option<u32>,
    tools: Vec<Box<dyn ToolDyn>>,
) -> (Arc<Agent>, Arc<ScriptedBatchProvider>) {
    let profile: crate::agent::AgentProfile = serde_yaml::from_str(
        "agent:\n  name: t\n  description: d\nllm:\n  provider: p\n  model: m\n",
    )
    .unwrap();
    // 同一份执行器必须同时给 `from_parts`（工具定义来源）和 `McpManager`
    // （`execute_tools` 的实际执行入口）：只塞一处会得到「Tool not found」。
    let executor: Arc<dyn crate::tools::ToolExecutor> = Arc::new(DefaultToolsExecutor::new(tools));
    let provider = Arc::new(ScriptedBatchProvider::new(scripts));
    let mut llm = crate::agent::ModelConfigBuilder::new().stream(false);
    if let Some(tokens) = max_tokens {
        llm = llm.max_tokens(tokens);
    }
    let agent = Arc::new(Agent::from_parts(
        std::path::PathBuf::from("/tmp/agent.md"),
        profile,
        "sys".to_string(),
        Arc::clone(&provider) as Arc<dyn ModelProvider>,
        llm.build(),
        Arc::clone(&executor),
        Arc::new(crate::mcp::McpManager::empty(executor)),
        None,
    ));
    (agent, provider)
}

/// 无工具的 scripted agent。
pub(crate) fn scripted_agent(
    scripts: Vec<GenerateResult>,
) -> (Arc<Agent>, Arc<ScriptedBatchProvider>) {
    scripted_agent_impl(scripts, None, Vec::new())
}

/// 同 [`scripted_agent`]，但给 agent 的 `llm:` 显式配置 `max_tokens`。
pub(crate) fn scripted_agent_with_max_tokens(
    scripts: Vec<GenerateResult>,
    max_tokens: Option<u32>,
) -> (Arc<Agent>, Arc<ScriptedBatchProvider>) {
    scripted_agent_impl(scripts, max_tokens, Vec::new())
}

/// scripted agent + 预置工具（用于工具输出带声明的路径）。
pub(crate) fn scripted_agent_with_tools(
    responses: Vec<GenerateResult>,
    tools: Vec<Box<dyn ToolDyn>>,
) -> (Arc<Agent>, Arc<ScriptedBatchProvider>) {
    scripted_agent_impl(responses, None, tools)
}

/// 固定输出的工具。
pub(crate) struct StaticTool {
    name: String,
    output: Content,
}

impl StaticTool {
    pub(crate) fn new(name: &str, output: Content) -> Self {
        Self {
            name: name.to_string(),
            output,
        }
    }
}

fn tool_definition(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: "test tool".to_string(),
        parameters: serde_json::json!({ "type": "object", "properties": {} }),
    }
}

impl ToolDyn for StaticTool {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition(&self) -> ToolDefinition {
        tool_definition(&self.name)
    }

    fn call<'a>(
        &'a self,
        _args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move { Ok(self.output.clone()) })
    }
}

/// 可卡住的工具：先进 `entered`，等 `release` 才返回输出。
///
/// 用于在「工具已开始执行、结果尚未收集」的窗口里注入取消。
pub(crate) struct GateTool {
    name: String,
    pub(crate) entered: Arc<Notify>,
    pub(crate) release: Arc<Notify>,
    output: Content,
}

impl GateTool {
    pub(crate) fn new(name: &str, output: Content) -> Self {
        Self {
            name: name.to_string(),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            output,
        }
    }
}

impl ToolDyn for GateTool {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition(&self) -> ToolDefinition {
        tool_definition(&self.name)
    }

    fn call<'a>(
        &'a self,
        _args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(self.output.clone())
        })
    }
}

/// 按名字查 agent 的 [`AgentAccess`] 桩件。
pub(crate) struct MapAgentAccess(pub(crate) HashMap<String, Arc<Agent>>);

impl AgentAccess for MapAgentAccess {
    fn load_agent(&self, name: &str) -> Result<Arc<Agent>, AgentError> {
        self.0
            .get(name)
            .cloned()
            .ok_or_else(|| AgentError::Config(format!("no such agent: {name}")))
    }

    fn list_agent_names(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    fn save_agent(&self, _name: &str, _content: &str) -> Result<(), String> {
        Err("not supported".into())
    }

    fn read_agent(&self, _name: &str) -> Result<String, String> {
        Err("not supported".into())
    }

    fn delete_agent(&self, _name: &str) -> Result<(), String> {
        Err("not supported".into())
    }
}
