// ============================================================================
// TOML 配置文件反序列化结构 & API Key 解析
// ============================================================================

use crate::agent::error::AgentError;
use crate::config::LlmApiParams;

// ============================================================================
// Agent Markdown 配置（agent.md）— 新格式
// ============================================================================

/// 从 agent.md 的 YAML frontmatter 中解析出的完整配置。
///
/// # Example
///
/// ```yaml
/// ---
/// agent:
///   name: "my-agent"
///   description: "A helpful agent"
/// llm:
///   provider: "deepseek"
///   model: "deepseek-v4-flash"
/// tools:
///   - shell
/// ---
/// # System Prompt
/// You are a helpful assistant.
/// ```
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AgentProfile {
    /// Agent 身份信息（name + description）。
    pub agent: AgentIdentity,

    /// LLM provider 和模型配置。全部可选 — 未指定时从 provider 默认配置取默认值。
    #[serde(default)]
    pub llm: Option<LlmConfig>,

    /// 工具白名单（ToolManager 中的工具名）。
    #[serde(default)]
    pub tools: Vec<String>,

    /// MCP 服务器名称列表。
    #[serde(default)]
    pub mcp: Vec<String>,

    /// Skill 名称列表（SkillRegister 中的 skill）。
    #[serde(default)]
    pub skills: Vec<String>,

    /// 单次响应最大对话轮数。未配置时默认为 20。
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
}

/// `max_turns` 的默认值：20 轮对话。
pub fn default_max_turns() -> usize {
    20
}

/// Agent 身份信息。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AgentIdentity {
    /// Agent 名称（全局唯一标识）。
    pub name: String,

    /// Agent 功能描述（用于调度匹配）。
    pub description: String,
}

/// agent.md 中的 LLM 配置段。
///
/// 所有字段均为 `Option` — 未指定的字段从 providers.toml 的 provider 默认值中获取。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct LlmConfig {
    /// Provider 逻辑名（如 "deepseek", "openai"）。`None` 时使用 `default_provider`。
    #[serde(default)]
    pub provider: Option<String>,

    /// 模型名。`None` 时使用 provider 的 `defaults.model`。
    #[serde(default)]
    pub model: Option<String>,

    /// 采样温度（可选）。
    #[serde(default)]
    pub temperature: Option<f64>,

    /// 最大输出 token 数（可选）。
    #[serde(default)]
    pub max_tokens: Option<u64>,

    /// 是否启用流式输出（可选）。
    #[serde(default)]
    pub stream: Option<bool>,

    /// 推理力度（可选，如 "low", "medium", "high"）。
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

// ── ModelConfig — Agent 级别的模型参数配置 ───────────────────────────────────────

/// Agent 请求的模型配置，用于覆盖 provider 默认值。
///
/// 所有字段均为 `Option` — `None` 表示使用 provider 默认值。
/// 通过 [`ModelConfigBuilder`] 构造。
#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub provider_name: Option<String>,
    pub model_name: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<usize>,
    pub stream: Option<bool>,
    pub reasoning_effort: Option<String>,
}

impl ModelConfig {
    /// 合并 provider 默认值：`ModelConfig` 中为 `None` 的字段使用 `defaults` 中的值。
    ///
    /// 若 `defaults` 为 `None`，原样返回 clone。
    pub fn merge_defaults(&self, defaults: Option<&LlmApiParams>) -> Self {
        let Some(defaults) = defaults else {
            return self.clone();
        };
        Self {
            provider_name: self.provider_name.clone(),
            model_name: self
                .model_name
                .clone()
                .or_else(|| Some(defaults.model.clone())),
            temperature: self.temperature.or(defaults.temperature.map(|t| t as f32)),
            max_tokens: self.max_tokens.or(defaults.max_tokens.map(|t| t as usize)),
            stream: self.stream.or(defaults.stream),
            reasoning_effort: self
                .reasoning_effort
                .clone()
                .or_else(|| defaults.reasoning_effort.clone()),
        }
    }
}

