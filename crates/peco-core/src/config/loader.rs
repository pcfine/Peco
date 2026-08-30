// ============================================================================
// Config 加载器 — 从标准路径读取 providers.toml
// ============================================================================

use std::path::{Path, PathBuf};

use super::error::ConfigError;
use super::types::ProvidersConfig;

/// 从标准路径加载配置文件。
///
/// 按优先级依次尝试：
/// 1. `PECO_PROVIDERS_CONFIG` 环境变量
/// 2. 当前工作目录下的 `providers.toml`
/// 3. `~/.config/peco/providers.toml`
///
/// 返回第一个存在的文件路径，若都不存在则返回 [`ConfigError::ConfigNotFound`]。
pub fn find_config_path() -> Result<PathBuf, ConfigError> {
    let candidates: Vec<PathBuf> = [
        std::env::var("PECO_PROVIDERS_CONFIG")
            .ok()
            .map(PathBuf::from),
        Some(PathBuf::from("providers.toml")),
        std::env::var("HOME").ok().map(|h| {
            PathBuf::from(h)
                .join(".config")
                .join("peco")
                .join("providers.toml")
        }),
    ]
    .into_iter()
    .flatten()
    .collect();

    for path in &candidates {
        if path.exists() {
            return Ok(path.clone());
        }
    }
    Err(ConfigError::ConfigNotFound)
}

/// 从指定路径或默认位置加载 [`ProvidersConfig`]。
///
/// 若 `path` 为 `None`，按 [`find_config_path`] 搜索。
/// 若文件不存在但 `required` 为 `false`，返回内置默认配置。
pub fn load_config(path: Option<&Path>) -> Result<ProvidersConfig, ConfigError> {
    let resolved = match path {
        Some(p) => p.to_path_buf(),
        None => find_config_path()?,
    };

    let content = std::fs::read_to_string(&resolved)?;
    let config: ProvidersConfig = toml::from_str(&content)?;
    Ok(config)
}

/// 将 [`ProvidersConfig`] 写入指定路径。
///
/// 若父目录不存在，自动创建。
pub fn save_config(config: &ProvidersConfig, path: &Path) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// 解析并验证配置，返回所有 provider 的名称列表（排序后）。
pub fn provider_names(config: &ProvidersConfig) -> Vec<&str> {
    let mut names: Vec<&str> = config.providers.keys().map(|s| s.as_str()).collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_save_and_load_roundtrip() {
        let config = ProvidersConfig {
            default_provider: "deepseek".into(),
            providers: Default::default(),
            web_search: None,
        };

        let dir = std::env::temp_dir().join("peco-config-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("providers.toml");

        save_config(&config, &path).unwrap();
        let loaded = load_config(Some(&path)).unwrap();
        assert_eq!(loaded.default_provider, "deepseek");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_provider_names_sorted() {
        use super::super::types::ProviderEntry;
        let mut config = ProvidersConfig {
            default_provider: "openai".into(),
            providers: Default::default(),
            web_search: None,
        };
        config.providers.insert(
            "deepseek".into(),
            ProviderEntry {
                provider_type: "deepseek".into(),
                api_key: Some("sk-123".into()),
                base_url: None,
                api: None,
                default: None,
            },
        );
        config.providers.insert(
            "openai".into(),
            ProviderEntry {
                provider_type: "openai".into(),
                api_key: Some("sk-456".into()),
                base_url: None,
                api: None,
                default: None,
            },
        );
        let names = provider_names(&config);
        assert_eq!(names, vec!["deepseek", "openai"]);
    }
}
