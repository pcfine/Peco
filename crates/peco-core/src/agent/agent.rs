// ============================================================================
// Agent — 从 agent.md 配置文件组装完整的 Agent 实例
// ============================================================================

use std::sync::Arc;

use model_provider::{
    ChatRequest, ChatResponse, ChatStream, DeepSeek, Message, ModelProvider, ToolDefinition, Usage,
};
use crate::GlobalHandler;
use crate::agent::agent_config::{
    AgentProfile, ModelConfig, ModelConfigBuilder, resolve_api_key, split_frontmatter,
};
use crate::agent::error::AgentError;

use serde::{Deserialize, Serialize};

use crate::config::McpServerConfig;
use crate::mcp::McpManager;
use crate::tools::{ToolExecutor, ToolFactory};

/// The response from a completed agent run.
///
/// Carries aggregate usage and turn count. Per-turn text output is delivered
/// via the `outcome` field of [`LooperEvent::TurnComplete`] ([`TurnOutcome`]),
/// and messages are managed by [`Session`].
///
/// [`LooperEvent::TurnComplete`]: crate::agent::agent_looper::LooperEvent::TurnComplete
/// [`TurnOutcome`]: crate::agent::agent_looper::TurnOutcome
/// [`Session`]: crate::session::Session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    /// Aggregated token usage across all model calls.
    pub usage: Usage,
    /// Number of model calls made.
    pub turns: usize,
}

// ── Agent ───────────────────────────────────────────────────────────────────────

/// 一个从 `agent.md` 配置文件加载的完整 Agent 实例。
///
/// 包含 LLM provider（已注册工具和 MCP 工具）、Skill 引用等全部组件。
/// 通过 [`Agent::from_file`] 创建。
///
/// # 字段说明
///
/// - `model` — 已注册全部工具的 LLM provider，可直接调用
///   [`ModelProvider::chat`] 等方法
/// - `tool_names` / `mcp_server_names` / `skills` — 用于内省和日志记录
pub struct Agent {
    /// agent.md 文件路径（用于日志和错误信息）。
    md_path: std::path::PathBuf,
    /// 解析后的 YAML frontmatter 配置。
    profile: AgentProfile,
    /// Markdown body（用作 system prompt / preamble）。
    preamble: String,

    /// LLM provider（已注册工具，可直接调用）
    model: Arc<dyn ModelProvider>,

    /// agent.md 中声明的模型配置和 provider 默认值的合并结果
    model_config: ModelConfig,
    /// 注册的内置工具名称（用于内省）
    tool_executor: Arc<dyn ToolExecutor>,

    /// 根据profile.mcp构建的MCP管理器，用于管理MCP连接
    mcp_manager: Arc<McpManager>,
}

impl Agent {
    /// 从组件直接构造 Agent（供 Web 层程序化创建，替代 [`from_file`](Agent::from_file)）。
    ///
    /// 所有依赖（ModelProvider、ToolExecutor、McpManager）由调用方构建后传入，
    /// Agent 本身不再执行 I/O 或配置解析。
    pub fn from_parts(
        md_path: std::path::PathBuf,
        profile: AgentProfile,
        preamble: String,
        model: Arc<dyn ModelProvider>,
        model_config: ModelConfig,
        tool_executor: Arc<dyn ToolExecutor>,
        mcp_manager: Arc<McpManager>,
    ) -> Self {
        Agent {
            md_path,
            profile,
            preamble,
            model,
            model_config,
            tool_executor,
            mcp_manager,
        }
    }