/// [`ModelConfig`] 的 Builder 模式构造器。
///
/// # Example
///
/// ```ignore
/// let config = ModelConfigBuilder::new()
///     .provider_name("deepseek")
///     .model_name("deepseek-v4-pro")
///     .temperature(0.7)
///     .stream(true)
///     .build();
/// ```
#[derive(Debug, Clone)]
pub struct ModelConfigBuilder {
    provider_name: Option<String>,
    model_name: Option<String>,
    temperature: Option<f32>,
    max_tokens: Option<usize>,
    stream: Option<bool>,
    reasoning_effort: Option<String>,
}

impl ModelConfigBuilder {
    /// 创建一个空的 Builder。
    pub fn new() -> Self {
        Self {
            provider_name: None,
            model_name: None,
            temperature: None,
            max_tokens: None,
            stream: None,
            reasoning_effort: None,
        }
    }

    /// 设置 provider 名称（如 `"deepseek"`, `"openai"`）。
    pub fn provider_name(mut self, name: impl Into<String>) -> Self {
        self.provider_name = Some(name.into());
        self
    }

    /// 设置模型名称（如 `"deepseek-v4-pro"`）。
    pub fn model_name(mut self, name: impl Into<String>) -> Self {
        self.model_name = Some(name.into());
        self
    }

    /// 设置采样温度（0.0 ~ 2.0）。
    pub fn temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }

    /// 设置最大输出 token 数。
    pub fn max_tokens(mut self, tokens: usize) -> Self {
        self.max_tokens = Some(tokens);
        self
    }

    /// 设置是否启用流式输出。
    pub fn stream(mut self, stream: bool) -> Self {
        self.stream = Some(stream);
        self
    }

    /// 设置推理力度（如 DeepSeek 的 `thinking` 或 Anthropic 的 `reasoning_effort`）。
    pub fn reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    /// 消费 Builder 并构建 [`ModelConfig`]。
    pub fn build(self) -> ModelConfig {
        ModelConfig {
            provider_name: self.provider_name,
            model_name: self.model_name,
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            stream: self.stream,
            reasoning_effort: self.reasoning_effort,
        }
    }
}

impl Default for ModelConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// agent.md 序列化 — 从 flat 参数组装 YAML + Markdown
// ============================================================================

/// [`assemble_agent_md`] 的输入参数。
///
/// 包含 agent.md 文件所需的全部字段。
/// 所有字段均为 flat 形式（非嵌套），与 API 请求体结构对应。
pub struct AssembleAgentMdParams {
    pub name: String,
    pub description: String,
    pub provider: String,
    pub model: String,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub stream: Option<bool>,
    pub reasoning_effort: Option<String>,
    pub tools: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub skills: Vec<String>,
    pub max_turns: usize,
    pub system_prompt: String,
}

