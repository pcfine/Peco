// Provider 连接探针 — 用真实适配器发一次最小请求
//
// 设计要点：探针复用 `build_provider_with_user` 的分派逻辑（适配器选择、
// api 风格、base_url 覆盖、`${ENV_VAR}` 解析），因此"测试通过"与"对话能用"
// 走的是同一条代码路径，不存在探针专用实现与实际请求漂移的可能。
//
// 探针是一笔真实计费请求（约 1 个输出 token），成本可忽略但与"ping 域名"
// 有本质区别：它同时验证凭据、endpoint、API 风格与模型名是否存在。
// 因此探针失败要给出可诊断的原因（HTTP 状态 + 错误体摘要），而非笼统的失败。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use model_provider::{GenerateRequest, InputItem, ReasoningConfig, Role};
use peco_core::agent::{ModelConfig, build_provider_with_user};
use peco_core::config::{McpConfig, ProviderEntry, ProvidersConfig, UserConfig};

/// 探针请求的输出上限。
///
/// 16 是各适配器的安全下限（DashScope Responses 对低于 16 的值直接返回 400），
/// 既足够拿到一次完整响应，又让推理型模型的思考开销降到最低。
const PROBE_MAX_OUTPUT_TOKENS: u32 = 16;

/// 探针整体超时。前端 HTTP 客户端超时为 30s，这里必须显著更短，
/// 才能把"上游无响应"变成一条可读结果而不是前端超时报错。
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// 错误体摘要长度上限 — 上游报错常带整段 HTML/JSON，截断后再回给前端。
const ERROR_BODY_LIMIT: usize = 400;

/// 一次连接测试的结论。
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub success: bool,
    /// 面向用户的结论（成功时的确认信息或失败原因）。
    pub message: String,
    /// 实际用于探针的 provider 名与模型名（故障定位用）。
    pub provider_name: String,
    pub model: String,
}

impl ProbeOutcome {
    fn failure(provider_name: &str, model: &str, message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            provider_name: provider_name.to_string(),
            model: model.to_string(),
        }
    }

    fn success(provider_name: &str, model: &str) -> Self {
        Self {
            success: true,
            message: format!("连接正常（provider '{provider_name}'，模型 '{model}'）"),
            provider_name: provider_name.to_string(),
            model: model.to_string(),
        }
    }
}

/// 探针的输入 — 保存前的表单值或已保存的条目都能映射到它。
#[derive(Debug, Clone)]
pub struct ProbeTarget {
    /// provider 逻辑名（providers.toml 的 key，也是 agent.md 引用它的名字）。
    pub name: String,
    pub provider_type: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub api: Option<String>,
    pub model: Option<String>,
}

/// 用给定目标发一次最小真实请求。
///
/// 不使用 `?`：所有失败都转成 `success: false` 的结论回给调用方，
/// 因为"连不上"是本接口的正常返回之一，不是服务端错误。
pub async fn run_probe(target: ProbeTarget) -> ProbeOutcome {
    let model_label = target.model.clone().unwrap_or_default();

    // 1. 组装与运行时同构的用户配置（只含目标 provider）
    let user_config = match build_user_config(&target) {
        Ok(cfg) => cfg,
        Err(msg) => return ProbeOutcome::failure(&target.name, &model_label, msg),
    };

    let model_config = ModelConfig {
        provider_name: Some(target.name.clone()),
        model_name: target.model.clone(),
        temperature: None,
        max_tokens: None,
        stream: None,
        reasoning_effort: None,
    };

    // 2. 与对话路径同一分派：适配器 + api 风格 + base_url + key 解析
    let provider = match build_provider_with_user(&model_config, &user_config) {
        Ok(p) => p,
        Err(e) => {
            return ProbeOutcome::failure(&target.name, &model_label, format!("配置无效：{e}"));
        }
    };

    // 3. 模型名缺失时无法发起请求 — 这本身就是要报给用户的配置缺陷
    let Some(model) = target.model.clone().filter(|m| !m.trim().is_empty()) else {
        return ProbeOutcome::failure(
            &target.name,
            &model_label,
            "未指定模型名：请在「默认模型」填入该 provider 的模型，或让 Agent 的 agent.md 指定 llm.model",
        );
    };

    let request = GenerateRequest {
        model: model.clone(),
        instructions: Some("Connection probe. Reply with a single word.".to_string()),
        input: vec![Arc::new(InputItem::Message {
            role: Role::User,
            content: "ping".to_string().into(),
        })]
        .into(),
        tools: vec![],
        tool_choice: None,
        temperature: Some(0.0),
        top_p: None,
        max_output_tokens: Some(PROBE_MAX_OUTPUT_TOKENS),
        // 关闭推理：探针只验证连通性与凭据，思考会拖长延迟、放大 token 消耗
        reasoning: Some(ReasoningConfig {
            enabled: false,
            effort: None,
        }),
        text: None,
        additional_params: None,
    };

    match tokio::time::timeout(PROBE_TIMEOUT, provider.generate_full(&request)).await {
        Err(_) => ProbeOutcome::failure(
            &target.name,
            &model,
            format!("请求超时（超过 {}s 无响应）", PROBE_TIMEOUT.as_secs()),
        ),
        Ok(Err(e)) => ProbeOutcome::failure(&target.name, &model, describe_provider_error(&e)),
        Ok(Ok(result)) => {
            // 适配器把上游错误折叠进 GenerateResult.error，不会返回 Err
            if let Some(err) = result.error {
                return ProbeOutcome::failure(
                    &target.name,
                    &model,
                    format!("上游返回失败：{}", err.message),
                );
            }
            ProbeOutcome::success(&target.name, &model)
        }
    }
}

