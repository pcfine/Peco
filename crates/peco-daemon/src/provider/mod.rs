//! 团队级 provider 配置：`teams/<team_id>/providers.toml` 的读写与呈现。
//!
//! 写入是**字段级部分更新**：请求里 `None` 的字段保留已存值，只有显式给出的
//! 值才覆盖（空串表示清空）。两个原因：`api_key` 永不回读，前端编辑时手里没有
//! 旧值，全量替换会把它清空；模型/默认参数同理，只改 `base_url` 时不该丢。
//!
//! 生效配置是多层合并的结果：系统 → Peco → 团队（见 [`layers_for`]）。列表返回
//! 生效并集，每项标注它由哪一层定义（[`ProviderSource`]）——只列本团队文件会
//! 让继承来的条目看不见，像是没生效。`is_default` 与 `has_api_key` 按合并结果
//! 判定：「能跑」和「本层文件里写了 key」是两件事。

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use peco_core::config::{
    LlmApiParams, PROVIDER_TYPES, ProviderEntry, ProvidersConfig, SystemConfig, UserConfig,
};
use peco_core::provider::probe::ProbeOutcome;
use serde::{Deserialize, Serialize};

use crate::error::DaemonError;
use crate::space::SpaceEnv;
use crate::space::peco::PECO_TEAM_ID;

mod handler;

pub use handler::{delete, get, list, list_types, test_connection, test_draft, upsert};

/// 团队能力根下的 provider 配置文件。
pub const PROVIDERS_FILE: &str = "providers.toml";

/// provider 在列表/详情中的呈现。
///
/// `api_key` 永不回读 —— 只暴露是否已配置，避免凭据经 API 二次泄露。
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
    /// 该条目由哪一层定义 —— 只有 `"team"` 能在本团队文件里编辑/删除。
    pub source: ProviderSource,
}

/// provider 条目的定义层，也是列表里的 `source` 取值。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderSource {
    /// 团队自己的 `providers.toml`。
    Team,
    /// 保留团队 Peco 的 `providers.toml`（团队全局默认）。
    Peco,
    /// 系统层 `~/.config/peco/providers.toml`。
    System,
}

impl ProviderSource {
    /// 列表顺序：团队条目在前，其次 Peco，最后系统层。
    fn rank(self) -> u8 {
        match self {
            Self::Team => 0,
            Self::Peco => 1,
            Self::System => 2,
        }
    }

    /// 继承条目不可删时，告诉用户「这条是从哪继承来的」。
    fn origin_label(self) -> &'static str {
        match self {
            Self::Peco => "Peco",
            Self::System => "系统配置",
            Self::Team => "本团队",
        }
    }
}

/// 生效 provider 配置的一层：能力根 + 该层在列表里的来源标注。
#[derive(Debug, Clone)]
pub struct ProviderLayer {
    pub root: PathBuf,
    pub source: ProviderSource,
}

/// 一个团队的 provider 层级（低 → 高）。
///
/// Peco 自己只有两层：系统 + 自己，不自我继承 —— 否则 `teams/peco` 会被
/// 当成两层读同一份文件。
pub fn layers_for(env: &SpaceEnv, team_id: &str) -> Vec<ProviderLayer> {
    let team = ProviderLayer {
        root: env.team_root(team_id),
        source: ProviderSource::Team,
    };
    if team_id == PECO_TEAM_ID {
        return vec![team];
    }
    vec![
        ProviderLayer {
            root: env.team_root(PECO_TEAM_ID),
            source: ProviderSource::Peco,
        },
        team,
    ]
}

