// ============================================================================
// Provider 目录 — 每种 provider 类型的静态元信息
// ============================================================================
//
// 这些值是"新建 provider 时应该填什么"的权威答案：默认 base_url 直接取自
// model-provider 各适配器的常量（同一份定义，不会与真实请求地址漂移），
// 建议模型取自各适配器公开的模型名常量。
//
// 消费方：配置 UI（表单默认值）与连接测试端点。

use model_provider::providers::deepseek::{DEEPSEEK_API_BASE_URL, DEEPSEEK_V4_FLASH};
use model_provider::providers::openai::{
    OPENAI_API_BASE_URL, OPENAI_GPT5_1, OPENAI_GPT5_2, OPENAI_GPT5_MINI,
};
use model_provider::providers::qwen::{QWEN_API_BASE_URL, QWEN_FLASH, QWEN_MAX, QWEN_PLUS};

/// 单个 provider 类型的静态元信息。
#[derive(Debug, Clone, Copy)]
pub struct ProviderTypeMeta {
    /// `providers.toml` 中 `type` 字段的取值。
    pub provider_type: &'static str,
    /// 面向用户展示的名字。
    pub display_name: &'static str,
    /// 不填 `base_url` 时适配器实际使用的地址。
    pub default_base_url: &'static str,
    /// `api` 字段的合法取值，首项为该类型的默认风格。
    pub api_modes: &'static [&'static str],
    /// 常用模型名（自由填写，仅作候选提示）。
    pub suggested_models: &'static [&'static str],
    /// 默认模型（新建时的预填值）。
    pub default_model: &'static str,
}

/// 全部受支持的 provider 类型。
///
/// 与 [`crate::agent::build_provider_with_user`] 的分派分支一一对应；
/// `supported_provider_type`/目录条目失配由单元测试兜底。
pub const PROVIDER_TYPES: &[ProviderTypeMeta] = &[
    ProviderTypeMeta {
        provider_type: "deepseek",
        display_name: "DeepSeek",
        default_base_url: DEEPSEEK_API_BASE_URL,
        api_modes: &["responses", "chat"],
        suggested_models: &[DEEPSEEK_V4_FLASH],
        default_model: DEEPSEEK_V4_FLASH,
    },
    ProviderTypeMeta {
        provider_type: "qwen",
        display_name: "通义千问（DashScope）",
        default_base_url: QWEN_API_BASE_URL,
        api_modes: &["responses", "chat"],
        suggested_models: &[QWEN_MAX, QWEN_PLUS, QWEN_FLASH],
        default_model: QWEN_MAX,
    },
    ProviderTypeMeta {
        provider_type: "openai",
        display_name: "OpenAI",
        default_base_url: OPENAI_API_BASE_URL,
        api_modes: &["chat", "responses"],
        suggested_models: &[OPENAI_GPT5_2, OPENAI_GPT5_1, OPENAI_GPT5_MINI],
        default_model: OPENAI_GPT5_2,
    },
];

/// 按 `type` 查找元信息。
pub fn provider_type_meta(provider_type: &str) -> Option<&'static ProviderTypeMeta> {
    PROVIDER_TYPES
        .iter()
        .find(|m| m.provider_type.eq_ignore_ascii_case(provider_type))
}

/// 该 `type` 是否被适配器分派支持。
pub fn is_supported_provider_type(provider_type: &str) -> bool {
    provider_type_meta(provider_type).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 目录里的每个类型都必须能被适配器构建（构建报错信息里不能是
    /// "unsupported provider type"）——防止目录与分派分支漂移。
    #[test]
    fn catalog_types_are_dispatchable() {
        use crate::config::{McpConfig, ProviderEntry, ProvidersConfig, UserConfig};
        use std::collections::HashMap;

        for meta in PROVIDER_TYPES {
            let mut providers = HashMap::new();
            providers.insert(
                meta.provider_type.to_string(),
                ProviderEntry {
                    provider_type: meta.provider_type.to_string(),
                    api_key: Some("sk-probe-placeholder".to_string()),
                    base_url: Some(meta.default_base_url.to_string()),
                    api: None,
                    default: None,
                },
            );
            let user_config = UserConfig {
                providers: ProvidersConfig {
                    default_provider: meta.provider_type.to_string(),
                    providers,
                    web_search: None,
                },
                mcp: McpConfig::empty(),
            };

            let model_config = crate::agent::ModelConfig {
                provider_name: Some(meta.provider_type.to_string()),
                model_name: Some(meta.default_model.to_string()),
                temperature: None,
                max_tokens: None,
                stream: None,
                reasoning_effort: None,
            };

            let built = crate::agent::build_provider_with_user(&model_config, &user_config);
            assert!(
                built.is_ok(),
                "provider type '{}' 无法构建：{:?}",
                meta.provider_type,
                built.err()
            );
        }
    }

    /// 未知类型不产生元信息（UI 据此拒绝非法输入）。
    #[test]
    fn unknown_provider_type_has_no_meta() {
        assert!(provider_type_meta("anthropic").is_none());
        assert!(!is_supported_provider_type("ollama"));
        assert!(is_supported_provider_type("DeepSeek"));
    }

    /// 目录默认模型必须落在各自类型的建议模型列表内（UI 预填值可选）。
    #[test]
    fn default_model_is_suggested() {
        for meta in PROVIDER_TYPES {
            assert!(
                meta.suggested_models.contains(&meta.default_model),
                "{} 的默认模型 {} 不在建议列表中",
                meta.provider_type,
                meta.default_model
            );
        }
    }
}