/// 把表单值组装成单条目的 [`UserConfig`]。
fn build_user_config(target: &ProbeTarget) -> Result<UserConfig, String> {
    if target.provider_type.trim().is_empty() {
        return Err("provider 类型不能为空".to_string());
    }
    // 取目录里的规范值：适配器分派是精确匹配，大小写变体会在构建期才失败
    let Some(meta) = peco_core::config::provider_type_meta(&target.provider_type) else {
        let supported: Vec<&str> = peco_core::config::PROVIDER_TYPES
            .iter()
            .map(|m| m.provider_type)
            .collect();
        return Err(format!(
            "不支持的 provider 类型 '{}'（支持：{}）",
            target.provider_type,
            supported.join(", ")
        ));
    };
    if target
        .api_key
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        return Err("API Key 不能为空".to_string());
    }

    let mut providers = HashMap::new();
    providers.insert(
        target.name.clone(),
        ProviderEntry {
            provider_type: meta.provider_type.to_string(),
            api_key: target.api_key.clone(),
            base_url: target.base_url.clone().filter(|u| !u.trim().is_empty()),
            api: target.api.clone().filter(|a| !a.trim().is_empty()),
            default: None,
        },
    );

    Ok(UserConfig {
        providers: ProvidersConfig {
            default_provider: target.name.clone(),
            providers,
            web_search: None,
        },
        mcp: McpConfig::empty(),
    })
}

/// 把适配器错误翻译成可诊断的中文说明。
///
/// 关键是保留 HTTP 状态与上游错误体摘要 — "401 invalid api key" 与
/// "404 model not found" 需要用户做完全不同的事，笼统的"连接失败"没有价值。
fn describe_provider_error(err: &model_provider::ProviderError) -> String {
    use model_provider::ProviderError;

    match err {
        ProviderError::Api { status, body } => {
            let hint = match *status {
                401 | 403 => "（API Key 无效或无权限）",
                404 => "（endpoint 或模型名不存在）",
                429 => "（触发限流或余额不足）",
                _ => "",
            };
            format!("HTTP {status}{hint}：{}", truncate(body, ERROR_BODY_LIMIT))
        }
        ProviderError::Http(e) => format!("网络请求失败：{e}"),
        ProviderError::Json(e) => format!("响应解析失败（可能不是兼容的 API 端点）：{e}"),
        ProviderError::Response(msg) => format!("响应异常：{}", truncate(msg, ERROR_BODY_LIMIT)),
        ProviderError::Stream(msg) => format!("流式响应异常：{}", truncate(msg, ERROR_BODY_LIMIT)),
        ProviderError::Request(msg) => format!("请求构建失败：{msg}"),
    }
}

