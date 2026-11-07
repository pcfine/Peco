//! `/api/teams/{team_id}/providers` — 团队级 provider 配置的 HTTP 层。
//!
//! 只做「请求 → 领域调用 → 响应」：配置语义在 [`super`] 里，这里负责团队存在性、
//! 写盘后的实例重载与错误形状。

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use peco_core::config::{PROVIDER_TYPES, ProvidersConfig};
use peco_core::provider::probe::{ProbeTarget, run_probe};
use tracing::{info, warn};

use super::{
    PROVIDERS_FILE, ProviderInfo, ProviderTypeInfo, SuccessResponse, TestDraftRequest,
    TestResponse, UpsertProviderRequest, apply_delete, apply_upsert, effective_config,
    ensure_supported_type, load_user_providers, provider_info, save_providers_atomic,
    saved_message,
};
use crate::db::workspaces;
use crate::error::DaemonError;
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

/// `GET /api/teams/{team_id}/providers` — 用户 providers.toml 中的条目。
pub async fn list(
    UrlPath(team_id): UrlPath<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<ProviderInfo>>, DaemonError> {
    let root = team_root(&state, &team_id)?;
    let user = load_user_providers(&root.join(PROVIDERS_FILE))?;
    let effective = effective_config(&state.env.system_config, &root)?;

    Ok(Json(
        user.providers
            .iter()
            .map(|(name, entry)| provider_info(name, entry, &effective))
            .collect(),
    ))
}

/// `GET /api/teams/{team_id}/providers/{name}` — 单条详情。
pub async fn get(
    UrlPath((team_id, name)): UrlPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ProviderInfo>, DaemonError> {
    let root = team_root(&state, &team_id)?;
    let user = load_user_providers(&root.join(PROVIDERS_FILE))?;
    let entry = user
        .providers
        .get(&name)
        .ok_or_else(|| DaemonError::ProviderNotFound { name: name.clone() })?;
    let effective = effective_config(&state.env.system_config, &root)?;

    Ok(Json(provider_info(&name, entry, &effective)))
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

    let name = apply_upsert(&mut config, &req)?;

    persist(
        &state,
        &team_id,
        &path,
        &config,
        format!("Provider '{name}' saved"),
    )
    .await
}

/// `DELETE /api/teams/{team_id}/providers/{name}` — 删除一个 provider。
pub async fn delete(
    UrlPath((team_id, name)): UrlPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse>, DaemonError> {
    let root = team_root(&state, &team_id)?;
    let path = root.join(PROVIDERS_FILE);
    let mut config = load_user_providers(&path)?;
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
    let root = team_root(&state, &team_id)?;
    let effective = effective_config(&state.env.system_config, &root)?;
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

/// 写后刷新该团队**已打开**的空间，让已缓存 Agent 重建。
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

async fn reload_open_spaces(state: &AppState, team_id: &str) -> ReloadOutcome {
    let dirs = match workspaces::open_work_dirs_by_team(&state.pool, team_id).await {
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
