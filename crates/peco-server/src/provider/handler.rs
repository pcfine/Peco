// Provider Handler — providers.toml 管理
//
// 写语义是**字段级部分更新**：请求里 `None` 的字段保留已存值，只有显式给出
// 的值才覆盖。原因有两个：
//   1. api_key 永不回读（响应里只有 `has_api_key`），前端编辑时拿不到旧值，
//      全量替换语义会把它清空；
//   2. 模型/默认参数同理，用户只改 base_url 时不该丢模型配置。
// 需要清空某个字段时显式传空串。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use peco_core::config::{LlmApiParams, ProvidersConfig};
use serde::{Deserialize, Serialize};

use super::probe::{ProbeOutcome, ProbeTarget, run_probe};
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

/// provider 在列表/详情中的呈现。
///
/// `api_key` 永不回读 — 只暴露是否已配置，避免凭据经 API 二次泄露。
#[derive(Debug, Serialize)]
pub struct ProviderInfo {
    /// providers.toml 中的 key，也是 agent.md `llm.provider` 引用它的名字。
    pub name: String,
    pub provider_type: String,
    pub base_url: Option<String>,
    /// 该 provider 的默认模型（Agent 未在 agent.md 指定 model 时生效）。
    pub default_model: Option<String>,
    /// API 风格：`"responses"` | `"chat"`；`None` = 用该类型的默认风格。
    pub api: Option<String>,
    /// 是否已配置 API Key（不回读明文）。
    pub has_api_key: bool,
    /// 是否为当前默认 provider（agent.md 未指定 provider 时走它）。
    pub is_default: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpsertProviderRequest {
    /// provider 逻辑名（providers.toml 的 key）。省略时回退为 `type`。
    ///
    /// 这是 agent.md `llm.provider` 引用的名字：想替换默认 provider 的凭据时
    /// 就沿用 `deepseek` 这类既有名字；想并存多个端点则另起别名。
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub provider_type: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    /// 使用的 API 风格：`"responses"` | `"chat"`（可选，默认随类型）。
    #[serde(default)]
    pub api: Option<String>,
    /// 该 provider 的默认模型（可选）。
    #[serde(default)]
    pub default_model: Option<String>,
    /// 是否同时把该 provider 设为默认（`default_provider`）。
    #[serde(default)]
    pub set_default: bool,
}

#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// provider 类型目录的呈现 — 供配置 UI 预填默认 URL / 模型。
#[derive(Debug, Serialize)]
pub struct ProviderTypeInfo {
    pub provider_type: String,
    pub display_name: String,
    pub default_base_url: String,
    pub api_modes: Vec<String>,
    pub suggested_models: Vec<String>,
    pub default_model: String,
}

/// 连接测试结果。
#[derive(Debug, Serialize)]
pub struct TestResponse {
    pub success: bool,
    pub message: String,
    pub provider_type: String,
    pub model: String,
}

impl From<ProbeOutcome> for TestResponse {
    fn from(outcome: ProbeOutcome) -> Self {
        Self {
            success: outcome.success,
            message: outcome.message,
            provider_type: outcome.provider_name,
            model: outcome.model,
        }
    }
}

fn load_providers(path: &std::path::Path) -> Result<ProvidersConfig, ApiError> {
    if path.exists() {
        let content = std::fs::read_to_string(path)
            .map_err(|e| ApiError::Internal(format!("failed to read providers.toml: {e}")))?;
        toml::from_str(&content)
            .map_err(|e| ApiError::BadRequest(format!("invalid providers.toml: {e}")))
    } else {
        Ok(ProvidersConfig {
            default_provider: "deepseek".to_string(),
            providers: Default::default(),
            web_search: None,
        })
    }
}

fn save_providers(path: &std::path::Path, config: &ProvidersConfig) -> Result<(), ApiError> {
    let content = toml::to_string_pretty(config)
        .map_err(|e| ApiError::Internal(format!("failed to serialize providers.toml: {e}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApiError::Internal(format!("failed to create directory: {e}")))?;
    }
    std::fs::write(path, content)
        .map_err(|e| ApiError::Internal(format!("failed to write providers.toml: {e}")))
}

/// 请求里显式给出的 provider 名，省略时回退为 `fallback`（调用方传目录里的规范类型名，
/// 使名字不会因大小写变体而变成 `OpenAI` 这种与类型不一致的 key）。
fn resolve_provider_name(req: &UpsertProviderRequest, fallback: &str) -> String {
    req.name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// 部分更新：`None` 保留旧值，`Some("")` 清空。
fn merge_opt(old: Option<String>, new: Option<String>) -> Option<String> {
    match new {
        None => old,
        Some(v) if v.trim().is_empty() => None,
        Some(v) => Some(v.trim().to_string()),
    }
}

/// 部分更新默认模型：`None` 保留旧模型与旧参数，只换 model 字段。
fn merge_default_model(
    old: Option<LlmApiParams>,
    new_model: Option<String>,
) -> Option<LlmApiParams> {
    let Some(model) = new_model.filter(|m| !m.trim().is_empty()) else {
        return old;
    };
    match old {
        Some(mut params) => {
            params.model = model.trim().to_string();
            Some(params)
        }
        None => Some(LlmApiParams {
            model: model.trim().to_string(),
            temperature: None,
            max_tokens: None,
            stream: None,
            reasoning_effort: None,
        }),
    }
}

/// 把一次 upsert 请求应用到配置上，返回生效的 provider 名。
///
/// 纯函数（不碰文件系统），字段级部分更新的全部语义都在这里：
/// 省略即保留、空串即清空、换类型即不继承、新条目可在满足条件时接管默认位。
fn apply_upsert(
    config: &mut ProvidersConfig,
    req: &UpsertProviderRequest,
) -> Result<String, ApiError> {
    // 目录查找大小写不敏感，但适配器分派是精确匹配 —— 存目录里的规范值，
    // 否则 "OpenAI" 这类写法能通过校验，却在构建 Agent 时报 unsupported。
    let Some(meta) = peco_core::config::provider_type_meta(&req.provider_type) else {
        let supported: Vec<&str> = peco_core::config::PROVIDER_TYPES
            .iter()
            .map(|m| m.provider_type)
            .collect();
        return Err(ApiError::BadRequest(format!(
            "unsupported provider type '{}' (supported: {})",
            req.provider_type,
            supported.join(", ")
        )));
    };
    let provider_type = meta.provider_type.to_string();

    let name = resolve_provider_name(req, &provider_type);
    if name.is_empty() {
        return Err(ApiError::BadRequest(
            "provider name must not be empty".into(),
        ));
    }

    let existing = config.providers.get(&name).cloned();
    let is_new = existing.is_none();

    // 换类型 = 换端点：旧类型的密钥 / 地址 / 默认模型一律不继承，否则改完
    // 类型后会把 A 家的 api_key 发到 B 家的地址上。此时只有请求显式给出的
    // 字段生效（前端切类型时会同时带上新类型的默认地址与模型）。
    let carried = existing.filter(|e| e.provider_type.eq_ignore_ascii_case(&provider_type));

    let api_key = match req.api_key.clone() {
        None => carried.as_ref().and_then(|e| e.api_key.clone()),
        Some(k) if k.trim().is_empty() => None,
        Some(k) => Some(k),
    };
    let base_url = merge_opt(
        carried.as_ref().and_then(|e| e.base_url.clone()),
        req.base_url.clone(),
    );
    let api = merge_opt(
        carried.as_ref().and_then(|e| e.api.clone()),
        req.api.clone(),
    );
    // api 档位是类型特有的；写进不支持的档位会让 Agent 构建期才报错
    if let Some(mode) = api.as_deref()
        && !meta.api_modes.iter().any(|m| m.eq_ignore_ascii_case(mode))
    {
        return Err(ApiError::BadRequest(format!(
            "unsupported api mode '{mode}' for provider type '{provider_type}' (supported: {})",
            meta.api_modes.join(", ")
        )));
    }
    let default = merge_default_model(
        carried.as_ref().and_then(|e| e.default.clone()),
        req.default_model.clone(),
    );

    config.providers.insert(
        name.clone(),
        peco_core::config::ProviderEntry {
            provider_type,
            api_key,
            base_url,
            api,
            default,
        },
    );

    // `set_default` 显式接管；或这是配置里的第一个 provider —— 否则新增的
    // provider 永远不会被"agent.md 未指定 provider"的 Agent 用到，
    // 表现为"加好了却没有任何变化"。
    if req.set_default || (is_new && config.providers.len() == 1) {
        config.default_provider = name.clone();
    }

    Ok(name)
}

/// 从配置中删除一个 provider。
///
/// 默认 provider 被删除时回落到剩余条目里字典序最小的一个；一个都不剩则置空 ——
/// 空串在 `merge_providers_config` 中表示"交给系统层默认值"，是唯一不产生
/// 悬空引用的取值（留着被删的名字会让所有未指定 provider 的 Agent 报
/// "provider not found"，或在系统层有同名条目时静默换用系统凭据）。
fn apply_delete(config: &mut ProvidersConfig, name: &str) -> Result<(), ApiError> {
    if config.providers.remove(name).is_none() {
        return Err(ApiError::NotFound(format!("provider '{name}' not found")));
    }

    if config.default_provider == name {
        let mut remaining: Vec<&String> = config.providers.keys().collect();
        remaining.sort();
        config.default_provider = remaining.first().map(|s| (*s).clone()).unwrap_or_default();
    }
    Ok(())
}

/// 合并后的生效配置里，该 provider 是否带凭据。
///
/// 用户文件与系统配置深递归合并时 api_key 会由系统层兜底，所以"能跑"与
/// "用户文件里写了 key"是两回事。
fn has_effective_api_key(config: &peco_core::config::UserConfig, name: &str) -> bool {
    config
        .provider_entry(Some(name))
        .and_then(|e| e.api_key.as_ref())
        .is_some()
}

/// 拼接保存结果文案：热重载失败时附在成功文案之后（写入本身已成功），
/// 形如 `Provider 'x' saved, but hot reload failed: ...`。
fn saved_message(base: String, warning: Option<String>) -> String {
    match warning {
        Some(w) => format!("{base}, {w}"),
        None => base,
    }
}

/// 保存后统一收尾：让 provider 立即生效，并登记模块哈希。
///
/// **立即生效**靠 `reload_providers` 就地替换 provider 快照并失效已缓存 Agent ——
/// 否则 LRU 里那个 WorkSpace 会一直拿着旧凭据，直到进程重启或缓存被驱逐。
/// 哈希只服务于跨进程/外部改动的增量同步，因此在重载**成功之后**才写入：
/// 写入失败重载的哈希会让文件监听误判"已应用"而不再重试。
///
/// 重载失败不当作保存失败返回：文件此刻已经落盘，报错只会让用户反复重试一个
/// 已经生效的写入。失败原因（例如 workspace 的 mcpconfig.json 有语法错误，
/// `UserConfig::load` 会一并解析）作为警告文案回给调用方。
async fn after_provider_change(
    state: &AppState,
    user_id: &str,
    ws: &peco_core::workspace::WorkSpace,
) -> Option<String> {
    match ws.reload_providers() {
        Ok(invalidated) => {
            let providers_hash = peco_core::workspace::hash::compute_providers_hash(ws.root());
            if let Err(e) = crate::db::workspace_hashes::upsert_hash(
                &state.db,
                user_id,
                "providers",
                &providers_hash,
            )
            .await
            {
                tracing::warn!(%user_id, error = %e, "Failed to record providers hash");
            }
            tracing::info!(
                %user_id,
                invalidated,
                "Provider config reloaded; cached agents invalidated"
            );
            None
        }
        Err(e) => {
            tracing::warn!(%user_id, error = %e, "Provider config saved but reload failed");
            Some(format!(
                "but hot reload failed: {e}. Changes take effect after restart"
            ))
        }
    }
}

/// `GET /api/providers/types` — 各 provider 类型的默认值目录。
pub async fn list_types(
    AuthUser { user_id: _ }: AuthUser,
) -> Result<Json<Vec<ProviderTypeInfo>>, ApiError> {
    Ok(Json(
        peco_core::config::PROVIDER_TYPES
            .iter()
            .map(|m| ProviderTypeInfo {
                provider_type: m.provider_type.to_string(),
                display_name: m.display_name.to_string(),
                default_base_url: m.default_base_url.to_string(),
                api_modes: m.api_modes.iter().map(|s| s.to_string()).collect(),
                suggested_models: m.suggested_models.iter().map(|s| s.to_string()).collect(),
                default_model: m.default_model.to_string(),
            })
            .collect(),
    ))
}

/// `GET /api/providers` — 用户 providers.toml 中的条目。
///
/// 只列用户文件里的条目（它们可编辑）；系统级 provider 由合并逻辑兜底，
/// 不出现在这个列表里，但会影响 `is_default` 的判定。
pub async fn list(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ProviderInfo>>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let path = ws.root().join("providers.toml");
    let config = load_providers(&path)?;
    let effective = ws.config();
    let effective_default = effective.default_provider_name().to_string();

    let providers: Vec<ProviderInfo> = config
        .providers
        .iter()
        .map(|(name, entry)| ProviderInfo {
            name: name.clone(),
            provider_type: entry.provider_type.clone(),
            base_url: entry.base_url.clone(),
            default_model: entry.default.as_ref().map(|d| d.model.clone()),
            api: entry.api.clone(),
            // 凭据来自合并后的生效配置：用户文件里没写 key、但系统层补上了的条目
            // 实际可用，按原始文件判定会误报"未配置 API Key"。
            has_api_key: has_effective_api_key(&effective, name),
            is_default: *name == effective_default,
        })
        .collect();

    Ok(Json(providers))
}

pub async fn get(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<ProviderInfo>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let path = ws.root().join("providers.toml");
    let config = load_providers(&path)?;
    let entry = config
        .providers
        .get(&name)
        .ok_or_else(|| ApiError::NotFound(format!("provider '{name}' not found")))?;
    let effective = ws.config();
    let effective_default = effective.default_provider_name().to_string();

    Ok(Json(ProviderInfo {
        is_default: name == effective_default,
        name: name.clone(),
        provider_type: entry.provider_type.clone(),
        base_url: entry.base_url.clone(),
        default_model: entry.default.as_ref().map(|d| d.model.clone()),
        api: entry.api.clone(),
        has_api_key: has_effective_api_key(&effective, &name),
    }))
}

/// `PUT /api/providers` — 新增或部分更新一个 provider。
pub async fn upsert(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpsertProviderRequest>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let path = ws.root().join("providers.toml");
    let mut config = load_providers(&path)?;

    let name = apply_upsert(&mut config, &req)?;

    save_providers(&path, &config)?;
    let warning = after_provider_change(&state, &user_id, &ws).await;

    Ok(Json(SuccessResponse {
        success: true,
        message: Some(saved_message(format!("Provider '{name}' saved"), warning)),
    }))
}

/// `DELETE /api/providers/{name}` — 删除一个 provider。
pub async fn delete(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let path = ws.root().join("providers.toml");
    let mut config = load_providers(&path)?;
    apply_delete(&mut config, &name)?;

    save_providers(&path, &config)?;
    let warning = after_provider_change(&state, &user_id, &ws).await;

    Ok(Json(SuccessResponse {
        success: true,
        message: Some(saved_message(format!("Provider '{name}' deleted"), warning)),
    }))
}

/// `POST /api/providers/{name}/test` — 用**已保存**的配置发起真实请求。
///
/// 测试对象是合并后的生效配置（用户文件 + 系统配置），与对话实际使用的一致。
pub async fn test_connection(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<TestResponse>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let config = ws.config();
    let entry = config
        .provider_entry(Some(&name))
        .ok_or_else(|| ApiError::NotFound(format!("provider '{name}' not found")))?
        .clone();

    let outcome = run_probe(ProbeTarget {
        name: name.clone(),
        provider_type: entry.provider_type,
        api_key: entry.api_key,
        base_url: entry.base_url,
        api: entry.api,
        model: entry.default.map(|d| d.model),
    })
    .await;

    Ok(Json(outcome.into()))
}

/// `POST /api/providers/test` — 用**表单当前值**发起真实请求（保存前可测）。
///
/// 这是"填完就想试一下"的入口：不落盘、不影响正在运行的对话，
/// 因此字段全部必填（没有已存值可回退）。
pub async fn test_draft(
    AuthUser { user_id: _ }: AuthUser,
    Json(req): Json<TestDraftRequest>,
) -> Result<Json<TestResponse>, ApiError> {
    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&req.provider_type)
        .to_string();

    let outcome = run_probe(ProbeTarget {
        name,
        provider_type: req.provider_type,
        api_key: req.api_key,
        base_url: req.base_url,
        api: req.api,
        model: req.default_model,
    })
    .await;

    Ok(Json(outcome.into()))
}

/// `POST /api/providers/test` 的请求体。
#[derive(Debug, Deserialize)]
pub struct TestDraftRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub provider_type: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn req(name: Option<&str>, provider_type: &str) -> UpsertProviderRequest {
        UpsertProviderRequest {
            name: name.map(str::to_string),
            provider_type: provider_type.to_string(),
            api_key: None,
            base_url: None,
            api: None,
            default_model: None,
            set_default: false,
        }
    }

    /// 省略 name 时回退到类型名（前端旧版行为），显式别名优先。
    #[test]
    fn provider_name_falls_back_to_type() {
        assert_eq!(
            resolve_provider_name(&req(None, "deepseek"), "deepseek"),
            "deepseek"
        );
        assert_eq!(
            resolve_provider_name(&req(Some("  "), "openai"), "openai"),
            "openai"
        );
        assert_eq!(
            resolve_provider_name(&req(Some(" gateway "), "openai"), "openai"),
            "gateway"
        );
        // 大小写变体回退到规范名，不产生 "OpenAI" 这种与类型不一致的 key
        assert_eq!(
            resolve_provider_name(&req(None, "OpenAI"), "openai"),
            "openai"
        );
    }

    /// 省略字段保留旧值：只改 base_url 不会清掉 api_key。
    #[test]
    fn omitted_fields_keep_existing_values() {
        assert_eq!(
            merge_opt(Some("sk-old".into()), None),
            Some("sk-old".to_string())
        );
        // 显式空串 = 清空
        assert_eq!(merge_opt(Some("sk-old".into()), Some("".into())), None);
        assert_eq!(
            merge_opt(Some("https://a".into()), Some(" https://b ".into())),
            Some("https://b".to_string())
        );
    }

    /// 省略 default_model 保留旧模型**及其余参数**（temperature 等不被抹掉）。
    #[test]
    fn omitted_model_keeps_params() {
        let old = Some(LlmApiParams {
            model: "deepseek-v4-flash".to_string(),
            temperature: Some(0.3),
            max_tokens: Some(4096),
            stream: Some(true),
            reasoning_effort: Some("high".to_string()),
        });
        let kept = merge_default_model(old.clone(), None).unwrap();
        assert_eq!(kept.model, "deepseek-v4-flash");
        assert_eq!(kept.temperature, Some(0.3));
        assert_eq!(kept.max_tokens, Some(4096));
        assert_eq!(kept.reasoning_effort.as_deref(), Some("high"));

        // 换模型时同样保留其余参数
        let changed = merge_default_model(old, Some("deepseek-v4-pro".into())).unwrap();
        assert_eq!(changed.model, "deepseek-v4-pro");
        assert_eq!(changed.temperature, Some(0.3));
    }

    /// 原本没有 default 段 → 新建一个只含 model 的段。
    #[test]
    fn model_creates_default_section() {
        let created = merge_default_model(None, Some(" gpt-5.2 ".into())).unwrap();
        assert_eq!(created.model, "gpt-5.2");
        assert!(created.temperature.is_none());

        // 空串不创建空段（否则会写出 `[providers.x.default]` 空表）
        assert!(merge_default_model(None, Some("  ".into())).is_none());
    }

    /// 目录接口与常量目录同源。
    #[test]
    fn type_catalog_matches_core() {
        let infos: Vec<ProviderTypeInfo> = peco_core::config::PROVIDER_TYPES
            .iter()
            .map(|m| ProviderTypeInfo {
                provider_type: m.provider_type.to_string(),
                display_name: m.display_name.to_string(),
                default_base_url: m.default_base_url.to_string(),
                api_modes: m.api_modes.iter().map(|s| s.to_string()).collect(),
                suggested_models: m.suggested_models.iter().map(|s| s.to_string()).collect(),
                default_model: m.default_model.to_string(),
            })
            .collect();
        assert_eq!(infos.len(), 3);
        let ds = infos
            .iter()
            .find(|i| i.provider_type == "deepseek")
            .unwrap();
        assert_eq!(ds.default_base_url, "https://api.deepseek.com");
        assert!(
            ds.suggested_models
                .contains(&"deepseek-v4-flash".to_string())
        );
    }

    fn empty_config() -> ProvidersConfig {
        ProvidersConfig {
            default_provider: "deepseek".to_string(),
            providers: HashMap::new(),
            web_search: None,
        }
    }

    fn entry(
        provider_type: &str,
        key: Option<&str>,
        model: Option<&str>,
    ) -> peco_core::config::ProviderEntry {
        peco_core::config::ProviderEntry {
            provider_type: provider_type.to_string(),
            api_key: key.map(str::to_string),
            base_url: None,
            api: None,
            default: model.map(|m| LlmApiParams {
                model: m.to_string(),
                temperature: Some(0.3),
                max_tokens: None,
                stream: None,
                reasoning_effort: None,
            }),
        }
    }

    /// 首次新增（表原本为空）→ 自动接管默认位。
    /// 否则用户"加了 provider 却毫无变化"，正是「新增后没生效」的一半成因。
    #[test]
    fn first_provider_becomes_default() {
        let mut config = empty_config();
        let mut r = req(Some("openai"), "openai");
        r.api_key = Some("sk-openai".to_string());

        let name = apply_upsert(&mut config, &r).unwrap();

        assert_eq!(name, "openai");
        assert_eq!(config.default_provider, "openai");
    }

    /// 已有其他 provider 时不抢默认位；`set_default` 显式要求才接管。
    #[test]
    fn existing_default_not_stolen_without_flag() {
        let mut config = empty_config();
        apply_upsert(&mut config, &req(Some("deepseek"), "deepseek")).unwrap();
        assert_eq!(config.default_provider, "deepseek");

        let mut second = req(Some("openai"), "openai");
        second.api_key = Some("sk-openai".to_string());
        apply_upsert(&mut config, &second).unwrap();
        assert_eq!(config.default_provider, "deepseek");

        let mut third = req(Some("qwen"), "qwen");
        third.set_default = true;
        apply_upsert(&mut config, &third).unwrap();
        assert_eq!(config.default_provider, "qwen");
    }

    /// 回归：编辑时留空 api_key（前端无法回读旧值）不得清空已存凭据。
    #[test]
    fn upsert_without_api_key_keeps_existing_key() {
        let mut config = empty_config();
        config.providers.insert(
            "deepseek".to_string(),
            entry("deepseek", Some("sk-old"), Some("deepseek-v4-flash")),
        );

        let mut r = req(Some("deepseek"), "deepseek");
        r.base_url = Some("https://proxy.internal".to_string());
        apply_upsert(&mut config, &r).unwrap();

        let stored = config.providers.get("deepseek").unwrap();
        assert_eq!(stored.api_key.as_deref(), Some("sk-old"));
        assert_eq!(stored.base_url.as_deref(), Some("https://proxy.internal"));
        // 模型也一并保留
        assert_eq!(stored.default.as_ref().unwrap().model, "deepseek-v4-flash");

        // 显式空串才是"清空凭据"
        let mut clear = req(Some("deepseek"), "deepseek");
        clear.api_key = Some(String::new());
        apply_upsert(&mut config, &clear).unwrap();
        assert!(config.providers.get("deepseek").unwrap().api_key.is_none());
    }

    /// 未支持的类型在写盘前被拒。
    #[test]
    fn unsupported_type_rejected() {
        let mut config = empty_config();
        let err = apply_upsert(&mut config, &req(Some("anthropic"), "anthropic")).unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(_)));
        assert!(config.providers.is_empty());
    }

