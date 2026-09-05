// ============================================================================
// Agent — 从 agent.md 配置文件组装完整的 Agent 实例
// ============================================================================

use std::sync::Arc;

use crate::agent::agent_config::{
    AgentProfile, ModelConfig, ModelConfigBuilder, resolve_api_key, split_frontmatter,
};
use crate::agent::error::AgentError;
use model_provider::{
    DeepSeek, DeepSeekResponsesAdapter, GenerateRequest, GenerateResult, GenerateStream, InputItem,
    ModelProvider, OpenAI, OpenAiResponsesAdapter, QwenChatCompletionsAdapter,
    QwenResponsesAdapter, ReasoningConfig, ReasoningEffort, ToolChoice, ToolDefinition, Usage,
};

use serde::{Deserialize, Serialize};

use crate::config::{McpServerConfig, ProviderEntry, UserConfig};
use crate::mcp::McpManager;
use crate::skills::SkillRegister;
use crate::tools::ToolDependencies;
use crate::tools::ToolExecutor;

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
    ///
    /// # 图片部件契约
    ///
    /// 消息内容可为纯文本或文本 + 图片部件混排（[`model_provider::Content`]）。
    /// 只处理文本的实现应将含图消息**整体透传**；需要删除或变换图片的过滤器
    /// 自行处理 image 部件。调用方在过滤器输入含图片部件时会发出 `warn!` 日志
    ///（含计数），保证"图片未经本过滤器处理"对排障始终可见 — 脱敏类过滤器
    /// 不允许静默旁路图片。
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

    /// Skill 注册表（从 WorkSpace 注入）。None 表示此 Agent 不使用 Skill。
    skill_registry: Option<Arc<SkillRegister>>,
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
        skill_registry: Option<Arc<SkillRegister>>,
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
    /// 2. 从 `profile.skills` 推导是否注入 SkillRegister（非空时从 tool_deps 取）
    /// 3. 通过 ToolRegister 构建 tool_executor
    /// 4. 通过 user_config 构建 ModelProvider
    /// 5. 构建 McpManager（MCP 连接延迟到首次使用时建立）
    pub fn from_file(
        path: impl AsRef<std::path::Path>,
        user_config: &UserConfig,
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
        let preamble = crate::config::resolve_env_vars(body);

        // 从 profile.skills 推导是否需要 SkillRegister：非空时从 tool_deps 注入
        let skill_registry = if profile.skills.is_empty() {
            None
        } else {
            Some(tool_deps.skill_provider.skill_registry().clone())
        };

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
        // 从 agent profile 覆盖 KB 白名单（按 Agent 隔离）
        let mut tool_deps = tool_deps.clone();
        tool_deps.allowed_kbs = profile.knowledge_bases.clone();
        let tool_executor = crate::tools::ToolRegister::build(&profile.tools, &tool_deps);

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
            skill_registry,
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
        if !self.profile.skills.is_empty()
            && let Some(ref skill_registry) = self.skill_registry
        {
            let all_meta = skill_registry.all_meta();

            let mut section = String::from("\n\n## Available Skills\n\n");
            for skill_name in &self.profile.skills {
                match all_meta.iter().find(|m| m.name == *skill_name) {
                    Some(meta) => {
                        section.push_str(&format!("- **{}**: {}\n", meta.name, meta.description));
                    }
                    None => {
                        tracing::warn!(
                            skill = %skill_name,
                            "Skill declared in agent.md but not found in SkillRegister"
                        );
                    }
                }
            }
            prompt.push_str(&section);
        }
        prompt
    }

    // ── 请求发送方法 ───────────────────────────────────────────────────────────

    /// 构造中立 [`GenerateRequest`]。
    ///
    /// `instructions` 承载 system prompt（含动态上下文），`input` 为历史 [`InputItem`]。
    fn build_generate_request(
        &self,
        input: Vec<Arc<InputItem>>,
        instructions: Option<String>,
        tools: Vec<ToolDefinition>,
    ) -> GenerateRequest {
        GenerateRequest {
            model: self.model_config.model_name.clone().unwrap_or_default(),
            instructions,
            input: input.into(),
            tools,
            tool_choice: Some(ToolChoice::Auto),
            temperature: self.model_config.temperature.map(|t| t as f64),
            top_p: None,
            max_output_tokens: self.model_config.max_tokens,
            reasoning: reasoning_effort_to_config(self.model_config.reasoning_effort.as_deref()),
            text: None,
            additional_params: None,
        }
    }

    /// 发送非流式生成请求。
    ///
    /// 调用方提供历史 `input`（不含 system prompt）；system prompt 由
    /// `self.system_prompt()` 承载为 `instructions`。
    pub(crate) async fn generate_full(
        &self,
        input: Vec<Arc<InputItem>>,
    ) -> Result<GenerateResult, AgentError> {
        let tools = self.tool_executor.definitions();
        let instructions = Some(self.system_prompt());
        let request = self.build_generate_request(input, instructions, tools);
        Ok(self.model.generate_full(&request).await?)
    }

    /// 发送流式生成请求。
    pub(crate) async fn generate_stream(
        &self,
        input: Vec<Arc<InputItem>>,
        instructions: Option<String>,
    ) -> Result<GenerateStream, AgentError> {
        let tools = self.tool_executor.definitions();
        let request = self.build_generate_request(input, instructions, tools);
        Ok(self.model.generate_stream(&request).await?)
    }

    /// 发送非流式生成请求，使用指定的 tool 定义。
    ///
    /// 由 `SimpleAgentLooper` 在 tool_executor_override 场景下使用，
    /// 允许调用方提供自定义 tool 列表（如包含 `__submit_output__`）。
    pub(crate) async fn generate_with_tools(
        &self,
        input: Vec<Arc<InputItem>>,
        instructions: Option<String>,
        tools: Vec<ToolDefinition>,
    ) -> Result<GenerateResult, AgentError> {
        let request = self.build_generate_request(input, instructions, tools);
        Ok(self.model.generate_full(&request).await?)
    }
}

