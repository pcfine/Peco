// ============================================================================
// Config 数据类型 — providers.toml 的解析结构
// ============================================================================

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── providers.toml 顶层结构 ───────────────────────────────────────────────────

/// 完整的 providers.toml 解析结果。
///
/// # Example
///
/// ```toml
/// default_provider = "deepseek"
///
/// [providers.deepseek]
/// type = "deepseek"
/// api_key = "${DEEPSEEK_API_KEY}"
///
/// [providers.deepseek.default]
/// model = "deepseek-v4-flash"
/// temperature = 0.7
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProvidersConfig {
    /// 默认 provider 名称(如 "deepseek")，必须匹配 `providers` 中的某个 key。
    #[serde(default = "default_provider_name")]
    pub default_provider: String,

    /// provider 逻辑名 → 配置条目 的映射。
    /// key 是用户自定义的简称（如 "deepseek", "openai"）。
    #[serde(default)]
    pub providers: HashMap<String, ProviderEntry>,
}

fn default_provider_name() -> String {
    "deepseek".to_string()
}

// ── 单个 Provider 配置 ────────────────────────────────────────────────────────

/// 单个 provider 的 API 凭据、base URL 和默认模型参数。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderEntry {
    /// Provider 类型标识：`"deepseek"`, `"openai"`, `"anthropic"`, `"ollama"`, `"groq"`。
    #[serde(rename = "type")]
    pub provider_type: String,

    /// API Key：可以是字面量，或以 `${ENV_VAR}` 语法引用环境变量。
    /// Ollama 可不提供 API Key。
    #[serde(default)]
    pub api_key: Option<String>,

    /// 自定义 API base URL（可选，不填则用 provider 默认值）。
    #[serde(default)]
    pub base_url: Option<String>,

    /// 该 provider 的默认模型和参数。
    #[serde(default)]
    pub default: Option<LlmApiParams>,
}

/// Provider 级别的默认模型配置。
///
/// `model` 为必填 — 每个 provider 必须指定一个默认模型名。
/// 其余字段可选，agent.md 可以按需覆盖。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LlmApiParams {
    /// 默认模型名称 / ID（必填）。
    pub model: String,

    /// 默认采样温度（可选）。
    #[serde(default)]
    pub temperature: Option<f64>,

    /// 默认最大输出 token 数（可选）。
    #[serde(default)]
    pub max_tokens: Option<u64>,

    /// 是否默认启用流式输出（可选）。
    #[serde(default)]
    pub stream: Option<bool>,

    /// 默认推理力度（可选，如 DeepSeek 的 `thinking` 或 Anthropic 的 `reasoning_effort`）。
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

// ── 环境变量解析 ──────────────────────────────────────────────────────────────

/// 解析 API Key 字符串，支持 `${ENV_VAR}` 环境变量语法。
///
/// - `"${OPENAI_API_KEY}"` → 查找环境变量 `OPENAI_API_KEY`
/// - `"sk-abc123"` → 原样返回
pub fn resolve_key(raw: &str) -> Result<String, std::env::VarError> {
    if let Some(inner) = raw.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        std::env::var(inner)
    } else {
        Ok(raw.to_string())
    }
}