    /// 删除默认 provider → 默认位回落到剩余条目，不悬空。
    #[test]
    fn delete_reassigns_default_provider() {
        let mut config = empty_config();
        config.providers.insert(
            "deepseek".to_string(),
            entry("deepseek", Some("sk-d"), None),
        );
        config
            .providers
            .insert("openai".to_string(), entry("openai", Some("sk-o"), None));
        config.default_provider = "deepseek".to_string();

        apply_delete(&mut config, "deepseek").unwrap();
        assert_eq!(config.default_provider, "openai");

        // 删除最后一个条目：默认位必须置空（交给系统层兜底），
        // 留着被删的名字会让未指定 provider 的 Agent 全部加载失败。
        apply_delete(&mut config, "openai").unwrap();
        assert!(config.providers.is_empty());
        assert_eq!(config.default_provider, "");
    }

    /// 改写已有 provider 的类型时不继承旧类型的凭据与模型 ——
    /// 否则 A 家的 api_key 会被发到 B 家的地址上。
    #[test]
    fn type_change_does_not_inherit_credentials() {
        let mut config = empty_config();
        config.providers.insert(
            "gateway".to_string(),
            entry("openai", Some("sk-openai"), Some("gpt-5.2")),
        );

        let mut r = req(Some("gateway"), "deepseek");
        r.base_url = Some("https://api.deepseek.com".to_string());
        r.default_model = Some("deepseek-v4-flash".to_string());
        apply_upsert(&mut config, &r).unwrap();

        let stored = config.providers.get("gateway").unwrap();
        assert_eq!(stored.provider_type, "deepseek");
        assert!(stored.api_key.is_none(), "旧类型密钥被继承了");
        assert_eq!(stored.default.as_ref().unwrap().model, "deepseek-v4-flash");

        // 类型不变时仍然按部分更新语义保留
        let mut keep = req(Some("gateway"), "deepseek");
        keep.base_url = Some("https://proxy.internal".to_string());
        apply_upsert(&mut config, &keep).unwrap();
        let stored = config.providers.get("gateway").unwrap();
        assert_eq!(stored.default.as_ref().unwrap().model, "deepseek-v4-flash");
        assert_eq!(stored.base_url.as_deref(), Some("https://proxy.internal"));
    }

    /// provider 类型归一化为目录里的规范值，避免大小写变体通过校验却在
    /// Agent 构建期才报 "unsupported provider type"。
    #[test]
    fn provider_type_is_canonicalized() {
        let mut config = empty_config();
        let mut r = req(Some("gateway"), "OpenAI");
        r.api_key = Some("sk-x".to_string());
        apply_upsert(&mut config, &r).unwrap();

        assert_eq!(
            config.providers.get("gateway").unwrap().provider_type,
            "openai"
        );
    }

    /// 类型不支持的 api 档位在写盘前被拒（分派器只认各类型的合法档位）。
    #[test]
    fn unsupported_api_mode_rejected() {
        let mut config = empty_config();
        let mut r = req(Some("qwen"), "qwen");
        r.api = Some("completions".to_string());
        let err = apply_upsert(&mut config, &r).unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(_)));
        assert!(config.providers.is_empty());
    }

    /// 删除不存在的条目 → 404（而不是静默成功）。
    #[test]
    fn delete_missing_provider_is_not_found() {
        let mut config = empty_config();
        let err = apply_delete(&mut config, "nope").unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)));
    }
}
