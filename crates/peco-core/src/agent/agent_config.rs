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

    /// Skill 名称列表（GlobalSkillList 中的 skill）。
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
}