    /// 从 `agent.md` 文件创建 Agent。
    /// 若文件读取、YAML 解析、provider 构建等任一环节失败，返回 [`AgentError`]。
    pub async fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, AgentError> {
        let path = path.as_ref();
        // ── 解析 agent.md ──────────────────────────────────────────────
        let raw = std::fs::read_to_string(path)?;
        tracing::info!("read agent file: {}", path.to_string_lossy());
        let (frontmatter_str, body) =
            split_frontmatter(&raw).map_err(AgentError::InvalidFrontmatter)?;
        let profile: AgentProfile = serde_yaml::from_str(frontmatter_str)?;
        let preamble = body.to_string();

        // 构建最终生效的模型配置：agent.md 覆盖 → 合并 providers.toml 默认值
        let model_config = build_model_config(&profile);

        tracing::debug!(
            provider = ?model_config.provider_name,
            model = ?model_config.model_name,
            temperature = ?model_config.temperature,
            "Model config resolved (agent.md merged with provider defaults)"
        );
        // ── 1. 构建 ModelProvider ──────────────────────────────────────────
        let model = build_provider(&model_config)?;

        tracing::info!(
            provider = %model.name(),
            agent = %profile.agent.name,
            "Agent assembled successfully"
        );

        // ── 2. 构建 ToolExecutor ──────────────────────────────────────────
        let tool_factory = ToolFactory::global();
        let tool_executor: Arc<dyn ToolExecutor> =
            Arc::new(tool_factory.make_tools_executor(&profile.tools));

        tracing::info!(
            tool_count = tool_executor.definitions().len(),
            tools = ?profile.tools,
            "Built-in tools registered for agent"
        );

        // ── 3. 构建 McpManager ────────────────────────────────────────────
        let mcp_config = GlobalHandler::global().config().mcp_config();
        let mcp_servers: Vec<(String, McpServerConfig)> = profile
            .mcp
            .iter()
            .filter_map(|name| {
                mcp_config
                    .get_server(name)
                    .filter(|c| c.enabled)
                    .map(|c| (name.clone(), c.clone()))
            })
            .collect();

        // 对 profile 中声明了但未在 McpConfig 中找到的 server 发出警告
        for name in &profile.mcp {
            if mcp_config.get_server(name).is_none() {
                tracing::warn!(
                    server = %name,
                    "MCP server declared in agent.md but not found in mcpconfig.json"
                );
            }
        }

        let mcp_manager = Arc::new(McpManager::new(&mcp_servers, tool_executor.clone()).await);

        tracing::info!(
            mcp_count = mcp_manager.server_count(),
            mcp_names = ?mcp_manager.server_names(),
            "MCP connections established for agent"
        );

        Ok(Agent {
            md_path: path.to_path_buf(),
            profile,
            preamble,
            model,
            model_config,
            tool_executor,
            mcp_manager,
        })
    }

    /// 返回内部 [`ModelProvider`] 的引用。
    pub fn provider(&self) -> &Arc<dyn ModelProvider> {
        &self.model
    }

    /// 返回 Agent 的解析配置（从 agent.md 解析）。
    pub fn config(&self) -> &AgentProfile {
        &self.profile
    }

    /// 返回 agent.md 文件路径。
    pub fn path(&self) -> &std::path::Path {
        &self.md_path
    }

    /// 返回合并后的模型配置。
    pub fn model_config(&self) -> &ModelConfig {
        &self.model_config
    }

    /// 返回 MCP 管理器的引用。
    pub fn mcp_manager(&self) -> &Arc<McpManager> {
        &self.mcp_manager
    }

    /// 返回 ToolExecutor 的引用（用于运行时动态添加/移除工具）。
    pub fn tool_executor(&self) -> &Arc<dyn ToolExecutor> {
        &self.tool_executor
    }

    /// 返回此 Agent 单次运行的最大对话轮数。
    pub fn max_turns(&self) -> usize {
        self.profile.max_turns
    }

    /// 综合前缀和 skill 描述，返回完整的 system prompt。
    pub fn system_prompt(&self) -> String {
        let mut prompt = self.preamble.clone();
        if !self.profile.skills.is_empty() {
            let skill_list = GlobalHandler::global()
                .skill_list()
                .read()
                .expect("RwLock poisoned");
            let all_meta = skill_list.all_meta();

            let mut section = String::from("\n\n## Available Skills\n\n");
            for skill_name in &self.profile.skills {
                match all_meta.iter().find(|m| m.name == *skill_name) {
                    Some(meta) => {
                        section.push_str(&format!("- **{}**: {}\n", meta.name, meta.description));
                    }
                    None => {
                        tracing::warn!(
                            skill = %skill_name,
                            "Skill declared in agent.md but not found in GlobalSkillList"
                        );
                    }
                }
            }
            prompt.push_str(&section);
        }
        prompt
    }

    // ── 请求发送方法 ───────────────────────────────────────────────────────────

