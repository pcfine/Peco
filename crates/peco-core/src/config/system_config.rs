// ============================================================================
// SystemConfig — 进程级共享配置（所有 WorkSpace 共享）
// ============================================================================
//
// 这是进程级共享配置，所有 WorkSpace 共享。
// 不包含任何用户数据（api_key 为空）。

use std::collections::HashMap;
use std::path::PathBuf;

use super::mcp_config::McpConfig;
use super::types::{LlmApiParams, ProviderEntry, ProvidersConfig};

/// 系统级配置 — 进程生命周期内不变，所有 WorkSpace 共享。
///
/// 包含 providers.toml 的基础配置（api_key 为空）、MCP 服务器注册表、
/// skills 根目录和知识库根目录。
#[derive(Debug, Clone)]
pub struct SystemConfig {
    /// 系统 providers.toml 的基础配置（base_url, provider_type）。
    /// api_key 为空，由用户在 workspace 层覆盖。
    pub providers: ProvidersConfig,
    /// 系统级 MCP 服务器注册表（用户未配置时的兜底）。
    pub mcp: McpConfig,
    /// 系统 skills 根目录。
    pub skills_root: PathBuf,
    /// 知识库根目录。
    pub knowledge_dir: PathBuf,
}

impl SystemConfig {
    /// 从默认路径加载系统配置。
    ///
    /// 加载：
    /// - `~/.config/peco/providers.toml`（或 `PECO_PROVIDERS_CONFIG` 环境变量）
    /// - `~/.config/peco/mcpconfig.json`（或 `PECO_MCP_CONFIG` 环境变量）
    /// - skills 根目录（`PECO_SKILLS_ROOT` 或 `./skills`）
    /// - knowledge 目录（默认 `~/.peco/knowledge`）
    pub fn load() -> Self {
        let providers = super::loader::load_config(None).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to load providers.toml, using built-in fallback");
            let mut providers = HashMap::new();
            providers.insert(
                "deepseek".to_string(),
                ProviderEntry {
                    provider_type: "deepseek".to_string(),
                    api_key: None,
                    base_url: Some("https://api.deepseek.com".to_string()),
                    api: None,
                    default: Some(LlmApiParams {
                        model: "deepseek-v4-flash".to_string(),
                        temperature: Some(0.7),
                        max_tokens: None,
                        stream: Some(true),
                        reasoning_effort: None,
                    }),
                },
            );
            ProvidersConfig {
                default_provider: "deepseek".into(),
                providers,
                web_search: None,
            }
        });

        let mcp = McpConfig::load().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to load MCP config, using empty config");
            McpConfig::empty()
        });

        let skills_root = resolve_skills_root();
        let knowledge_dir = resolve_knowledge_dir();

        Self {
            providers,
            mcp,
            skills_root,
            knowledge_dir,
        }
    }
}

/// 解析 skills 根目录路径。
fn resolve_skills_root() -> PathBuf {
    std::env::var("PECO_SKILLS_ROOT")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./skills"))
}

/// 解析 knowledge 根目录路径。
fn resolve_knowledge_dir() -> PathBuf {
    std::env::var("PECO_KNOWLEDGE_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".peco")
                .join("knowledge")
        })
}