/// 将 flat 参数组装为 agent.md 文件内容（YAML frontmatter + Markdown body）。
///
/// 输出格式与 [`AgentProfile`] 的 YAML 反序列化兼容，
/// 保证 `parse_agent_md(assemble_agent_md(params))` 可以 round-trip。
///
/// # Example
///
/// ```ignore
/// let params = AssembleAgentMdParams {
///     name: "my-agent".into(),
///     description: "A helpful agent".into(),
///     provider: "deepseek".into(),
///     model: "deepseek-v4-flash".into(),
///     temperature: None,
///     max_tokens: None,
///     stream: None,
///     reasoning_effort: None,
///     tools: vec!["shell".into()],
///     mcp_servers: vec![],
///     skills: vec![],
///     max_turns: 20,
///     system_prompt: "You are a helpful assistant.".into(),
/// };
/// let md = assemble_agent_md(&params);
/// std::fs::write("agent.md", md)?;
/// ```
pub fn assemble_agent_md(params: &AssembleAgentMdParams) -> String {
    let mut yaml = String::from("---\n");

    // agent 段（必填）
    yaml.push_str("agent:\n");
    yaml.push_str(&format!("  name: \"{}\"\n", yaml_escape(&params.name)));
    yaml.push_str(&format!(
        "  description: \"{}\"\n",
        yaml_escape(&params.description)
    ));

    // llm 段
    yaml.push_str("llm:\n");
    yaml.push_str(&format!(
        "  provider: \"{}\"\n",
        yaml_escape(&params.provider)
    ));
    yaml.push_str(&format!("  model: \"{}\"\n", yaml_escape(&params.model)));
    if let Some(t) = params.temperature {
        yaml.push_str(&format!("  temperature: {}\n", t));
    }
    if let Some(m) = params.max_tokens {
        yaml.push_str(&format!("  max_tokens: {}\n", m));
    }
    if let Some(s) = params.stream {
        yaml.push_str(&format!("  stream: {}\n", s));
    }
    if let Some(ref e) = params.reasoning_effort {
        yaml.push_str(&format!("  reasoning_effort: \"{}\"\n", yaml_escape(e)));
    }

    // tools（加引号防止 YAML 保留字如 true/false/null 被误解析）
    if !params.tools.is_empty() {
        yaml.push_str("tools:\n");
        for t in &params.tools {
            yaml.push_str(&format!("  - \"{}\"\n", yaml_escape(t)));
        }
    }

    // mcp
    if !params.mcp_servers.is_empty() {
        yaml.push_str("mcp:\n");
        for m in &params.mcp_servers {
            yaml.push_str(&format!("  - \"{}\"\n", yaml_escape(m)));
        }
    }

    // skills
    if !params.skills.is_empty() {
        yaml.push_str("skills:\n");
        for s in &params.skills {
            yaml.push_str(&format!("  - \"{}\"\n", yaml_escape(s)));
        }
    }

    // max_turns（只在非默认值时写出，保持最小惊讶原则）
    if params.max_turns != 20 {
        yaml.push_str(&format!("max_turns: {}\n", params.max_turns));
    }

    yaml.push_str("---\n");
    yaml.push_str(&params.system_prompt);

    yaml
}

/// YAML 字符串值转义：包含特殊字符时加双引号。
///
/// 当前实现总是加引号（与 `assemble_agent_md` 的输出格式一致）。
fn yaml_escape(s: &str) -> String {
    // 简单实现：转义内部双引号和反斜杠
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 从 agent.md 文件内容解析出 [`AgentProfile`] 和 Markdown body。
///
/// 封装了 [`split_frontmatter`] + `serde_yaml::from_str`。
///
/// # Errors
///
/// - frontmatter 格式不正确
/// - YAML 解析失败
pub fn parse_agent_md(content: &str) -> Result<(AgentProfile, String), AgentError> {
    let (frontmatter_str, body) =
        split_frontmatter(content).map_err(AgentError::InvalidFrontmatter)?;
    let profile: AgentProfile = serde_yaml::from_str(frontmatter_str)?;
    Ok((profile, body.to_string()))
}

/// 解析 API Key 字符串，支持 `${ENV_VAR}` 环境变量语法。
///
/// - `"${OPENAI_API_KEY}"` → 查找环境变量 `OPENAI_API_KEY`
/// - `"sk-abc123"` → 原样返回
pub fn resolve_api_key(raw: &str) -> Result<String, AgentError> {
    if let Some(inner) = raw.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        std::env::var(inner).map_err(|e| AgentError::EnvVar(inner.to_string(), e))
    } else {
        Ok(raw.to_string())
    }
}

// ============================================================================
// Frontmatter 解析 — 供 agent.md 和 SKILL.md 共用
// ============================================================================

