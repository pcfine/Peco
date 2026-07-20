// ============================================================================
// Agent — 从 agent.md 配置文件组装完整的 Agent 实例
// ============================================================================

use std::sync::Arc;

use std::sync::RwLock;

use crate::agent::agent_config::{
    AgentProfile, ModelConfig, ModelConfigBuilder, resolve_api_key, split_frontmatter,
};
use crate::agent::error::AgentError;
use model_provider::{
    ChatRequest, ChatResponse, ChatStream, DeepSeek, Message, ModelProvider, ToolDefinition, Usage,
};

use serde::{Deserialize, Serialize};

use crate::config::{McpServerConfig, UserConfig};
use crate::mcp::McpManager;
use crate::skills::SkillRegistry;
use crate::tools::ToolExecutor;
use crate::workspace::ToolDependencies;

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

// ── MessageFilter ─────────────────────────────────────────────────────────────

/// Agent 层消息过滤器。
///
/// 在上下文构建**之前**对 Session 中的 [`AnnotatedMessage`] 引用列表进行过滤。
/// System prompt 和动态上下文由 [`build_context`](super::context::build_context)
/// 单独注入，不经过此过滤器，因此不会被误修改或误删除。
///
/// 典型用途包括：按 turn 过滤历史消息、脱敏、注入提醒等。
///
/// 与 [`ContextFilter`](super::context::ContextFilter) 的区别：
/// - `ContextFilter` 从 Session 历史中选择*哪些*消息进入上下文（替代 build_context 内部策略）
/// - `MessageFilter` 在 build_context **之前**对消息做预处理（system prompt 不可见）
///
/// 默认为 `None`（不过滤），可通过外部注入 `dyn` trait 对象覆盖。
pub trait MessageFilter: Send + Sync {
    /// 对 AnnotatedMessage 引用列表进行过滤/转换，返回处理后的结果。
    fn filter(
        &self,
        messages: &[&crate::session::AnnotatedMessage],
    ) -> Vec<crate::session::AnnotatedMessage>;
}

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

    /// Skill 注册表（从 Workspace 注入）。
    skill_registry: Arc<RwLock<SkillRegistry>>,
}