/// 按字符边界截断（错误体可能含多字节字符）。
fn truncate(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let head: String = s.chars().take(limit).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(provider_type: &str, key: Option<&str>, model: Option<&str>) -> ProbeTarget {
        ProbeTarget {
            name: provider_type.to_string(),
            provider_type: provider_type.to_string(),
            api_key: key.map(str::to_string),
            base_url: None,
            api: None,
            model: model.map(str::to_string),
        }
    }

    /// 未知类型在发请求前就被拒绝，且错误信息列出合法取值。
    #[test]
    fn unsupported_type_rejected_before_request() {
        let msg = build_user_config(&target("anthropic", Some("sk-x"), None)).unwrap_err();
        assert!(msg.contains("不支持的 provider 类型"), "{msg}");
        assert!(msg.contains("deepseek"), "{msg}");
    }

    /// 空 key 直接拒绝 — 不发无谓请求。
    #[test]
    fn empty_api_key_rejected() {
        assert!(build_user_config(&target("deepseek", None, None)).is_err());
        assert!(build_user_config(&target("deepseek", Some("   "), None)).is_err());
    }

    /// 只允许表单里出现的目标 provider，`default_provider` 指向它。
    #[test]
    fn single_entry_config_uses_target_provider() {
        let cfg = build_user_config(&target("qwen", Some("sk-x"), Some("qwen3.7-max"))).unwrap();
        assert_eq!(cfg.default_provider_name(), "qwen");
        assert_eq!(cfg.providers.providers.len(), 1);
        let entry = cfg.provider_entry(Some("qwen")).unwrap();
        assert_eq!(entry.provider_type, "qwen");
        assert!(entry.base_url.is_none());
    }

    /// 空白 base_url 视同未填（走适配器默认地址），避免拼出 " /v1" 这类坏 URL。
    #[test]
    fn blank_base_url_treated_as_absent() {
        let mut t = target("openai", Some("sk-x"), None);
        t.base_url = Some("  ".to_string());
        let cfg = build_user_config(&t).unwrap();
        assert!(
            cfg.provider_entry(Some("openai"))
                .unwrap()
                .base_url
                .is_none()
        );
    }

    /// 缺少模型名 → 明确提示该填哪里，而不是发一个必然 404 的请求。
    #[tokio::test]
    async fn missing_model_reported_without_request() {
        let outcome = run_probe(target("deepseek", Some("sk-x"), None)).await;
        assert!(!outcome.success);
        assert!(
            outcome.message.contains("未指定模型名"),
            "{}",
            outcome.message
        );
    }

    /// `${ENV_VAR}` 形式且变量不存在 → 报环境变量缺失，而不是网络错误。
    #[tokio::test]
    async fn missing_env_var_key_reported() {
        let outcome = run_probe(target(
            "deepseek",
            Some("${PECO_PROBE_MISSING_VAR_9F3A}"),
            Some("deepseek-v4-flash"),
        ))
        .await;
        assert!(!outcome.success);
        assert!(outcome.message.contains("配置无效"), "{}", outcome.message);
    }

    /// HTTP 状态与错误体摘要必须出现在结论里（用户据此区分 key 错/模型错）。
    #[test]
    fn api_error_keeps_status_and_body() {
        let msg = describe_provider_error(&model_provider::ProviderError::Api {
            status: 401,
            body: "{\"error\":{\"message\":\"invalid api key\"}}".to_string(),
        });
        assert!(msg.contains("401"), "{msg}");
        assert!(msg.contains("API Key 无效"), "{msg}");
        assert!(msg.contains("invalid api key"), "{msg}");

        let msg404 = describe_provider_error(&model_provider::ProviderError::Api {
            status: 404,
            body: "model not found".to_string(),
        });
        assert!(msg404.contains("404"), "{msg404}");
        assert!(msg404.contains("模型名不存在"), "{msg404}");
    }

    /// 超长错误体按字符截断，不 panic 在多字节边界。
    #[test]
    fn long_body_truncated_on_char_boundary() {
        let body = "错".repeat(ERROR_BODY_LIMIT * 2);
        let out = truncate(&body, ERROR_BODY_LIMIT);
        assert_eq!(out.chars().count(), ERROR_BODY_LIMIT + 1); // +1 = 省略号
        assert!(out.ends_with('…'));
    }
}