/// 交给 [`UserConfig::load_layered`] 的路径序列（低 → 高）。
pub fn layer_roots(layers: &[ProviderLayer]) -> Vec<PathBuf> {
    layers.iter().map(|layer| layer.root.clone()).collect()
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

/// `POST .../providers/test` 的请求体 — 保存前的表单值。
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

/// 用户文件里的 provider 配置。
///
/// 文件缺失是常态（团队还没配过 provider），此时给一份空表而不是报错：
/// 第一次 PUT 才有地方落笔。
///
/// 文件没写 `default_provider` 时留空串，而不是 serde 的默认值：空串在合并里
/// 表示「沿用低层默认」，写回时也不产生该键。填上默认值就把「本层没声明」变成
/// 一个真实默认位，团队保存任何一条 provider 都会静默顶掉低层选定的默认。
pub fn load_user_providers(path: &Path) -> Result<ProvidersConfig, DaemonError> {
    match std::fs::read_to_string(path) {
        Ok(raw) => parse_user_providers(path, &raw),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ProvidersConfig {
            default_provider: String::new(),
            providers: Default::default(),
            web_search: None,
        }),
        Err(e) => Err(DaemonError::Internal {
            message: format!("cannot read {}: {e}", path.display()),
        }),
    }
}

/// 解析一层用户文件，并把「没声明默认位」保留成空串。
///
/// 先解析成通用 TOML 再转结构体：`default_provider` 是否写了只能从原始文档判断，
/// 结构体里缺失键与 serde 填的默认值长得一模一样。
fn parse_user_providers(path: &Path, raw: &str) -> Result<ProvidersConfig, DaemonError> {
    let document: toml::Value = toml::from_str(raw).map_err(|e| invalid_file(path, &e))?;
    let declares_default = document.get("default_provider").is_some();
    let mut config: ProvidersConfig = document.try_into().map_err(|e| invalid_file(path, &e))?;
    if !declares_default {
        config.default_provider.clear();
    }
    Ok(config)
}

/// 损坏的用户文件是用户能自己修的输入错误 —— 400，不是 500。
fn invalid_file(path: &Path, source: &toml::de::Error) -> DaemonError {
    DaemonError::InvalidArgument {
        message: format!("invalid {}: {source}", path.display()),
    }
}

/// 合并后的生效配置（各层文件 + 系统兜底）。
///
/// 与对话路径读同一份：判定「凭据够不够用」「谁是默认」都必须看合并结果，
/// 只看原始文件会误报低层已补上的 key 未配置。
pub fn effective_config(
    system: &SystemConfig,
    layers: &[PathBuf],
) -> Result<UserConfig, DaemonError> {
    UserConfig::load_layered(system, layers).map_err(|e| DaemonError::Config(e.to_string()))
}

/// 生效配置 + 每个 provider 名的定义层（列表与详情的 `source` 都来自它）。
///
/// 先算来源：损坏的层文件由此报成 400（用户可修的输入错误），而不是等到
/// `load_layered` 才变成 config 错误。
pub fn effective_with_sources(
    system: &SystemConfig,
    layers: &[ProviderLayer],
) -> Result<(UserConfig, HashMap<String, ProviderSource>), DaemonError> {
    let mut sources = HashMap::new();
    // 从最高优先层往回走，先写入的赢：团队层覆盖 Peco 同名条目时，
    // 列表该显示「团队写的」。
    for layer in layers.iter().rev() {
        let config = load_user_providers(&layer.root.join(PROVIDERS_FILE))?;
        for name in config.providers.keys() {
            sources.entry(name.clone()).or_insert(layer.source);
        }
    }
    let effective = effective_config(system, &layer_roots(layers))?;
    Ok((effective, sources))
}

/// 该名字的定义层；没有任何层文件写过它 ⇒ 系统层。
pub fn source_of(sources: &HashMap<String, ProviderSource>, name: &str) -> ProviderSource {
    sources.get(name).copied().unwrap_or(ProviderSource::System)
}

/// 一条生效条目在合并后的生效配置里的呈现。
pub fn provider_info(
    name: &str,
    entry: &ProviderEntry,
    effective: &UserConfig,
    source: ProviderSource,
) -> ProviderInfo {
    ProviderInfo {
        name: name.to_string(),
        provider_type: entry.provider_type.clone(),
        base_url: entry.base_url.clone(),
        default_model: entry.default.as_ref().map(|d| d.model.clone()),
        api: entry.api.clone(),
        has_api_key: has_effective_api_key(effective, name),
        is_default: name == effective.default_provider_name(),
        source,
    }
}

