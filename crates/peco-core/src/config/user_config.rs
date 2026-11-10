// ============================================================================
// UserConfig — 用户级配置，与系统配置做深递归合并
// ============================================================================

use std::path::{Path, PathBuf};

use super::mcp_config::McpConfig;
use super::merge::merge_providers_config;
use super::system_config::SystemConfig;
use super::types::{ProviderEntry, ProvidersConfig};
use crate::config::ConfigError;

/// 每层能力根下的 provider 配置文件。
const PROVIDERS_FILE: &str = "providers.toml";

/// MCP 配置文件；它不分层，只读最高优先层的那一份。
const MCP_FILE: &str = "mcpconfig.json";

/// 顶层默认 provider 键。
const DEFAULT_PROVIDER_KEY: &str = "default_provider";

/// 用户级配置 — providers.toml 逐层深递归合并结果 + MCP 配置。
///
/// 在 WorkSpace 打开时从各层能力根下的 `providers.toml` 加载（低 → 高），
/// 与系统配置做深递归合并；`mcpconfig.json` 只取最高优先层。
#[derive(Debug, Clone)]
pub struct UserConfig {
    /// 合并后的 provider 配置。
    pub providers: ProvidersConfig,
    /// 用户级 MCP 配置（若存在则完全替代系统级）。
    pub mcp: McpConfig,
}

impl UserConfig {
    /// 单层加载：只有 `workspace_root` 一层用户文件，与系统配置深递归合并。
    ///
    /// 若 `workspace_root` 下不存在 providers.toml 或 mcpconfig.json，
    /// 则以系统配置为基准（api_key 由用户在运行时通过环境变量提供）。
    pub fn load(system: &SystemConfig, workspace_root: &Path) -> Result<Self, ConfigError> {
        Self::load_layered(system, &[workspace_root.to_path_buf()])
    }