/// 将 Markdown-with-YAML-frontmatter 文件内容拆分为 `(yaml_str, body_str)`。
///
/// 文件必须以 `---` 开头。第一对 `---` 之间的内容为 YAML frontmatter，
/// 之后的内容为 Markdown body。
///
/// # Errors
///
/// - 文件不以 `---` 开头
/// - 缺少闭合的 `---`
pub fn split_frontmatter(raw: &str) -> Result<(&str, &str), String> {
    let trimmed_start = raw.trim_start();
    if !trimmed_start.starts_with("---") {
        return Err("file must start with '---' frontmatter delimiter".into());
    }

    // 跳过开头的 `---`
    let after_open = &trimmed_start[3..];
    // 跳过 `---` 后的换行符
    let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);

    // 查找闭合的 `---`
    let closing_pos = after_open.find("\n---").or_else(|| {
        // 边界情况：闭合 `---` 可能在末尾（无尾随换行）
        if after_open.starts_with("---") {
            Some(0)
        } else {
            None
        }
    });

    match closing_pos {
        Some(pos) => {
            let frontmatter_str = &after_open[..pos];
            let remainder = &after_open[pos..];
            // 跳过闭合的 `---` 行
            let body = remainder
                .strip_prefix("\n---")
                .or_else(|| remainder.strip_prefix("---"))
                .unwrap_or(remainder);
            // 去除 body 开头的换行符
            let body = body.strip_prefix('\n').unwrap_or(body);
            Ok((frontmatter_str.trim(), body))
        }
        None => Err("missing closing '---' frontmatter delimiter".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_api_key_literal() {
        let result = resolve_api_key("sk-abc123").unwrap();
        assert_eq!(result, "sk-abc123");
    }

    #[test]
    fn test_resolve_api_key_env_var() {
        unsafe { std::env::set_var("TEST_API_KEY_FOR_CONFIG", "my-secret-key") };
        let result = resolve_api_key("${TEST_API_KEY_FOR_CONFIG}").unwrap();
        assert_eq!(result, "my-secret-key");
    }

    #[test]
    fn test_resolve_api_key_missing_env() {
        let result = resolve_api_key("${MISSING_VAR_12345}");
        assert!(result.is_err());
        if let Err(AgentError::EnvVar(name, _)) = result {
            assert_eq!(name, "MISSING_VAR_12345");
        } else {
            panic!("expected EnvVar error");
        }
    }

    // ── split_frontmatter ───────────────────────────────────────────────

    #[test]
    fn test_split_frontmatter_valid() {
        let raw = "---\nname: test\ndescription: A test\n---\n\n# Hello\n\nSome body text.";
        let (fm, body) = split_frontmatter(raw).unwrap();
        assert_eq!(fm, "name: test\ndescription: A test");
        assert!(body.contains("# Hello"));
        assert!(!body.contains("---"));
    }

    #[test]
    fn test_split_frontmatter_missing_opening() {
        let raw = "name: test\n---\nbody";
        assert!(split_frontmatter(raw).is_err());
    }

    #[test]
    fn test_split_frontmatter_missing_closing() {
        let raw = "---\nname: test\nbody without closing";
        assert!(split_frontmatter(raw).is_err());
    }

    #[test]
    fn test_split_frontmatter_empty_body() {
        let raw = "---\nname: minimal\ndescription: Minimal\n---\n";
        let (fm, body) = split_frontmatter(raw).unwrap();
        assert_eq!(fm, "name: minimal\ndescription: Minimal");
        assert!(body.is_empty() || body == "");
    }

    // ── assemble_agent_md / parse_agent_md round-trip ────────────────────

    #[test]
    fn test_assemble_and_parse_roundtrip_full() {
        let params = AssembleAgentMdParams {
            name: "test-agent".into(),
            description: "A test agent for round-trip verification".into(),
            provider: "deepseek".into(),
            model: "deepseek-v4-pro".into(),
            temperature: Some(0.7),
            max_tokens: Some(4096),
            stream: Some(true),
            reasoning_effort: Some("high".into()),
            tools: vec!["shell".into(), "fetch".into()],
            mcp_servers: vec!["filesystem".into()],
            skills: vec!["code-review".into()],
            max_turns: 30,
            system_prompt: "You are a helpful assistant.\nBe concise.".into(),
        };

        let md = assemble_agent_md(&params);
        let (profile, body) = parse_agent_md(&md).expect("round-trip parse should succeed");

        // 验证 identity
        assert_eq!(profile.agent.name, "test-agent");
        assert_eq!(
            profile.agent.description,
            "A test agent for round-trip verification"
        );

        // 验证 llm 配置
        let llm = profile.llm.expect("llm section should be present");
        assert_eq!(llm.provider.as_deref(), Some("deepseek"));
        assert_eq!(llm.model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(llm.temperature, Some(0.7));
        assert_eq!(llm.max_tokens, Some(4096));
        assert_eq!(llm.stream, Some(true));
        assert_eq!(llm.reasoning_effort.as_deref(), Some("high"));

        // 验证列表
        assert_eq!(profile.tools, vec!["shell", "fetch"]);
        assert_eq!(profile.mcp, vec!["filesystem"]);
        assert_eq!(profile.skills, vec!["code-review"]);

        // 验证 max_turns 和 body
        assert_eq!(profile.max_turns, 30);
        assert_eq!(body, "You are a helpful assistant.\nBe concise.");
    }

    #[test]
    fn test_assemble_and_parse_roundtrip_minimal() {
        let params = AssembleAgentMdParams {
            name: "minimal".into(),
            description: "Minimal config".into(),
            provider: "deepseek".into(),
            model: "deepseek-v4-flash".into(),
            temperature: None,
            max_tokens: None,
            stream: None,
            reasoning_effort: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            max_turns: 20, // 默认值，不应写入
            system_prompt: "Be helpful.".into(),
        };

        let md = assemble_agent_md(&params);
        let (profile, body) = parse_agent_md(&md).expect("round-trip parse should succeed");

        assert_eq!(profile.agent.name, "minimal");
        assert_eq!(profile.agent.description, "Minimal config");

        let llm = profile.llm.expect("llm section should be present");
        assert_eq!(llm.provider.as_deref(), Some("deepseek"));
        assert_eq!(llm.model.as_deref(), Some("deepseek-v4-flash"));
        assert!(llm.temperature.is_none());
        assert!(llm.max_tokens.is_none());
        assert!(llm.stream.is_none());
        assert!(llm.reasoning_effort.is_none());

        assert!(profile.tools.is_empty());
        assert!(profile.mcp.is_empty());
        assert!(profile.skills.is_empty());
        assert_eq!(profile.max_turns, 20);
        assert_eq!(body, "Be helpful.");
    }

    #[test]
    fn test_assemble_agent_md_contains_expected_structure() {
        let params = AssembleAgentMdParams {
            name: "struct-test".into(),
            description: "Test structure".into(),
            provider: "openai".into(),
            model: "gpt-4".into(),
            temperature: None,
            max_tokens: None,
            stream: None,
            reasoning_effort: None,
            tools: vec!["shell".into()],
            mcp_servers: vec![],
            skills: vec![],
            max_turns: 20,
            system_prompt: "You are helpful.".into(),
        };

        let md = assemble_agent_md(&params);

        // 必须以 --- 开头
        assert!(md.starts_with("---\n"), "agent.md must start with ---");

        // 必须包含闭合的 ---
        let parts: Vec<&str> = md.splitn(3, "---").collect();
        assert!(parts.len() >= 3, "must have opening and closing ---");

        // body 必须存在
        assert!(md.contains("You are helpful."), "body must be present");
    }

    #[test]
    fn test_parse_agent_md_invalid_yaml() {
        let bad = "---\nagent: { invalid yaml!!!\n---\nbody";
        let result = parse_agent_md(bad);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_agent_md_missing_frontmatter() {
        let bad = "no frontmatter here\njust text";
        let result = parse_agent_md(bad);
        assert!(result.is_err());
    }
}
