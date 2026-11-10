//! `/api/teams/{team_id}/providers` — 团队级 provider 配置的 HTTP 层。
//!
//! 只做「请求 → 领域调用 → 响应」：配置语义在 [`super`] 里，这里负责团队存在性、
//! 写盘后的实例重载与错误形状。

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use peco_core::config::{PROVIDER_TYPES, ProvidersConfig, SystemConfig};
use peco_core::provider::probe::{ProbeTarget, run_probe};
use tracing::{info, warn};

use super::{
    PROVIDERS_FILE, ProviderInfo, ProviderLayer, ProviderTypeInfo, SuccessResponse,
    TestDraftRequest, TestResponse, UpsertProviderRequest, apply_delete, apply_upsert,
    effective_with_sources, ensure_supported_type, layers_for, load_user_providers, provider_info,
    save_providers_atomic, saved_message, sort_for_display, source_of,
};
use crate::db::workspaces;
use crate::error::DaemonError;
use crate::space::peco::PECO_TEAM_ID;
use crate::state::AppState;

/// `GET /api/teams/{team_id}/providers/types` — 各 provider 类型的默认值目录。
pub async fn list_types(
    UrlPath(team_id): UrlPath<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<ProviderTypeInfo>>, DaemonError> {
    team_root(&state, &team_id)?;
    Ok(Json(
        PROVIDER_TYPES
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

/// `GET /api/teams/{team_id}/providers` — 生效并集，每项标注定义层。
///
/// 只列本团队文件会让继承来的条目看不见 —— 用户以为配好的 provider 没生效。
pub async fn list(
    UrlPath(team_id): UrlPath<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<ProviderInfo>>, DaemonError> {
    team_root(&state, &team_id)?;
    let layers = layers_for(&state.env, &team_id);
    let (effective, sources) = effective_with_sources(&state.env.system_config, &layers)?;

    let mut infos: Vec<ProviderInfo> = effective
        .providers
        .providers
        .iter()
        .map(|(name, entry)| provider_info(name, entry, &effective, source_of(&sources, name)))
        .collect();
    sort_for_display(&mut infos);
    Ok(Json(infos))
}

/// `GET /api/teams/{team_id}/providers/{name}` — 单条详情（含继承条目）。
pub async fn get(
    UrlPath((team_id, name)): UrlPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ProviderInfo>, DaemonError> {
    team_root(&state, &team_id)?;
    let layers = layers_for(&state.env, &team_id);
    let (effective, sources) = effective_with_sources(&state.env.system_config, &layers)?;
    let entry = effective
        .providers
        .providers
        .get(&name)
        .ok_or_else(|| DaemonError::ProviderNotFound { name: name.clone() })?;

    Ok(Json(provider_info(
        &name,
        entry,
        &effective,
        source_of(&sources, &name),
    )))
}

/// `PUT /api/teams/{team_id}/providers` — 新增或部分更新一个 provider。
pub async fn upsert(
    UrlPath(team_id): UrlPath<String>,
    State(state): State<AppState>,
    Json(req): Json<UpsertProviderRequest>,
) -> Result<Json<SuccessResponse>, DaemonError> {
    let root = team_root(&state, &team_id)?;
    let path = root.join(PROVIDERS_FILE);
    let mut config = load_user_providers(&path)?;

    let layers = layers_for(&state.env, &team_id);
    let inherited = inheritable_default(&state.env.system_config, &layers)?;
    let name = apply_upsert(&mut config, &req, inherited.as_deref())?;

    persist(
        &state,
        &team_id,
        &path,
        &config,
        format!("Provider '{name}' saved"),
    )
    .await
}

/// `DELETE /api/teams/{team_id}/providers/{name}` — 删除本团队文件里的一条。
///
/// 只在继承层存在的名字删不掉：那层不属于这个团队，删了也不会消失。此时报
/// `inherited_provider` 而不是 `provider_not_found` —— 后者会让用户以为配置坏了。
pub async fn delete(
    UrlPath((team_id, name)): UrlPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse>, DaemonError> {
    let root = team_root(&state, &team_id)?;
    let path = root.join(PROVIDERS_FILE);
    let mut config = load_user_providers(&path)?;
    if !config.providers.contains_key(&name) {
        let layers = layers_for(&state.env, &team_id);
        let (effective, sources) = effective_with_sources(&state.env.system_config, &layers)?;
        return Err(if effective.providers.providers.contains_key(&name) {
            DaemonError::InheritedProvider {
                name: name.clone(),
                origin: source_of(&sources, &name).origin_label(),
            }
        } else {
            DaemonError::ProviderNotFound { name }
        });
    }
    apply_delete(&mut config, &name)?;

    persist(
        &state,
        &team_id,
        &path,
        &config,
        format!("Provider '{name}' deleted"),
    )
    .await
}

/// `POST /api/teams/{team_id}/providers/test` — 用**表单当前值**发真实探针。
///
/// 保存前的试一下：不落盘、不碰已打开实例，因此字段没有已存值可回退。
pub async fn test_draft(
    UrlPath(team_id): UrlPath<String>,
    State(state): State<AppState>,
    Json(req): Json<TestDraftRequest>,
) -> Result<Json<TestResponse>, DaemonError> {
    team_root(&state, &team_id)?;
    ensure_supported_type(&req.provider_type)?;

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

/// `POST /api/teams/{team_id}/providers/{name}/test` — 用**已保存**的配置发探针。
///
/// 测的是合并后的生效配置（用户文件 + 系统兜底），与对话实际使用的一致。
pub async fn test_connection(
    UrlPath((team_id, name)): UrlPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<TestResponse>, DaemonError> {
    team_root(&state, &team_id)?;
    let layers = layers_for(&state.env, &team_id);
    let (effective, _) = effective_with_sources(&state.env.system_config, &layers)?;
    let entry = effective
        .provider_entry(Some(&name))
        .ok_or_else(|| DaemonError::ProviderNotFound { name: name.clone() })?;

    let outcome = run_probe(ProbeTarget {
        name,
        provider_type: entry.provider_type.clone(),
        api_key: entry.api_key.clone(),
        base_url: entry.base_url.clone(),
        api: entry.api.clone(),
        model: entry.default.as_ref().map(|d| d.model.clone()),
    })
    .await;

    Ok(Json(outcome.into()))
}

// ── 内部辅助 ────────────────────────────────────────────────────────────────

/// 团队能力根；团队不存在直接 404（写盘与列表都不该凭空建出团队目录）。
fn team_root(state: &AppState, team_id: &str) -> Result<PathBuf, DaemonError> {
    if !state.env.team_exists(team_id) {
        return Err(DaemonError::TeamNotFound {
            team_id: team_id.to_string(),
        });
    }
    Ok(state.env.team_root(team_id))
}

/// 低层此刻是否已经给出可继承的默认 provider。
///
/// 只看本层之下的层（[`layers_for`] 的最后一层是本团队，Peco 团队自己身上则是
/// 系统层）：要判定的是「接管默认位会不会顶掉别人」，本层文件写了什么与此无关。
/// 低层写了 `default_provider` 但低层 providers 里没有这个名字时不算 —— 那个
/// 名字指不到任何端点，团队首条条目仍该接管。
fn inheritable_default(
    system: &SystemConfig,
    layers: &[ProviderLayer],
) -> Result<Option<String>, DaemonError> {
    let Some((_, lower)) = layers.split_last() else {
        return Ok(None);
    };
    // 走与列表/详情同一条读法：低层文件损坏报 400（用户可修的输入错误），
    // 而不是 PUT 时才冒出一个 500。
    let (config, _) = effective_with_sources(system, lower)?;
    let name = config.default_provider_name();
    Ok(config
        .providers
        .providers
        .contains_key(name)
        .then(|| name.to_string()))
}

/// 写盘 → 让已打开实例用上新配置 → 拼文案。
///
/// 重载失败不当作保存失败：文件此刻已经落盘，报错只会让用户反复重试一个
/// 已经生效的写入，失败原因作为警告文案回给调用方。
async fn persist(
    state: &AppState,
    team_id: &str,
    path: &Path,
    config: &ProvidersConfig,
    base: String,
) -> Result<Json<SuccessResponse>, DaemonError> {
    save_providers_atomic(path, config)?;
    let outcome = reload_open_spaces(state, team_id).await;
    info!(team_id, path = %path.display(), "provider config saved");

    Ok(Json(SuccessResponse {
        success: true,
        message: Some(saved_message(base, outcome.note())),
    }))
}

/// 一次写后重载的结论：刷了几个实例、哪些失败。
#[derive(Debug, Default)]
struct ReloadOutcome {
    reloaded: usize,
    failures: Vec<String>,
}

impl ReloadOutcome {
    /// 附在成功文案之后的一句结论；没有实例可刷也没有失败时不制造噪声。
    fn note(&self) -> Option<String> {
        if !self.failures.is_empty() {
            return Some(format!(
                "but hot reload failed for {} space(s): {}. Changes take effect after restart",
                self.failures.len(),
                self.failures.join("; ")
            ));
        }
        (self.reloaded > 0).then(|| format!("reloaded {} open space(s)", self.reloaded))
    }
}

/// 写后刷新受影响的已打开空间。
///
/// 改 Peco 层会影响所有团队（它们都继承 Peco），只刷 Peco 自己的空间等于
/// 让其余团队一直拿着旧快照；改别的团队只影响自己。
async fn reload_open_spaces(state: &AppState, team_id: &str) -> ReloadOutcome {
    let dirs = if team_id == PECO_TEAM_ID {
        workspaces::open_work_dirs(&state.pool).await
    } else {
        workspaces::open_work_dirs_by_team(&state.pool, team_id).await
    };
    let dirs = match dirs {
        Ok(dirs) => dirs,
        Err(e) => {
            warn!(team_id, error = %e, "cannot list open spaces; skipping provider reload");
            return ReloadOutcome {
                failures: vec![format!("cannot list open spaces: {e}")],
                ..Default::default()
            };
        }
    };

    let mut outcome = ReloadOutcome::default();
    for dir in dirs {
        // 库里有行不代表实例还活着：`WorkspaceHub` 是唯一实例表。
        let Some(handle) = state.hub.get(Path::new(&dir)) else {
            continue;
        };
        match handle.workspace().reload_providers() {
            Ok(invalidated) => {
                outcome.reloaded += 1;
                info!(team_id, work_dir = %dir, invalidated, "provider config reloaded");
            }
            Err(e) => {
                warn!(team_id, work_dir = %dir, error = %e, "provider reload failed");
                outcome.failures.push(format!("{dir}: {e}"));
            }
        }
    }
    outcome
}