    /// 多层加载：`layers` 按优先级从低到高，最后一层是 workspace 根。
    ///
    /// 每层读 `providers.toml` 与已累计的结果深合并，文件缺失的层跳过。
    /// 合并粒度是 provider 名下逐字段，高层的空缺字段保留低层值。
    ///
    /// `default_provider` 只在该层**显式写出**时生效：serde 会给缺失的键填
    /// 默认值，直接采纳会让每一层都用那份默认值顶掉低层设的默认。
    pub fn load_layered(system: &SystemConfig, layers: &[PathBuf]) -> Result<Self, ConfigError> {
        let providers = load_providers(system, layers)?;
        let mcp = match layers.last() {
            Some(root) => load_mcp(root, &system.mcp)?,
            None => system.mcp.clone(),
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

/// 低 → 高逐层合并各层的 `providers.toml`。
fn load_providers(
    system: &SystemConfig,
    layers: &[PathBuf],
) -> Result<ProvidersConfig, ConfigError> {
    let mut merged = system.providers.clone();

    for layer in layers {
        let path = layer.join(PROVIDERS_FILE);
        if !path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&path)?;
        // 先解析成通用 TOML 再转结构体：`default_provider` 是否「写了」只能从
        // 原始文档判断，结构体里缺失键与显式写出会得到同一个值。
        let document: toml::Value = toml::from_str(&content)?;
        let has_default = document.get(DEFAULT_PROVIDER_KEY).is_some();
        let layer_config: ProvidersConfig = document.try_into()?;

        let inherited_default = merged.default_provider.clone();
        let next = merge_providers_config(&merged, &layer_config);
        merged = ProvidersConfig {
            default_provider: if has_default {
                next.default_provider
            } else {
                inherited_default
            },
            ..next
        };
    }

    Ok(merged)
}

/// 读最高优先层的 `mcpconfig.json`，缺失则用系统兜底。
fn load_mcp(root: &Path, system_mcp: &McpConfig) -> Result<McpConfig, ConfigError> {
    let path = root.join(MCP_FILE);
    if !path.exists() {
        return Ok(system_mcp.clone());
    }
    let content = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&content)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LlmApiParams;
    use std::collections::HashMap;

    /// 系统层：一条 `deepseek`（无 key、有地址与模型），默认也是它。
    fn system_config() -> SystemConfig {
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
                    stream: None,
                    reasoning_effort: None,
                }),
            },
        );
        SystemConfig {
            providers: ProvidersConfig {
                default_provider: "deepseek".to_string(),
                providers,
                web_search: None,
            },
            mcp: McpConfig::empty(),
        }
    }

    /// 写一层 `providers.toml`，返回该层根。
    fn layer(dir: &Path, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(PROVIDERS_FILE), body).unwrap();
        dir.to_path_buf()
    }

    /// 全层都缺文件 ⇒ 结果就是系统配置（含默认 provider 与 MCP 兜底）。
    #[test]
    fn missing_files_fall_back_to_system_config() {
        let tmp = tempfile::tempdir().unwrap();
        let system = system_config();
        let missing = vec![
            tmp.path().join("teams/peco"),
            tmp.path().join("teams/research"),
        ];

        let config = UserConfig::load_layered(&system, &missing).unwrap();

        assert_eq!(config.default_provider_name(), "deepseek");
        assert_eq!(
            config.provider_entry(Some("deepseek")).unwrap().api_key,
            None
        );
        assert_eq!(
            config.providers.providers["deepseek"].base_url.as_deref(),
            Some("https://api.deepseek.com")
        );
        assert!(config.mcp.mcp_servers.is_empty());
    }

    /// 三层逐字段合并：高层的空缺字段保留低层值，新条目只出现在写它的层。
    #[test]
    fn layers_merge_provider_fields_one_by_one() {
        let tmp = tempfile::tempdir().unwrap();
        let peco = layer(
            &tmp.path().join("teams/peco"),
            r#"
[providers.deepseek]
type = "deepseek"
api_key = "sk-peco"

[providers.deepseek.default]
model = "peco-model"
"#,
        );
        let team = layer(
            &tmp.path().join("teams/research"),
            r#"
[providers.deepseek]
type = "deepseek"
base_url = "https://proxy.example.com/v1"

[providers.qwen]
type = "qwen"
api_key = "sk-qwen"
"#,
        );

        let config = UserConfig::load_layered(&system_config(), &[peco, team]).unwrap();

        let deepseek = &config.providers.providers["deepseek"];
        assert_eq!(deepseek.api_key.as_deref(), Some("sk-peco"), "来自 Peco 层");
        assert_eq!(
            deepseek.base_url.as_deref(),
            Some("https://proxy.example.com/v1"),
            "来自团队层"
        );
        assert_eq!(
            deepseek.default.as_ref().map(|d| d.model.as_str()),
            Some("peco-model"),
            "团队层没写模型，保留 Peco 层的"
        );
        assert_eq!(
            config.providers.providers["qwen"].api_key.as_deref(),
            Some("sk-qwen")
        );
        assert_eq!(
            config.providers.providers.len(),
            2,
            "两个名字各一条：系统层与 Peco 层各贡献一个不同的 key"
        );
    }

    /// 高层显式写出 `default_provider` ⇒ 接管默认位。
    #[test]
    fn written_default_provider_takes_over() {
        let tmp = tempfile::tempdir().unwrap();
        let peco = layer(
            &tmp.path().join("teams/peco"),
            "default_provider = \"peco-default\"\n\n[providers.peco-default]\ntype = \"deepseek\"\napi_key = \"sk-peco\"\n",
        );
        let team = layer(
            &tmp.path().join("teams/research"),
            "default_provider = \"qwen\"\n\n[providers.qwen]\ntype = \"qwen\"\napi_key = \"sk-qwen\"\n",
        );

        let config = UserConfig::load_layered(&system_config(), &[peco, team]).unwrap();

        assert_eq!(config.default_provider_name(), "qwen");
    }

    /// 高层没写 `default_provider` ⇒ 沿用低层结果，哪怕 serde 的默认值
    /// `deepseek` 恰好也是合并结果里的一个条目。
    #[test]
    fn unwritten_default_provider_keeps_the_lower_layer() {
        let tmp = tempfile::tempdir().unwrap();
        let peco = layer(
            &tmp.path().join("teams/peco"),
            r#"
default_provider = "peco-default"

[providers.peco-default]
type = "deepseek"
api_key = "sk-peco"

[providers.deepseek]
type = "deepseek"
api_key = "sk-plain"
"#,
        );
        let team = layer(
            &tmp.path().join("teams/research"),
            "[providers.qwen]\ntype = \"qwen\"\napi_key = \"sk-qwen\"\n",
        );

        let config = UserConfig::load_layered(&system_config(), &[peco, team]).unwrap();

        assert!(config.providers.providers.contains_key("deepseek"));
        assert_eq!(
            config.default_provider_name(),
            "peco-default",
            "没写 default_provider 的层不得用 serde 默认值顶掉低层的默认"
        );
    }

    /// MCP 只读最高优先层；该层没有文件则用系统兜底。
    #[test]
    fn mcp_comes_from_the_highest_layer() {
        let tmp = tempfile::tempdir().unwrap();
        let system = system_config();
        let peco = tmp.path().join("teams/peco");
        let team = tmp.path().join("teams/research");
        std::fs::create_dir_all(&peco).unwrap();
        std::fs::create_dir_all(&team).unwrap();

        let fallback = UserConfig::load_layered(&system, &[peco.clone(), team.clone()]).unwrap();
        assert!(fallback.mcp.mcp_servers.is_empty(), "无文件 ⇒ 系统兜底");

        std::fs::write(
            team.join(MCP_FILE),
            r#"{"mcpServers": {"fs": {"transport": "stdio", "command": "npx", "args": ["fs"]}}}"#,
        )
        .unwrap();
        let config = UserConfig::load_layered(&system, &[peco, team]).unwrap();
        assert!(config.mcp.mcp_servers.contains_key("fs"));
    }
}
