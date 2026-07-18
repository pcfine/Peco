// ============================================================================
// GlobalConfig — 全局配置容器，聚合项目中的所有配置项
// ============================================================================
//
// GlobalConfig 是项目配置的顶层聚合结构，由 [`GlobalHandler`](crate::GlobalHandler)
// 持有并作为单例提供。各模块通过 `GlobalHandler::global().config()` 访问配置。
//
// # 扩展方式
//
// 新增配置项时：
// 1. 在本结构体中添加对应的配置字段
// 2. 在 [`GlobalConfig::load`] 中加载新配置（含默认值降级逻辑）
// 3. 添加对应的访问器方法
//
// # Example
//
// ```ignore
// use peco_core::GlobalHandler;
//
// let config = GlobalHandler::global().config();
// let provider_name = config.default_provider_name();
// let mcp_servers = config.mcp_config().enabled_servers();
// ```

use std::collections::HashMap;

use super::loader::load_config;
use super::mcp_config::McpConfig;
use super::types::{LlmApiParams, ProviderEntry, ProvidersConfig};
use crate::knowledge::KnowledgeConfig;

// ── GlobalConfig ─────────────────────────────────────────────────────────────

/// 全局配置容器。
///
/// 聚合 `providers.toml`、`skills/`、`mcpconfig.json` 等所有配置源。
/// 由 [`GlobalHandler`](crate::GlobalHandler) 在启动时一次性加载。
///
/// 配置在运行时**不可变** — 如需热加载，需要通过其他机制（如文件监听）
/// 重建整个 [`GlobalConfig`] 实例。
#[derive(Debug, Clone)]
pub struct GlobalConfig {
    /// provider 配置（来自 `providers.toml`）
    provider_config: ProvidersConfig,

    /// MCP 配置（来自 `mcpconfig.json` 或 `PECO_MCP_CONFIG` 环境变量）
    mcp_config: McpConfig,

    /// 知识库模块配置
    knowledge_config: KnowledgeConfig,
    // 未来扩展字段：
    // skills_config: SkillsConfig,
    // server_config: ServerConfig,
    // logging_config: LoggingConfig,
}

impl GlobalConfig {
    // ── 构造 ────────────────────────────────────────────────────────────────

    /// 加载全局配置。
    ///
    /// 按优先级尝试从标准路径加载各项配置，加载失败时使用内置默认值。
    ///
    /// # Panics
    ///
    /// 当前不会 panic — 所有配置加载失败均有默认降级。
    pub fn load() -> Self {
        let provider_config = load_config(None).unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                "Failed to load providers.toml, falling back to built-in defaults"
            );
            let mut providers = HashMap::new();
            providers.insert("deepseek".to_string(), ProviderEntry {
                provider_type: "deepseek".to_string(),
                api_key: None,
                base_url: None,
                default: Some(LlmApiParams {
                    model: "deepseek-v4-flash".to_string(),
                    temperature: Some(0.7),
                    max_tokens: Some(4096),
                    stream: Some(true),
                    reasoning_effort: None,
                }),
            });
            ProvidersConfig {
                default_provider: "deepseek".to_string(),
                providers,
            }
        });

        let mcp_config = McpConfig::load().unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                "Failed to load MCP config, falling back to empty config"
            );
            McpConfig::empty()
        });

        let knowledge_config = KnowledgeConfig::load().unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                "Failed to load knowledge config, falling back to defaults"
            );
            KnowledgeConfig::default()
        });

        Self {
            provider_config,
            mcp_config,
            knowledge_config,
        }
    }

    // ── Provider 配置访问器 ─────────────────────────────────────────────────

    /// 返回完整的 Provider 配置的引用。
    pub fn providers(&self) -> &ProvidersConfig {
        &self.provider_config
    }

    /// 返回默认 provider 名称。
    pub fn default_provider_name(&self) -> &str {
        &self.provider_config.default_provider
    }

    /// 检查指定名称的 provider 是否已配置。
    pub fn has_provider(&self, name: &str) -> bool {
        self.provider_config.providers.contains_key(name)
    }

    /// 返回所有已配置的 provider 名称（排序后）。
    pub fn provider_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .provider_config
            .providers
            .keys()
            .map(|s| s.as_str())
            .collect();
        names.sort();
        names
    }

    /// 查找指定 provider 的配置条目。
    ///
    /// 若 `name` 为 `None`，返回默认 provider 的配置。
    pub fn provider_entry(&self, name: Option<&str>) -> Option<&super::types::ProviderEntry> {
        let key = name.unwrap_or(&self.provider_config.default_provider);
        self.provider_config.providers.get(key)
    }

    // ── MCP 配置访问器 ─────────────────────────────────────────────────────

    /// 返回完整的 MCP 配置的引用。
    ///
    /// 通过此方法可以访问所有已配置的 MCP 服务器信息：
    ///
    /// ```ignore
    /// let config = GlobalHandler::global().config();
    /// for (name, server) in config.mcp_config().enabled_servers() {
    ///     println!("MCP server: {name} ({:?})", server.transport);
    /// }
    /// ```
    pub fn mcp_config(&self) -> &McpConfig {
        &self.mcp_config
    }

    /// 返回所有启用的 MCP 服务器配置。
    ///
    /// 便捷方法，等效于 `self.mcp_config().enabled_servers()`。
    pub fn mcp_servers(&self) -> Vec<(String, &super::mcp_config::McpServerConfig)> {
        self.mcp_config.enabled_servers()
    }

    /// 检查是否配置了任何 MCP 服务器。
    pub fn has_mcp_servers(&self) -> bool {
        !self.mcp_config.mcp_servers.is_empty()
    }

    /// 返回 MCP 服务器数量（包括禁用的）。
    pub fn mcp_server_count(&self) -> usize {
        self.mcp_config.mcp_servers.len()
    }

    // ── 知识库配置访问器 ─────────────────────────────────────────────────────

    /// 返回知识库模块配置的引用。
    pub fn knowledge_config(&self) -> &KnowledgeConfig {
        &self.knowledge_config
    }
}
