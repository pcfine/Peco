// ============================================================================
// UserConfig — 用户级配置，与系统配置做深递归合并
// ============================================================================

use std::path::Path;

use super::mcp_config::McpConfig;
use super::merge::merge_providers_config;
use super::system_config::SystemConfig;
use super::types::{ProviderEntry, ProvidersConfig};
use crate::config::ConfigError;

/// 用户级配置 — providers.toml 深递归合并结果 + MCP 配置。
///
/// 在 Workspace 打开时从 `{workspace_root}/providers.toml` 和
/// `{workspace_root}/mcpconfig.json` 加载，并与系统配置做深递归合并。
#[derive(Debug, Clone)]
pub struct UserConfig {
    /// 合并后的 provider 配置。
    pub providers: ProvidersConfig,
    /// 用户级 MCP 配置（若存在则完全替代系统级）。
    pub mcp: McpConfig,
}

impl UserConfig {
    /// 加载用户配置并与系统配置做深递归合并。
    ///
    /// 若 workspace_root 下不存在 providers.toml 或 mcpconfig.json，
    /// 则以系统配置为基准（api_key 由用户在运行时通过环境变量提供）。
    ///
    /// # 加载流程
    ///
    /// 1. 尝试读取 `{workspace_root}/providers.toml`
    /// 2. 若存在，与 `system.providers` 做深递归合并
    /// 3. 若不存在，直接使用 `system.providers`
    /// 4. 尝试读取 `{workspace_root}/mcpconfig.json`
    /// 5. 若存在，使用用户 MCP 配置；否则使用系统兜底
    pub fn load(system: &SystemConfig, workspace_root: &Path) -> Result<Self, ConfigError> {
        // ── 加载用户 providers.toml ──────────────────────────────
        let user_providers_path = workspace_root.join("providers.toml");

        let providers = if user_providers_path.exists() {
            let content = std::fs::read_to_string(&user_providers_path).map_err(ConfigError::Io)?;
            let user_providers: ProvidersConfig =
                toml::from_str(&content).map_err(ConfigError::TomlParse)?;
            merge_providers_config(&system.providers, &user_providers)
        } else {
            system.providers.clone()
        };

        // ── 加载用户 mcpconfig.json ──────────────────────────────
        let user_mcp_path = workspace_root.join("mcpconfig.json");

        let mcp = if user_mcp_path.exists() {
            let content = std::fs::read_to_string(&user_mcp_path).map_err(ConfigError::Io)?;
            serde_json::from_str(&content).map_err(ConfigError::JsonParse)?
        } else {
            system.mcp.clone()
        };

        Ok(Self { providers, mcp })
    }

    /// 查找指定 provider 的配置条目。
    ///
    /// 若 `name` 为 `None`，返回默认 provider 的配置。
    pub fn provider_entry(&self, name: Option<&str>) -> Option<&ProviderEntry> {
        let key = name.unwrap_or(&self.providers.default_provider);
        self.providers.providers.get(key)
    }

    /// 返回默认 provider 名称。
    pub fn default_provider_name(&self) -> &str {
        &self.providers.default_provider
    }
}