/// 列表顺序：团队条目在前（按名），再 Peco、系统层（各按名）。
pub fn sort_for_display(infos: &mut [ProviderInfo]) {
    infos.sort_by(|a, b| {
        a.source
            .rank()
            .cmp(&b.source.rank())
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// 合并后的生效配置里该 provider 是否带凭据。
///
/// 用户文件与系统配置做深递归合并时 `api_key` 由系统层兜底，所以「能跑」与
/// 「用户文件里写了 key」是两回事；按原始文件判定会把实际可用的 provider 报成
/// 未配置。
fn has_effective_api_key(effective: &UserConfig, name: &str) -> bool {
    effective
        .provider_entry(Some(name))
        .and_then(|e| e.api_key.as_ref())
        .is_some()
}

/// 请求里显式给出的 provider 名，省略时回退为 `fallback`（调用方传目录里的规范
/// 类型名，使名字不会因大小写变体而变成 `OpenAI` 这种与类型不一致的 key）。
pub fn resolve_provider_name(req: &UpsertProviderRequest, fallback: &str) -> String {
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

/// 把一次 upsert 请求应用到用户配置上，返回生效的 provider 名。
///
/// 纯函数（不碰文件系统），字段级部分更新的全部语义都在这里：
/// 省略即保留、空串即清空、换类型即不继承、新条目可在满足条件时接管默认位。
///
/// `inherited_default` 是低层（见 [`layers_for`] 里本层之下的那些层）此刻已经
/// 给出的默认 provider，由调用方解析后传入 —— 判定它需要读文件系统，不属于本
/// 函数。`None` 表示低层没有可用的默认，此刻新增的首条条目才该接管默认位。
pub fn apply_upsert(
    config: &mut ProvidersConfig,
    req: &UpsertProviderRequest,
    inherited_default: Option<&str>,
) -> Result<String, DaemonError> {
    // 目录查找大小写不敏感，但适配器分派是精确匹配 —— 存目录里的规范值，
    // 否则 "OpenAI" 这类写法能通过校验，却在构建 Agent 时报 unsupported。
    ensure_supported_type(&req.provider_type)?;
    let Some(meta) = peco_core::config::provider_type_meta(&req.provider_type) else {
        // `ensure_supported_type` 与目录查询同源，走到这里说明两者漂移了。
        return Err(unsupported_type_error(&req.provider_type));
    };
    let provider_type = meta.provider_type.to_string();

    let name = resolve_provider_name(req, &provider_type);
    if name.is_empty() {
        return Err(DaemonError::InvalidArgument {
            message: "provider name must not be empty".into(),
        });
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
        return Err(DaemonError::InvalidArgument {
            message: format!(
                "unsupported api mode '{mode}' for provider type '{provider_type}' (supported: {})",
                meta.api_modes.join(", ")
            ),
        });
    }
    let default = merge_default_model(
        carried.as_ref().and_then(|e| e.default.clone()),
        req.default_model.clone(),
    );

    config.providers.insert(
        name.clone(),
        ProviderEntry {
            provider_type,
            api_key,
            base_url,
            api,
            default,
        },
    );

    // `set_default` 一律显式接管；首条新条目只在低层没有可继承的默认时才顺手
    // 接管 —— 否则「加好了却没有任何变化」会被静默换端点这件事替掉：团队本可以
    // 沿用低层默认，多写一个 provider 就把所有未指定 provider 的 Agent 迁走了。
    if req.set_default || (is_new && config.providers.len() == 1 && inherited_default.is_none()) {
        config.default_provider = name.clone();
    }

    Ok(name)
}

/// 从用户配置中删除一个 provider。
///
/// 默认 provider 被删除时回落到剩余条目里字典序最小的一个；一个都不剩则置空 ——
/// 空串在 `merge_providers_config` 中表示「交给系统层默认值」，是唯一不产生
/// 悬空引用的取值（留着被删的名字会让所有未指定 provider 的 Agent 报
/// "provider not found"，或在系统层有同名条目时静默换用系统凭据）。
pub fn apply_delete(config: &mut ProvidersConfig, name: &str) -> Result<(), DaemonError> {
    if config.providers.remove(name).is_none() {
        return Err(DaemonError::ProviderNotFound {
            name: name.to_string(),
        });
    }

    if config.default_provider == name {
        let mut remaining: Vec<&String> = config.providers.keys().collect();
        remaining.sort();
        config.default_provider = remaining.first().map(|s| (*s).clone()).unwrap_or_default();
    }
    Ok(())
}

/// 原子写：同目录临时文件 → `fsync` → `rename`。
///
/// 直接覆盖写会在中途失败时留下半个文件，下一次列表报解析错误、用户文件里
/// 其它 provider 也一并丢失。`rename` 在同一文件系统内是原子的：读者要么看到
/// 旧内容，要么看到新内容。失败路径删除临时文件，绝不碰原文件。
pub fn save_providers_atomic(path: &Path, config: &ProvidersConfig) -> Result<(), DaemonError> {
    let body = toml::to_string_pretty(config).map_err(|e| DaemonError::Internal {
        message: format!("cannot serialize providers: {e}"),
    })?;
    write_atomic(path, &temp_path(path), &body)
}

fn write_atomic(path: &Path, tmp: &Path, body: &str) -> Result<(), DaemonError> {
    let mut file = std::fs::File::create(tmp).map_err(|e| write_error(path, e))?;
    if let Err(e) = file
        .write_all(body.as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = std::fs::remove_file(tmp);
        return Err(write_error(path, e));
    }
    drop(file);
    if let Err(e) = std::fs::rename(tmp, path) {
        let _ = std::fs::remove_file(tmp);
        return Err(write_error(path, e));
    }
    Ok(())
}

fn write_error(path: &Path, source: std::io::Error) -> DaemonError {
    DaemonError::Internal {
        message: format!("cannot write {}: {source}", path.display()),
    }
}

/// 临时文件与目标同目录：`rename` 只在同一文件系统内原子。
///
/// 名字带唯一后缀，两个并发 PUT 不会互相踩对方的临时文件。
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.tmp.{}", uuid::Uuid::new_v4().simple()))
}

/// 请求里的 `type` 必须是适配器能分派的类型：写进不认识的类型，
/// 错误要到构建 Agent 时才暴露。
pub fn ensure_supported_type(provider_type: &str) -> Result<(), DaemonError> {
    if peco_core::config::is_supported_provider_type(provider_type) {
        Ok(())
    } else {
        Err(unsupported_type_error(provider_type))
    }
}

fn unsupported_type_error(provider_type: &str) -> DaemonError {
    DaemonError::InvalidArgument {
        message: format!(
            "unsupported provider type '{provider_type}' (supported: {})",
            supported_type_names()
        ),
    }
}

/// 合法 `type` 取值清单 — 错误文案与目录接口同源。
fn supported_type_names() -> String {
    PROVIDER_TYPES
        .iter()
        .map(|m| m.provider_type)
        .collect::<Vec<_>>()
        .join(", ")
}

/// 保存结果的文案：重载结论附在成功文案之后（写入本身已成功）。
fn saved_message(base: String, note: Option<String>) -> String {
    match note {
        Some(note) => format!("{base}, {note}"),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
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

    fn empty_config() -> ProvidersConfig {
        ProvidersConfig {
            default_provider: "deepseek".to_string(),
            providers: HashMap::new(),
            web_search: None,
        }
    }

    fn entry(provider_type: &str, key: Option<&str>, model: Option<&str>) -> ProviderEntry {
        ProviderEntry {
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

    /// 省略 name 时回退到类型名，显式别名优先，大小写变体回退到规范名。
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
        assert_eq!(
            resolve_provider_name(&req(None, "OpenAI"), "openai"),
            "openai"
        );
    }

    /// 省略字段保留旧值，显式空串清空。
    #[test]
    fn omitted_fields_keep_existing_values() {
        assert_eq!(
            merge_opt(Some("sk-old".into()), None),
            Some("sk-old".to_string())
        );
        assert_eq!(merge_opt(Some("sk-old".into()), Some("".into())), None);
        assert_eq!(
            merge_opt(Some("https://a".into()), Some(" https://b ".into())),
            Some("https://b".to_string())
        );
    }

    /// 省略 default_model 保留旧模型**及其余参数**；换模型同样保留其余参数。
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

        let changed = merge_default_model(old, Some("deepseek-v4-pro".into())).unwrap();
        assert_eq!(changed.model, "deepseek-v4-pro");
        assert_eq!(changed.temperature, Some(0.3));
    }

    /// 原本没有 default 段 → 新建只含 model 的段；空串不创建空段。
    #[test]
    fn model_creates_default_section() {
        let created = merge_default_model(None, Some(" gpt-5.2 ".into())).unwrap();
        assert_eq!(created.model, "gpt-5.2");
        assert!(created.temperature.is_none());
        assert!(merge_default_model(None, Some("  ".into())).is_none());
    }

    /// 只改 base_url 不丢 api_key / 模型 —— 字段级部分更新的核心诉求。
    #[test]
    fn partial_update_keeps_key_and_model() {
        let mut config = empty_config();
        config.providers.insert(
            "gateway".into(),
            entry("openai", Some("sk-old"), Some("gpt-5.2")),
        );

        let mut r = req(Some("gateway"), "openai");
        r.base_url = Some("https://proxy.example.com/v1".into());
        let name = apply_upsert(&mut config, &r, None).unwrap();

        assert_eq!(name, "gateway");
        let stored = &config.providers["gateway"];
        assert_eq!(
            stored.base_url.as_deref(),
            Some("https://proxy.example.com/v1")
        );
        assert_eq!(stored.api_key.as_deref(), Some("sk-old"));
        assert_eq!(
            stored.default.as_ref().map(|d| d.model.as_str()),
            Some("gpt-5.2")
        );
    }

    /// 换类型不继承旧端点与凭据：否则 A 家的 key 会被发到 B 家的地址上。
    #[test]
    fn type_switch_drops_inherited_fields() {
        let mut config = empty_config();
        config.providers.insert(
            "gateway".into(),
            entry("openai", Some("sk-openai"), Some("gpt-5.2")),
        );

        let mut r = req(Some("gateway"), "qwen");
        r.default_model = Some("qwen3.7-max".into());
        apply_upsert(&mut config, &r, None).unwrap();

        let stored = &config.providers["gateway"];
        assert_eq!(stored.provider_type, "qwen");
        assert!(stored.api_key.is_none());
        assert!(
            stored
                .default
                .as_ref()
                .is_some_and(|d| d.model == "qwen3.7-max" && d.temperature.is_none())
        );
    }

    /// 首次新增（表原本为空）且低层没有可继承的默认 → 自动接管默认位。
    #[test]
    fn first_provider_becomes_default_when_nothing_to_inherit() {
        let mut config = empty_config();
        let mut r = req(Some("openai"), "openai");
        r.api_key = Some("sk-openai".into());

        let name = apply_upsert(&mut config, &r, None).unwrap();

        assert_eq!(name, "openai");
        assert_eq!(config.default_provider, "openai");
    }

    /// 低层已有可继承的默认 ⇒ 首条新增不接管，本层文件里保持「没声明默认位」。
    #[test]
    fn first_provider_keeps_the_inherited_default() {
        let mut config = empty_config();
        config.default_provider.clear();
        let mut r = req(Some("qwen-side"), "qwen");
        r.api_key = Some("sk-qwen".into());

        let name = apply_upsert(&mut config, &r, Some("peco-shared")).unwrap();

        assert_eq!(name, "qwen-side");
        assert_eq!(
            config.default_provider, "",
            "没接管就该保持未声明，保存时才不会写出 default_provider"
        );

        r.set_default = true;
        apply_upsert(&mut config, &r, Some("peco-shared")).unwrap();
        assert_eq!(
            config.default_provider, "qwen-side",
            "显式 set_default 一定接管，与低层有没有默认无关"
        );
    }

    /// 低层有默认但不是首条新增 ⇒ 同样不接管。
    #[test]
    fn inherited_default_is_irrelevant_for_later_entries() {
        let mut config = empty_config();
        config
            .providers
            .insert("gateway".into(), entry("openai", Some("sk"), None));
        config.default_provider.clear();

        apply_upsert(&mut config, &req(Some("qwen"), "qwen"), None).unwrap();

        assert_eq!(config.default_provider, "");
    }

    /// 非首个新增不抢默认位；显式 `set_default` 才接管。
    #[test]
    fn second_provider_does_not_steal_default() {
        let mut config = empty_config();
        config
            .providers
            .insert("gateway".into(), entry("openai", Some("sk"), None));
        config.default_provider = "gateway".into();

        let mut r = req(Some("qwen"), "qwen");
        r.api_key = Some("sk-qwen".into());
        apply_upsert(&mut config, &r, None).unwrap();
        assert_eq!(config.default_provider, "gateway");

        r.set_default = true;
        apply_upsert(&mut config, &r, None).unwrap();
        assert_eq!(config.default_provider, "qwen");
    }

    /// 非法类型 ⇒ 400 可控错误，文案列出合法取值，且不写坏配置。
    #[test]
    fn unsupported_type_is_a_client_error() {
        let mut config = empty_config();
        let err = apply_upsert(&mut config, &req(None, "anthropic"), None).unwrap_err();
        assert_eq!(err.code(), "invalid_argument");
        assert_eq!(err.http_status(), StatusCode::BAD_REQUEST);
        assert!(err.to_string().contains("deepseek"), "{err}");
        assert!(config.providers.is_empty(), "校验失败不得写入任何条目");
    }

    /// 类型不支持的 api 档位在写盘前拒绝。
    #[test]
    fn unsupported_api_mode_rejected() {
        let mut config = empty_config();
        let mut r = req(Some("qwen"), "qwen");
        r.api = Some("anthropic".into());
        let err = apply_upsert(&mut config, &r, None).unwrap_err();
        assert_eq!(err.code(), "invalid_argument");
        assert!(err.to_string().contains("qwen"), "{err}");
    }

    /// 删除默认 provider 后回落到剩余条目里字典序最小的一个；删空则置空。
    #[test]
    fn delete_default_falls_back() {
        let mut config = empty_config();
        config
            .providers
            .insert("zeta".into(), entry("openai", None, None));
        config
            .providers
            .insert("alpha".into(), entry("openai", None, None));
        config.default_provider = "zeta".into();

        apply_delete(&mut config, "zeta").unwrap();
        assert_eq!(config.default_provider, "alpha");

        apply_delete(&mut config, "alpha").unwrap();
        assert_eq!(config.default_provider, "");
        assert!(config.providers.is_empty());
    }

    /// 删除不存在的条目可辨认成 404，而不是静默成功。
    #[test]
    fn delete_unknown_is_not_found() {
        let mut config = empty_config();
        let err = apply_delete(&mut config, "ghost").unwrap_err();
        assert_eq!(err.code(), "provider_not_found");
        assert_eq!(err.http_status(), StatusCode::NOT_FOUND);
    }

    /// 临时文件必须与目标同目录（跨文件系统的 rename 不是原子操作）。
    #[test]
    fn temp_path_sits_next_to_target() {
        let target = Path::new("/tmp/teams/research/providers.toml");
        let tmp = temp_path(target);
        assert_eq!(tmp.parent(), target.parent());
        let name = tmp.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".providers.toml.tmp."), "{name}");
    }

    /// 写盘中途失败不得损坏原文件 —— 临时文件建不出来时一个字节都不该动。
    #[test]
    fn failed_write_leaves_original_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PROVIDERS_FILE);
        let original = "default_provider = \"keep\"\n";
        std::fs::write(&path, original).unwrap();

        // 同名的目录占住临时文件位置：`File::create` 必然失败（以 root 跑也一样）。
        let tmp = dir.path().join(".providers.toml.tmp.blocked");
        std::fs::create_dir(&tmp).unwrap();

        let err = write_atomic(&path, &tmp, "default_provider = \"clobbered\"\n").unwrap_err();
        assert_eq!(err.code(), "internal");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "写失败后原文件必须原样保留"
        );
    }

    /// 写成功路径：整份内容一次到位，且不留临时文件。
    #[test]
    fn successful_write_replaces_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PROVIDERS_FILE);
        let mut config = empty_config();
        config.providers.insert(
            "gateway".into(),
            entry("openai", Some("sk"), Some("gpt-5.2")),
        );
        config.default_provider = "gateway".into();

        save_providers_atomic(&path, &config).unwrap();

        let reloaded = load_user_providers(&path).unwrap();
        assert_eq!(reloaded.default_provider, "gateway");
        assert_eq!(reloaded.providers["gateway"].api_key.as_deref(), Some("sk"));
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "残留临时文件：{leftovers:?}");
    }

    /// 文件缺失时给空表（团队还没配过 provider 是常态，不是错误）。
    ///
    /// 默认位留空：没写这个键的层不声明默认，保存时也就不该写出它。
    #[test]
    fn missing_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_user_providers(&dir.path().join(PROVIDERS_FILE)).unwrap();
        assert!(config.providers.is_empty());
        assert_eq!(config.default_provider, "");
    }

    /// 文件写了 `default_provider` 才是本层声明的默认位，读回时必须保留。
    #[test]
    fn declared_default_provider_survives_a_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PROVIDERS_FILE);
        std::fs::write(
            &path,
            "default_provider = \"gateway\"\n\n[providers.gateway]\ntype = \"openai\"\n",
        )
        .unwrap();

        let config = load_user_providers(&path).unwrap();
        assert_eq!(config.default_provider, "gateway");
    }

    /// 没写 `default_provider` 的文件读回来仍是「未声明」，不是 serde 的默认值。
    #[test]
    fn undeclared_default_provider_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PROVIDERS_FILE);
        std::fs::write(&path, "[providers.gateway]\ntype = \"openai\"\n").unwrap();

        let config = load_user_providers(&path).unwrap();
        assert_eq!(config.default_provider, "");
    }

    /// 损坏的用户文件是用户可修的输入错误 ⇒ 400，不是 500。
    #[test]
    fn corrupt_file_is_a_client_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PROVIDERS_FILE);
        std::fs::write(&path, "providers = [not toml").unwrap();
        let err = load_user_providers(&path).unwrap_err();
        assert_eq!(err.code(), "invalid_argument");
    }

    /// 目录接口与 core 的目录同源。
    #[test]
    fn type_catalog_matches_core() {
        let infos: Vec<ProviderTypeInfo> = PROVIDER_TYPES
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
        assert_eq!(infos.len(), PROVIDER_TYPES.len());
        let ds = infos
            .iter()
            .find(|i| i.provider_type == "deepseek")
            .expect("deepseek 必须在目录里");
        assert_eq!(ds.default_base_url, "https://api.deepseek.com");
    }

    /// 保存成功文案把重载结论拼在同一条 message 里。
    #[test]
    fn saved_message_appends_reload_note() {
        assert_eq!(
            saved_message("Provider 'x' saved".into(), None),
            "Provider 'x' saved"
        );
        assert_eq!(
            saved_message("Provider 'x' saved".into(), Some("reloaded 2".into())),
            "Provider 'x' saved, reloaded 2"
        );
    }
}
