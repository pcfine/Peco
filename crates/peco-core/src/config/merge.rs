// ============================================================================
// 深递归合并算法 — providers.toml 用户配置覆盖系统配置
// ============================================================================
//
// 合并规则：
// 1. provider entries：同名 entry 做字段级递归合并
// 2. 标量字段（api_key, base_url, provider_type）：用户值覆盖系统值
// 3. 嵌套字段（default: LlmApiParams）：递归合并每个子字段
// 4. default_provider：用户优先，未配置则用系统值

use super::types::{LlmApiParams, ProviderEntry, ProvidersConfig};
use std::collections::HashMap;

/// 深递归合并两个 `ProvidersConfig`。
///
/// 用户配置中的 provider entries 按名称覆盖系统配置的同名 entry。
/// 用户有而系统没有的 provider 直接追加。
/// 系统有而用户没有的 provider 原样保留。
pub fn merge_providers_config(system: &ProvidersConfig, user: &ProvidersConfig) -> ProvidersConfig {
    let mut merged_providers: HashMap<String, ProviderEntry> = HashMap::new();

    // 先插入所有系统 provider
    for (name, entry) in &system.providers {
        merged_providers.insert(name.clone(), entry.clone());
    }

    // 用户 provider 覆盖或追加
    for (name, user_entry) in &user.providers {
        match merged_providers.get(name) {
            Some(sys_entry) => {
                // 深递归合并同名 provider
                merged_providers.insert(name.clone(), merge_provider_entry(sys_entry, user_entry));
            }
            None => {
                // 用户新增的 provider
                merged_providers.insert(name.clone(), user_entry.clone());
            }
        }
    }

    // default_provider：用户优先，未配置则用系统值
    let default_provider =
        if user.default_provider.is_empty() || user.default_provider == system.default_provider {
            system.default_provider.clone()
        } else if merged_providers.contains_key(&user.default_provider) {
            user.default_provider.clone()
        } else {
            system.default_provider.clone()
        };

    ProvidersConfig {
        default_provider,
        providers: merged_providers,
    }
}

/// 递归合并两个 ProviderEntry。
///
/// - 标量字段（provider_type, api_key, base_url）：用户 Some 覆盖系统
/// - 嵌套字段（default: LlmApiParams）：递归合并每个子字段
fn merge_provider_entry(system: &ProviderEntry, user: &ProviderEntry) -> ProviderEntry {
    let provider_type =
        if user.provider_type.is_empty() || user.provider_type == system.provider_type {
            system.provider_type.clone()
        } else {
            user.provider_type.clone()
        };

    let api_key = user.api_key.clone().or_else(|| system.api_key.clone());
    let base_url = user.base_url.clone().or_else(|| system.base_url.clone());
    let api = user.api.clone().or_else(|| system.api.clone());

    let default = match (&system.default, &user.default) {
        (Some(sys_default), Some(user_default)) => {
            Some(merge_llm_params(sys_default, user_default))
        }
        (None, Some(user_default)) => Some(user_default.clone()),
        (Some(sys_default), None) => Some(sys_default.clone()),
        (None, None) => None,
    };

    ProviderEntry {
        provider_type,
        api_key,
        base_url,
        api,
        default,
    }
}

/// 递归合并两个 LlmApiParams。
///
/// - 每个字段用户 Some 覆盖系统
/// - 用户未提供的字段保留系统值
fn merge_llm_params(system: &LlmApiParams, user: &LlmApiParams) -> LlmApiParams {
    LlmApiParams {
        model: if user.model.is_empty() {
            system.model.clone()
        } else {
            user.model.clone()
        },
        temperature: user.temperature.or(system.temperature),
        max_tokens: user.max_tokens.or(system.max_tokens),
        stream: user.stream.or(system.stream),
        reasoning_effort: user
            .reasoning_effort
            .clone()
            .or_else(|| system.reasoning_effort.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_system_config() -> ProvidersConfig {
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
                    max_tokens: Some(4096),
                    stream: Some(true),
                    reasoning_effort: None,
                }),
            },
        );
        ProvidersConfig {
            default_provider: "deepseek".to_string(),
            providers,
        }
    }

    fn make_user_config() -> ProvidersConfig {
        let mut providers = HashMap::new();
        providers.insert(
            "deepseek".to_string(),
            ProviderEntry {
                provider_type: "deepseek".to_string(),
                api_key: Some("sk-xxx".to_string()),
                base_url: None,
                api: None,
                default: Some(LlmApiParams {
                    model: "deepseek-v4-pro".to_string(),
                    temperature: None,
                    max_tokens: None,
                    stream: None,
                    reasoning_effort: Some("high".to_string()),
                }),
            },
        );
        ProvidersConfig {
            default_provider: String::new(),
            providers,
        }
    }

    #[test]
    fn test_deep_merge_preserves_system_defaults() {
        let system = make_system_config();
        let user = make_user_config();
        let merged = merge_providers_config(&system, &user);

        let ds = merged.providers.get("deepseek").unwrap();
        let default = ds.default.as_ref().unwrap();

        // 用户覆盖
        assert_eq!(ds.api_key.as_deref(), Some("sk-xxx"));
        assert_eq!(default.model, "deepseek-v4-pro");
        assert_eq!(default.reasoning_effort.as_deref(), Some("high"));

        // 系统继承（未丢失）
        assert_eq!(ds.base_url.as_deref(), Some("https://api.deepseek.com"));
        assert_eq!(default.temperature, Some(0.7));
        assert_eq!(default.max_tokens, Some(4096));
        assert_eq!(default.stream, Some(true));
    }

    #[test]
    fn test_deep_merge_default_provider_falls_back() {
        let system = make_system_config();
        let user = make_user_config();
        let merged = merge_providers_config(&system, &user);

        // 用户未设置 default_provider，应继承系统值
        assert_eq!(merged.default_provider, "deepseek");
    }

    #[test]
    fn test_merge_user_only_provider_added() {
        let system = make_system_config();
        let mut user = make_user_config();
        user.providers.insert(
            "openai".to_string(),
            ProviderEntry {
                provider_type: "openai".to_string(),
                api_key: Some("sk-openai".to_string()),
                base_url: None,
                api: None,
                default: None,
            },
        );

        let merged = merge_providers_config(&system, &user);
        assert!(merged.providers.contains_key("deepseek"));
        assert!(merged.providers.contains_key("openai"));
        assert_eq!(merged.providers.len(), 2);
    }
}