impl Agent {
    /// 从组件直接构造 Agent（供 Web 层程序化创建，替代 [`from_file`](Agent::from_file)）。
    ///
    /// 所有依赖（ModelProvider、ToolExecutor、McpManager）由调用方构建后传入，
    /// Agent 本身不再执行 I/O 或配置解析。
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        md_path: std::path::PathBuf,
        profile: AgentProfile,
        preamble: String,
        model: Arc<dyn ModelProvider>,
        model_config: ModelConfig,
        tool_executor: Arc<dyn ToolExecutor>,
        mcp_manager: Arc<McpManager>,
        skill_registry: Arc<RwLock<SkillRegistry>>,
    ) -> Self {
        Agent {
            md_path,
            profile,
            preamble,
            model,
            model_config,
            tool_executor,
            mcp_manager,
            skill_registry,
        }
    }

    /// 从 `agent.md` 文件创建 Agent（新签名：显式依赖注入，同步）。
    ///
    /// 接收显式依赖作为参数，内部完成：
    /// 1. 解析 agent.md → profile + preamble
    /// 2. 通过 ToolRegister 构建 tool_executor
    /// 3. 通过 user_config 构建 ModelProvider
    /// 4. 注入 skill_registry
    /// 5. 构建 McpManager（MCP 连接延迟到首次使用时建立）
    pub fn from_file(
        path: impl AsRef<std::path::Path>,
        user_config: &UserConfig,
        skill_registry: &Arc<RwLock<SkillRegistry>>,
        tool_deps: &ToolDependencies,
    ) -> Result<Self, AgentError> {
        let path = path.as_ref();
        // ── 解析 agent.md ──────────────────────────────────────────────
        let raw = std::fs::read_to_string(path).map_err(|source| AgentError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        tracing::info!("read agent file: {}", path.to_string_lossy());
        let (frontmatter_str, body) =
            split_frontmatter(&raw).map_err(AgentError::InvalidFrontmatter)?;
        let profile: AgentProfile = serde_yaml::from_str(frontmatter_str)?;
        let preamble = body.to_string();

        // 构建最终生效的模型配置：agent.md 覆盖 → 合并 UserConfig
        let model_config = build_model_config_with_user(&profile, user_config);

        tracing::debug!(
            provider = ?model_config.provider_name,
            model = ?model_config.model_name,
            temperature = ?model_config.temperature,
            "Model config resolved"
        );

        // ── 1. 构建 ModelProvider ──────────────────────────────────────────
        let model = build_provider_with_user(&model_config, user_config)?;

        tracing::info!(
            provider = %model.name(),
            agent = %profile.agent.name,
            "Agent assembled successfully"
        );

        // ── 2. 构建 ToolExecutor（通过 ToolRegister，依赖注入）────────────
        let tool_executor = crate::workspace::ToolRegister::build(&profile.tools, tool_deps);

        tracing::info!(
            tool_count = tool_executor.definitions().len(),
            tools = ?profile.tools,
            "Built-in tools registered for agent"
        );

        // ── 3. 构建 McpManager（lazy connect）─────────────────────────────
        let mcp_servers: Vec<(String, McpServerConfig)> = profile
            .mcp
            .iter()
            .filter_map(|name| {
                user_config
                    .mcp
                    .get_server(name)
                    .filter(|c| c.enabled)
                    .map(|c| (name.clone(), c.clone()))
            })
            .collect();

        for name in &profile.mcp {
            if user_config.mcp.get_server(name).is_none() {
                tracing::warn!(
                    server = %name,
                    "MCP server declared in agent.md but not found in mcpconfig.json"
                );
            }
        }

        let mcp_manager = Arc::new(McpManager::new_lazy(&mcp_servers, tool_executor.clone()));

        tracing::info!(
            mcp_count = mcp_manager.server_count(),
            mcp_names = ?mcp_manager.server_names(),
            "MCP manager created (connections lazy)"
        );

        Ok(Agent {
            md_path: path.to_path_buf(),
            profile,
            preamble,
            model,
            model_config,
            tool_executor,
            mcp_manager,
            skill_registry: skill_registry.clone(),
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
            let skill_list = self.skill_registry.read().expect("RwLock poisoned");
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
                            "Skill declared in agent.md but not found in SkillRegistry"
                        );
                    }
                }
            }
            prompt.push_str(&section);
        }
        prompt
    }

    // ── 请求发送方法 ───────────────────────────────────────────────────────────

    /// 构造 [`ChatRequest`]。
    fn build_chat_request(
        &self,
        messages: Vec<Arc<Message>>,
        tools: Vec<ToolDefinition>,
    ) -> ChatRequest {
        ChatRequest {
            model: self.model_config.model_name.clone().unwrap_or_default(),
            messages,
            tools,
            temperature: self.model_config.temperature.map(|t| t as f64),
            max_tokens: self.model_config.max_tokens.map(|t| t as u64),
            reasoning_effort: self.model_config.reasoning_effort.clone(),
            additional_params: None,
        }
    }

    /// 发送非流式 chat 请求。
    ///
    /// 调用方需自行构建完整的消息列表（含 system prompt）。
    /// Agent 负责收集 tool 定义、构造 ChatRequest、调用 provider。
    pub(crate) async fn chat(
        &self,
        messages: Vec<Arc<Message>>,
    ) -> Result<ChatResponse, AgentError> {
        let tools = self.tool_executor.definitions();
        let request = self.build_chat_request(messages, tools);
        Ok(self.model.chat(&request).await?)
    }

    /// 发送流式 chat 请求。
    ///
    /// 调用方需自行构建完整的消息列表（含 system prompt）。
    /// Agent 负责收集 tool 定义、构造 ChatRequest、调用 provider。
    pub(crate) async fn stream_chat(
        &self,
        messages: Vec<Arc<Message>>,
    ) -> Result<ChatStream, AgentError> {
        let tools = self.tool_executor.definitions();
        let request = self.build_chat_request(messages, tools);
        Ok(self.model.stream_chat(&request).await?)
    }
}

// ── 辅助函数 ────────────────────────────────────────────────────────────────────

/// 从 AgentProfile 构建 ModelConfig（使用 UserConfig 替代 GlobalHandler）。
pub fn build_model_config_with_user(
    profile: &AgentProfile,
    user_config: &UserConfig,
) -> ModelConfig {
    let mut builder = ModelConfigBuilder::new();

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

    let provider_name = model_config
        .provider_name
        .as_deref()
        .unwrap_or_else(|| user_config.default_provider_name())
        .to_string();

    let model_config = ModelConfig {
        provider_name: Some(provider_name.clone()),
        ..model_config
    };

    let defaults = user_config
        .provider_entry(Some(&provider_name))
        .and_then(|e| e.default.as_ref());

    model_config.merge_defaults(defaults)
}

/// 从 ModelConfig 构建 ModelProvider（使用 UserConfig 替代 GlobalHandler）。
pub fn build_provider_with_user(
    model_config: &ModelConfig,
    user_config: &UserConfig,
) -> Result<Arc<dyn ModelProvider>, AgentError> {
    let provider_name = model_config
        .provider_name
        .as_deref()
        .unwrap_or_else(|| user_config.default_provider_name());

    let entry = user_config
        .provider_entry(Some(provider_name))
        .ok_or_else(|| {
            AgentError::Config(format!(
                "provider '{provider_name}' not found in providers.toml"
            ))
        })?;

    let api_key = match &entry.api_key {
        Some(key) => resolve_api_key(key)?,
        None => {
            return Err(AgentError::Config(format!(
                "no api_key configured for provider '{provider_name}'"
            )));
        }
    };

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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