/// 将 agent.md 的 `reasoning_effort` 字符串（`Option<String>`）映射为中立 [`ReasoningConfig`]。
///
/// 规则对齐旧 chat 适配器的 `thinking` 映射：
/// - `None` → 返回 `None`（适配器按 provider 默认启用 high）；
/// - `"disabled"`/`"none"` → `enabled: false`；
/// - `"low"`/`"medium"`/`"high"`/`"max"` → `enabled: true` + 对应 effort；
/// - 其它（含空串/未知值）→ `enabled: true` + effort `None`（provider 默认）。
fn reasoning_effort_to_config(effort: Option<&str>) -> Option<ReasoningConfig> {
    let effort = effort?;
    let lower = effort.to_lowercase();
    if lower.is_empty() {
        return None;
    }
    if lower == "disabled" || lower == "none" {
        return Some(ReasoningConfig {
            enabled: false,
            effort: None,
        });
    }
    let effort = match lower.as_str() {
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "max" => Some(ReasoningEffort::Max),
        _ => None,
    };
    Some(ReasoningConfig {
        enabled: true,
        effort,
    })
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
            builder = builder.max_tokens(m);
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

/// 提取 provider 条目的 `api` 模式：去除首尾空白，空串视同未配置。
fn api_mode_of(entry: &ProviderEntry) -> Option<&str> {
    entry
        .api
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// 解析 provider（未实例化）的用户消息图片输入能力。
///
/// 与 [`build_provider_with_user`] 的适配器分派同源：openai 两 API 皆支持；
/// qwen 仅 chat completions（responses 保守剥离）；deepseek 不支持；
/// 未知 provider 名或类型不支持。仅反映用户消息维度 — 工具输出图片
/// 是另一维度，各适配器另有矩阵。
pub fn supports_image_input_for(user_config: &UserConfig, provider_name: Option<&str>) -> bool {
    let provider_name = provider_name.unwrap_or_else(|| user_config.default_provider_name());
    match user_config.provider_entry(Some(provider_name)) {
        Some(entry) => match entry.provider_type.as_str() {
            "openai" => true,
            "qwen" => api_mode_of(entry).is_some_and(|a| a.eq_ignore_ascii_case("chat")),
            _ => false,
        },
        None => false,
    }
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
            // 按 `api` 字段选择适配器：默认（含 None）→ 原生 /responses，
            // `"chat"` → chat completions，`"responses"` → 原生 /responses，
            // 其它值显式报错，避免拼写错误静默落到 responses 难排错。
            let api_mode = api_mode_of(entry);

            let use_responses = match api_mode {
                Some(api) if api.eq_ignore_ascii_case("chat") => false,
                Some(api) if api.eq_ignore_ascii_case("responses") => true,
                None => true,
                Some(other) => {
                    return Err(AgentError::Config(format!(
                        "unsupported api mode '{other}' for provider '{provider_name}'. \
                         Expected 'chat' or 'responses'"
                    )));
                }
            };

            if use_responses {
                let mut provider = DeepSeekResponsesAdapter::new(api_key)?;
                if let Some(ref url) = entry.base_url {
                    provider = provider.with_base_url(url.clone());
                }
                Ok(Arc::new(provider))
            } else {
                let mut provider = DeepSeek::new(api_key)?;
                if let Some(ref url) = entry.base_url {
                    provider = provider.with_base_url(url.clone());
                }
                Ok(Arc::new(provider))
            }
        }
        "qwen" => {
            // 按 `api` 字段选择适配器：默认（含 None）→ OpenAI 兼容 /responses，
            // `"chat"` → chat completions（显式选择时保留），其它值显式报错。
            let api_mode = api_mode_of(entry);

            let use_responses = match api_mode {
                Some(api) if api.eq_ignore_ascii_case("chat") => false,
                Some(api) if api.eq_ignore_ascii_case("responses") => true,
                None => true,
                Some(other) => {
                    return Err(AgentError::Config(format!(
                        "unsupported api mode '{other}' for provider '{provider_name}'. \
                         Expected 'chat' or 'responses'"
                    )));
                }
            };

            if use_responses {
                let mut provider = QwenResponsesAdapter::new(api_key)?;
                if let Some(ref url) = entry.base_url {
                    provider = provider.with_base_url(url.clone());
                }
                Ok(Arc::new(provider))
            } else {
                let mut provider = QwenChatCompletionsAdapter::new(api_key)?;
                if let Some(ref url) = entry.base_url {
                    provider = provider.with_base_url(url.clone());
                }
                Ok(Arc::new(provider))
            }
        }
        "openai" => {
            // 按 `api` 字段选择适配器：默认（含 None）与 `"chat"` → chat completions
            // （存量默认不变更，兼容网关也走 chat）；`"responses"` → OpenAI 原生
            // /responses（显式 opt-in）；其它值显式报错。
            let api_mode = api_mode_of(entry);

            match api_mode {
                Some(api) if api.eq_ignore_ascii_case("responses") => {
                    let mut provider = OpenAiResponsesAdapter::new(api_key)?;
                    if let Some(ref url) = entry.base_url {
                        provider = provider.with_base_url(url.clone());
                    }
                    Ok(Arc::new(provider))
                }
                Some(api) if api.eq_ignore_ascii_case("chat") => {
                    let mut provider = OpenAI::new(api_key)?;
                    if let Some(ref url) = entry.base_url {
                        provider = provider.with_base_url(url.clone());
                    }
                    Ok(Arc::new(provider))
                }
                None => {
                    let mut provider = OpenAI::new(api_key)?;
                    if let Some(ref url) = entry.base_url {
                        provider = provider.with_base_url(url.clone());
                    }
                    Ok(Arc::new(provider))
                }
                Some(other) => Err(AgentError::Config(format!(
                    "unsupported api mode '{other}' for provider '{provider_name}'. \
                         Expected 'chat' or 'responses'"
                ))),
            }
        }
        other => Err(AgentError::Config(format!(
            "unsupported provider type: '{other}'. Currently supported: deepseek, qwen, openai"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{McpConfig, ProviderEntry, ProvidersConfig};
    use crate::session::{AnnotatedMessage, MessageId, MessageSource};
    use model_provider::{Content, ContentPart, InputItem, Role};
    use std::collections::HashMap;

    /// 只处理文本的过滤器：纯文本消息被替换为占位符（模拟脱敏逻辑），
    /// 含图消息整体透传 — 只懂文本的实现不允许拆散部件数组。
    struct TextOnlyFilter;

    impl MessageFilter for TextOnlyFilter {
        fn filter(
            &self,
            messages: &[&crate::session::AnnotatedMessage],
        ) -> Vec<crate::session::AnnotatedMessage> {
            messages
                .iter()
                .map(|am| {
                    let replaced = match am.message.as_ref() {
                        InputItem::Message { role, content } if content.is_plain_text() => {
                            InputItem::Message {
                                role: *role,
                                content: Content::Text(format!(
                                    "[filtered] {}",
                                    content.text_view()
                                )),
                            }
                        }
                        other => other.clone(),
                    };
                    AnnotatedMessage::new(am.id, am.turn_index, replaced, am.source.clone())
                })
                .collect()
        }
    }

    /// MessageFilter 图片部件契约：只处理文本的过滤器把含图消息整体透传，
    /// 图片部件不被静默旁路（旁路本身由调用方的 warn! 日志保证可见）。
    #[test]
    fn test_message_filter_text_only_passes_parts_through_intact() {
        let parts = Content::Parts(vec![
            ContentPart::Text {
                text: "机密文件截图".to_string(),
            },
            ContentPart::Image {
                url: "https://example.com/secret.png".to_string(),
                detail: None,
            },
        ]);
        let annotated = AnnotatedMessage::new(
            MessageId(0),
            0,
            InputItem::Message {
                role: Role::User,
                content: parts.clone(),
            },
            MessageSource::UserInput,
        );
        let plain = AnnotatedMessage::new(
            MessageId(1),
            0,
            InputItem::Message {
                role: Role::User,
                content: Content::Text("纯文本消息".to_string()),
            },
            MessageSource::UserInput,
        );
        let refs = vec![&annotated, &plain];

        let out = TextOnlyFilter.filter(&refs);
        assert_eq!(out.len(), 2);
        // 含图消息原样通过
        match out[0].message.as_ref() {
            InputItem::Message { content, .. } => {
                assert_eq!(content, &parts);
                assert_eq!(content.image_count(), 1);
            }
            other => panic!("expected message, got {other:?}"),
        }
        // 纯文本消息被过滤器改写
        match out[1].message.as_ref() {
            InputItem::Message { content, .. } => {
                assert_eq!(content.text_view(), "[filtered] 纯文本消息");
            }
            other => panic!("expected message, got {other:?}"),
        }
    }

    /// 构造一个仅含指定 provider 条目的最小 UserConfig。
    fn user_config(name: &str, entry: ProviderEntry) -> UserConfig {
        let mut providers = HashMap::new();
        providers.insert(name.to_string(), entry);
        UserConfig {
            providers: ProvidersConfig {
                default_provider: name.to_string(),
                providers,
                web_search: None,
            },
            mcp: McpConfig::empty(),
        }
    }

    fn model_config(provider_name: &str) -> ModelConfig {
        ModelConfig {
            provider_name: Some(provider_name.to_string()),
            model_name: None,
            temperature: None,
            max_tokens: None,
            stream: None,
            reasoning_effort: None,
        }
    }

    fn qwen_entry() -> ProviderEntry {
        ProviderEntry {
            provider_type: "qwen".to_string(),
            api_key: Some("sk-test-qwen".to_string()),
            base_url: None,
            api: None,
            default: None,
        }
    }

    /// 未配置 `api` 时默认走 OpenAI 兼容 /responses。
    fn entry_of(provider_type: &str, api: Option<&str>) -> ProviderEntry {
        ProviderEntry {
            provider_type: provider_type.to_string(),
            api_key: Some("sk-test".to_string()),
            base_url: None,
            api: api.map(str::to_string),
            default: None,
        }
    }

    #[test]
    fn test_supports_image_input_openai_both_apis() {
        for api in [None, Some("chat"), Some("responses")] {
            let config = user_config("openai", entry_of("openai", api));
            assert!(
                supports_image_input_for(&config, Some("openai")),
                "openai api={api:?} 应支持用户图片输入"
            );
        }
    }

    #[test]
    fn test_supports_image_input_qwen_chat_only() {
        let responses = user_config("qwen", entry_of("qwen", None));
        assert!(!supports_image_input_for(&responses, Some("qwen")));

        let chat = user_config("qwen", entry_of("qwen", Some("chat")));
        assert!(supports_image_input_for(&chat, Some("qwen")));
    }

    #[test]
    fn test_supports_image_input_unsupported_and_unknown() {
        let deepseek = user_config("deepseek", entry_of("deepseek", None));
        assert!(!supports_image_input_for(&deepseek, Some("deepseek")));

        // provider 名在配置中不存在 → 不支持
        assert!(!supports_image_input_for(&deepseek, Some("nope")));
    }

    #[test]
    fn test_supports_image_input_falls_back_to_default_provider() {
        let config = user_config("qwen", entry_of("qwen", Some("chat")));
        assert!(supports_image_input_for(&config, None));
    }

    #[test]
    fn test_qwen_branch_defaults_to_responses_adapter() {
        let entry = qwen_entry();
        assert!(entry.api.is_none());
        let config = user_config("qwen", entry);
        let provider =
            build_provider_with_user(&model_config("qwen"), &config).expect("qwen must build");
        assert_eq!(provider.name(), "qwen-responses");
    }

    #[test]
    fn test_qwen_branch_builds_chat_adapter() {
        let entry = ProviderEntry {
            api: Some("chat".to_string()),
            ..qwen_entry()
        };
        let config = user_config("qwen", entry);
        let provider = build_provider_with_user(&model_config("qwen"), &config)
            .expect("qwen api = 'chat' must build");
        assert_eq!(provider.name(), "qwen");
    }

    #[test]
    fn test_qwen_branch_applies_base_url() {
        let entry = ProviderEntry {
            api: Some("chat".to_string()),
            base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
            ..qwen_entry()
        };
        let config = user_config("qwen", entry);
        let provider = build_provider_with_user(&model_config("qwen"), &config)
            .expect("qwen api = 'chat' must build");
        assert_eq!(provider.name(), "qwen");
    }

    #[test]
    fn test_qwen_branch_builds_responses_adapter() {
        let entry = ProviderEntry {
            api: Some("responses".to_string()),
            ..qwen_entry()
        };
        let config = user_config("qwen", entry);
        let provider = build_provider_with_user(&model_config("qwen"), &config)
            .expect("qwen api = 'responses' must build");
        assert_eq!(provider.name(), "qwen-responses");
    }

    #[test]
    fn test_qwen_branch_applies_base_url_to_responses_adapter() {
        let entry = ProviderEntry {
            api: Some("responses".to_string()),
            base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
            ..qwen_entry()
        };
        let config = user_config("qwen", entry);
        let provider = build_provider_with_user(&model_config("qwen"), &config)
            .expect("qwen api = 'responses' must build");
        assert_eq!(provider.name(), "qwen-responses");
    }

    #[test]
    fn test_qwen_branch_rejects_unknown_api_mode() {
        let entry = ProviderEntry {
            api: Some("native".to_string()),
            ..qwen_entry()
        };
        let config = user_config("qwen", entry);
        let err = build_provider_with_user(&model_config("qwen"), &config)
            .err()
            .expect("unknown api mode must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("unsupported api mode 'native'"),
            "unexpected message: {msg}"
        );
    }

    fn openai_entry() -> ProviderEntry {
        ProviderEntry {
            provider_type: "openai".to_string(),
            api_key: Some("sk-test-openai".to_string()),
            base_url: None,
            api: None,
            default: None,
        }
    }

    /// 未配置 `api` 时默认走 chat completions（存量默认不变更，兼容网关也走 chat）。
    #[test]
    fn test_openai_branch_defaults_to_chat_adapter() {
        let entry = openai_entry();
        assert!(entry.api.is_none());
        let config = user_config("openai", entry);
        let provider =
            build_provider_with_user(&model_config("openai"), &config).expect("openai must build");
        assert_eq!(provider.name(), "openai");
    }

    #[test]
    fn test_openai_branch_builds_chat_adapter() {
        let entry = ProviderEntry {
            api: Some("chat".to_string()),
            ..openai_entry()
        };
        let config = user_config("openai", entry);
        let provider = build_provider_with_user(&model_config("openai"), &config)
            .expect("openai api = 'chat' must build");
        assert_eq!(provider.name(), "openai");
    }

    #[test]
    fn test_openai_branch_applies_base_url() {
        let entry = ProviderEntry {
            api: Some("chat".to_string()),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            ..openai_entry()
        };
        let config = user_config("openai", entry);
        let provider = build_provider_with_user(&model_config("openai"), &config)
            .expect("openai with base_url must build");
        assert_eq!(provider.name(), "openai");
    }

    #[test]
    fn test_openai_branch_builds_responses_adapter() {
        let entry = ProviderEntry {
            api: Some("responses".to_string()),
            ..openai_entry()
        };
        let config = user_config("openai", entry);
        let provider = build_provider_with_user(&model_config("openai"), &config)
            .expect("openai api = 'responses' must build");
        assert_eq!(provider.name(), "openai-responses");
    }

    #[test]
    fn test_openai_branch_applies_base_url_to_responses_adapter() {
        let entry = ProviderEntry {
            api: Some("responses".to_string()),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            ..openai_entry()
        };
        let config = user_config("openai", entry);
        let provider = build_provider_with_user(&model_config("openai"), &config)
            .expect("openai api = 'responses' must build");
        assert_eq!(provider.name(), "openai-responses");
    }

    #[test]
    fn test_openai_branch_rejects_unknown_api_mode() {
        let entry = ProviderEntry {
            api: Some("native".to_string()),
            ..openai_entry()
        };
        let config = user_config("openai", entry);
        let err = build_provider_with_user(&model_config("openai"), &config)
            .err()
            .expect("unknown api mode must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("unsupported api mode 'native'"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_unknown_provider_type_lists_supported() {
        let entry = ProviderEntry {
            provider_type: "groq".to_string(),
            api_key: Some("sk-test".to_string()),
            base_url: None,
            api: None,
            default: None,
        };
        let config = user_config("groq", entry);
        let err = build_provider_with_user(&model_config("groq"), &config)
            .err()
            .expect("unknown provider type must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("Currently supported: deepseek, qwen, openai"),
            "unexpected message: {msg}"
        );
    }
}