    /// 从 session 消息构建完整请求消息列表（System prompt 动态注入）。
    ///
    /// System 消息不写入 Session — 每次请求时动态生成，确保修改 agent.md 后立即生效。
    ///
    /// 接收所有权以避免 clone 整个消息列表：多轮对话中 history 随轮次线性增长，
    /// 每次都 clone 导致 O(n²) 的 clone 开销。改为 `insert(0, ...)` 仅移动指针。
    fn build_request_messages(&self, mut session_messages: Vec<Message>) -> Vec<Message> {
        session_messages.insert(0, Message::system(self.system_prompt()));
        session_messages
    }

    /// 构造 [`ChatRequest`]。
    fn build_chat_request(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> ChatRequest {
        // 构建 additional_params：
        // - 若 reasoning_effort 配置了值，则按配置传递 thinking 参数
        // - 若未配置 reasoning_effort，则显式禁用 thinking 模式，
        //   避免 DeepSeek V4 系列模型默认开启 thinking 导致输出进入 reasoning_content
        let additional_params = match &self.model_config.reasoning_effort {
            Some(effort) if !effort.is_empty() => {
                let effort_lower = effort.to_lowercase();
                if effort_lower == "disabled" || effort_lower == "none" {
                    Some(serde_json::json!({"thinking": {"type": "disabled"}}))
                } else {
                    Some(serde_json::json!({"thinking": {"type": "enabled", "effort": effort_lower}}))
                }
            }
            _ => {
                // 默认禁用 thinking，防止模型输出进入 reasoning_content
                Some(serde_json::json!({"thinking": {"type": "disabled"}}))
            }
        };

        ChatRequest {
            model: self.model_config.model_name.clone().unwrap_or_default(),
            messages,
            tools,
            temperature: self.model_config.temperature.map(|t| t as f64),
            max_tokens: self.model_config.max_tokens.map(|t| t as u64),
            additional_params,
        }
    }

    /// 发送非流式 chat 请求。
    ///
    /// 内部完成：构建消息列表 → 收集 tool 定义 → 构造 ChatRequest → 调用 provider.chat()。
    ///
    /// 接收 [`Vec<Message>`] 所有权以避免 clone 整个消息列表。
    /// 多轮对话中 history 随轮次线性增长，每次 clone 导致 O(n²) 的 clone 开销。
    pub(crate) async fn chat(
        &self,
        session_messages: Vec<Message>,
    ) -> Result<ChatResponse, AgentError> {
        let messages = self.build_request_messages(session_messages);
        let tools = self.tool_executor.definitions();
        let request = self.build_chat_request(messages, tools);
        Ok(self.model.chat(&request).await?)
    }

    /// 发送流式 chat 请求。
    ///
    /// 内部完成：构建消息列表 → 收集 tool 定义 → 构造 ChatRequest → 调用 provider.stream_chat()。
    ///
    /// 接收 [`Vec<Message>`] 所有权以避免 clone 整个消息列表。
    /// 多轮对话中 history 随轮次线性增长，每次 clone 导致 O(n²) 的 clone 开销。
    pub(crate) async fn stream_chat(
        &self,
        session_messages: Vec<Message>,
    ) -> Result<ChatStream, AgentError> {
        let messages = self.build_request_messages(session_messages);
        let tools = self.tool_executor.definitions();
        let request = self.build_chat_request(messages, tools);
        Ok(self.model.stream_chat(&request).await?)
    }
}

// ── 辅助函数 ────────────────────────────────────────────────────────────────────

/// 从 [`AgentProfile`](crate::agent::AgentProfile) 的 LLM 配置构建最终生效的 [`ModelConfig`]。
///
/// 合并策略（高优先级覆盖低优先级）：
/// 1. agent.md 中的 `llm` 显式配置（最高优先级）
/// 2. providers.toml 中 provider 的 `[providers.<name>.default]` 默认参数
///
/// 若 agent.md 未指定 `provider`，则使用 `providers.toml` 中的 `default_provider`。
pub fn build_model_config(profile: &crate::agent::agent_config::AgentProfile) -> ModelConfig {
    let mut builder = ModelConfigBuilder::new();

    // Step 1: 从 agent.md 的 llm 段读取显式覆盖值
    if let Some(ref llm) = profile.llm {
        if let Some(ref provider) = llm.provider {
            builder = builder.provider_name(provider.clone());
        }
        if let Some(ref model) = llm.model {
            builder = builder.model_name(model.clone());
        }
        if let Some(t) = llm.temperature {
            builder = builder.temperature(t as f32);
        }
        if let Some(m) = llm.max_tokens {
            builder = builder.max_tokens(m as usize);
        }
        if let Some(s) = llm.stream {
            builder = builder.stream(s);
        }
        if let Some(ref e) = llm.reasoning_effort {
            builder = builder.reasoning_effort(e.clone());
        }
    }

    let model_config = builder.build();

    // Step 2: 确定 provider 名称（agent.md 未指定则用全局默认值）
    let handler = GlobalHandler::global();
    let config = handler.config();

    let provider_name = model_config
        .provider_name
        .as_deref()
        .unwrap_or_else(|| config.default_provider_name())
        .to_string();

    // 确保 provider_name 已设置
    let model_config = ModelConfig {
        provider_name: Some(provider_name.clone()),
        ..model_config
    };

    // Step 3: 合并 providers.toml 中的 provider 默认参数
    let defaults = config
        .provider_entry(Some(&provider_name))
        .and_then(|e| e.default.as_ref());

    model_config.merge_defaults(defaults)
}

/// 从合并后的 [`ModelConfig`] 构建 [`ModelProvider`] 实例。
///
/// 使用 `model_config.provider_name` 在 `providers.toml` 中查找对应的 provider
/// 条目，解析 API key 和 base URL，并根据 `provider_type` 构造具体实例。
///
/// # 解析流程
///
/// 1. 从 `model_config.provider_name` 获取 provider 名称
/// 2. 从 [`GlobalHandler`] 的 Provider 配置中查找 API key 和 base URL
/// 3. 根据 `provider_type` 构造对应的 provider 实例（当前支持 `deepseek`）
///
/// # Errors
///
/// 若 provider 未在 `providers.toml` 中配置、缺少 API key、或 provider 类型不
/// 支持，返回 [`AgentError`]。
pub fn build_provider(model_config: &ModelConfig) -> Result<Arc<dyn ModelProvider>, AgentError> {
    let handler = GlobalHandler::global();
    let config = handler.config();

    let provider_name = model_config
        .provider_name
        .as_deref()
        .unwrap_or_else(|| config.default_provider_name());

    // 从全局配置中查找 provider 条目
    let entry = config.provider_entry(Some(provider_name)).ok_or_else(|| {
        AgentError::Config(format!(
            "provider '{provider_name}' not found in providers.toml"
        ))
    })?;

    // 解析 API Key
    let api_key = match &entry.api_key {
        Some(key) => resolve_api_key(key)?,
        None => {
            return Err(AgentError::Config(format!(
                "no api_key configured for provider '{provider_name}'"
            )));
        }
    };

    // 根据 provider 类型构建实例
    match entry.provider_type.as_str() {
        "deepseek" => {
            let mut provider = DeepSeek::new(api_key)?;
            if let Some(ref url) = entry.base_url {
                provider = provider.with_base_url(url.clone());
            }
            Ok(Arc::new(provider))
        }
        other => Err(AgentError::Config(format!(
            "unsupported provider type: '{other}'. Currently supported: deepseek"
        ))),
    }
}

// ── 日志辅助函数 ─────────────────────────────────────────────────────────────────

/// 截断字符串用于日志输出，超出长度追加 `…(N more chars)`。
fn truncate_for_log(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len).collect();
        format!("{truncated}…({} more chars)", s.len() - max_len)
    }
}

/// 从 assistant 消息中提取日志摘要信息。
///
/// 返回 `(text_preview, reasoning_preview, tool_call_names)`。
fn extract_response_info(message: &Message) -> (String, String, String) {
    let (content, tool_calls, reasoning) = match message {
        Message::Assistant {
            content,
            tool_calls,
            reasoning_content,
        } => (content, tool_calls, reasoning_content),
        _ => return (String::from("—"), String::from("—"), String::from("—")),
    };

    let text_preview = content
        .as_deref()
        .filter(|c| !c.is_empty())
        .map(|c| truncate_for_log(c, 200))
        .unwrap_or_else(|| String::from("(none)"));

    let reasoning_preview = reasoning
        .as_deref()
        .filter(|r| !r.is_empty())
        .map(|r| truncate_for_log(r, 200))
        .unwrap_or_else(|| String::from("(none)"));

    let tool_call_names = tool_calls
        .as_ref()
        .map(|tcs| {
            tcs.iter()
                .map(|tc| tc.function.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| String::from("(none)"));

    (text_preview, reasoning_preview, tool_call_names)
}
