//! `/api/providers`（系统层）与 `/api/teams/{team_id}/providers`（团队层）——
//! provider 配置的 HTTP 层。
//!
//! 只做「请求 → 领域调用 → 响应」：配置语义在 [`super`] 里，这里负责作用域的
//! 存在性、写盘后的实例重载与错误形状。两个作用域共用同一批领域函数，差别只有
//! 目标路径、可以写哪一层、以及写后要刷哪些实例。

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use peco_core::config::{PROVIDER_TYPES, ProvidersConfig, SystemConfig, UserConfig};
use peco_core::provider::probe::{ProbeTarget, run_probe};
use tracing::{info, warn};

use super::{
    PROVIDERS_FILE, ProviderInfo, ProviderLayer, ProviderSource, ProviderTypeInfo, ReloadScope,
    SuccessResponse, TestDraftRequest, TestResponse, UpsertProviderRequest, apply_delete,
    apply_upsert, effective_with_sources, ensure_supported_type, layers_for, load_user_providers,
    provider_info, save_providers_atomic, saved_message, sort_for_display, source_of, system_layer,
};
use crate::db::workspaces;
use crate::error::DaemonError;
use crate::state::AppState;

/// 各 provider 类型的默认值目录 — 两个作用域的 `types` 共用这一份。
///
/// 目录与团队/系统无关（内容来自 `PROVIDER_TYPES`），抽成纯函数是为了让系统层
/// 那条路由不依赖「团队存不存在」：空 home 下团队根还没建，系统层照样要能列类型。
fn types_catalog() -> Vec<ProviderTypeInfo> {
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
        .collect()
}

/// `GET /api/teams/{team_id}/providers/types` — 各 provider 类型的默认值目录。
pub async fn list_types(
    UrlPath(team_id): UrlPath<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<ProviderTypeInfo>>, DaemonError> {
    team_root(&state, &team_id)?;
    Ok(Json(types_catalog()))
}

/// `GET /api/providers/types` — 系统层的类型目录（与团队层同源）。
pub async fn types_system(State(_state): State<AppState>) -> Json<Vec<ProviderTypeInfo>> {
    Json(types_catalog())
}

/// `GET /api/providers` — 系统层文件里的条目（`source` 恒为 `system`）。
///
/// `is_default` / `has_api_key` 只看**系统文件自己**：合并视角的默认会随任何一个
/// 团队文件的改动而变，同一份「系统层列表」在两次刷新之间改变含义，用户就无从
/// 判断自己改的是哪一层。
pub async fn list_system(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProviderInfo>>, DaemonError> {
    let layers = [system_layer(&state.env)];
    let (effective, _) = effective_with_sources(&state.env.system_config, &layers)?;

    let mut infos: Vec<ProviderInfo> = effective
        .providers
        .providers
        .iter()
        .map(|(name, entry)| provider_info(name, entry, &effective, ProviderSource::System))
        .collect();
    sort_for_display(&mut infos);
    Ok(Json(infos))
}

/// `PUT /api/providers` — 新增或部分更新系统层的一条。
///
/// 系统层是最底层，没有「从低层继承来的默认」可言 ⇒ `apply_upsert` 的
/// `inherited_default` 传 `None`，首条新增即可接管默认位。
pub async fn upsert_system(
    State(state): State<AppState>,
    Json(req): Json<UpsertProviderRequest>,
) -> Result<Json<SuccessResponse>, DaemonError> {
    let path = system_root(&state).join(PROVIDERS_FILE);
    let mut config = load_user_providers(&path)?;
    let name = apply_upsert(&mut config, &req, None)?;

    persist(
        &state,
        &path,
        ReloadScope::All,
        &config,
        format!("Provider '{name}' saved"),
    )
    .await
}

/// `DELETE /api/providers/{name}` — 删除系统层的一条。
///
/// 删掉的若是默认条目，`apply_delete` 会把默认改成剩余里字典序最小的一条：系统层
/// 是所有团队的公共兜底，这个静默变更必须写进成功文案，否则用户只知道「删掉
/// 了」，不知道「默认换成谁了、影响谁」。
pub async fn delete_system(
    UrlPath(name): UrlPath<String>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse>, DaemonError> {
    let path = system_root(&state).join(PROVIDERS_FILE);
    let mut config = load_user_providers(&path)?;
    if !config.providers.contains_key(&name) {
        return Err(DaemonError::ProviderNotFound { name });
    }
    let was_default = config.default_provider == name;
    apply_delete(&mut config, &name)?;

    let message = deleted_message(&name, was_default, &config);
    persist(&state, &path, ReloadScope::All, &config, message).await
}

/// `POST /api/providers/test` — 用表单当前值发真实探针（系统层，不落盘）。
pub async fn test_draft_system(
    State(_state): State<AppState>,
    Json(req): Json<TestDraftRequest>,
) -> Result<Json<TestResponse>, DaemonError> {
    probe_draft(req).await
}

/// `POST /api/providers/{name}/test` — 用系统层已保存的配置发探针。
pub async fn test_connection_system(
    UrlPath(name): UrlPath<String>,
    State(state): State<AppState>,
) -> Result<Json<TestResponse>, DaemonError> {
    let layers = [system_layer(&state.env)];
    let (effective, _) = effective_with_sources(&state.env.system_config, &layers)?;
    probe_saved(&name, &effective).await
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
        &path,
        ReloadScope::for_team(&team_id),
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
        &path,
        ReloadScope::for_team(&team_id),
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
    probe_draft(req).await
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
    probe_saved(&name, &effective).await
}

/// 用表单当前值发探针：两个作用域只差作用域本身的校验。
async fn probe_draft(req: TestDraftRequest) -> Result<Json<TestResponse>, DaemonError> {
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

/// 用生效配置里该名字的条目发探针：走与对话同一份合并结果。
async fn probe_saved(
    name: &str,
    effective: &UserConfig,
) -> Result<Json<TestResponse>, DaemonError> {
    let entry =
        effective
            .provider_entry(Some(name))
            .ok_or_else(|| DaemonError::ProviderNotFound {
                name: name.to_string(),
            })?;

    let outcome = run_probe(ProbeTarget {
        name: name.to_string(),
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

/// 系统层文件的目录：`$PECO_HOME`（或 `PECO_PROVIDERS_CONFIG` 的父目录）。
///
/// 不做「存在性」校验：系统层的存在性由 `$PECO_HOME` 决定，启动时已
/// `create_dir_all`，没有「团队不存在」这一档。
fn system_root(state: &AppState) -> PathBuf {
    state.env.system_providers_root().to_path_buf()
}

/// 系统层删除成功后的文案：默认位的去向必须写出来。
///
/// 系统层是所有团队的公共兜底，`apply_delete` 又会**静默**把默认改成剩余里字典序
/// 最小的一条 —— 不写出来，用户只看到「删掉了」，看不到默认被换到了谁身上。
fn deleted_message(name: &str, was_default: bool, remaining: &ProvidersConfig) -> String {
    if !was_default {
        return format!("Provider '{name}' deleted");
    }
    match remaining.providers.keys().min().map(|next| next.as_str()) {
        Some(next) => format!(
            "Provider '{name}' deleted: it was the system default, the default is now '{next}' (every team is affected)"
        ),
        None => format!(
            "Provider '{name}' deleted: the system layer has no provider left, teams without their own provider will fail on the next message"
        ),
    }
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

/// 写盘 → 让受影响实例用上新配置 → 拼文案。
///
/// 重载失败不当作保存失败：文件此刻已经落盘，报错只会让用户反复重试一个
/// 已经生效的写入，失败原因作为警告文案回给调用方。
async fn persist(
    state: &AppState,
    path: &Path,
    scope: ReloadScope,
    config: &ProvidersConfig,
    base: String,
) -> Result<Json<SuccessResponse>, DaemonError> {
    save_providers_atomic(path, config)?;
    let outcome = reload_open_spaces(state, &scope).await;
    info!(path = %path.display(), scope = ?scope, "provider config saved");

    Ok(Json(SuccessResponse {
        success: true,
        message: Some(saved_message(base, outcome.note())),
    }))
}

/// 一次写后重载的结论：刷了几个空间、几个 console、哪些失败。
#[derive(Debug, Default)]
struct ReloadOutcome {
    reloaded: usize,
    consoles_reloaded: usize,
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
        match (self.reloaded, self.consoles_reloaded) {
            (0, 0) => None,
            (spaces, 0) => Some(format!("reloaded {spaces} open space(s)")),
            (0, consoles) => Some(format!("reloaded {consoles} team console(s)")),
            (spaces, consoles) => Some(format!(
                "reloaded {spaces} open space(s), {consoles} team console(s)"
            )),
        }
    }
}

/// 写后刷新受影响的已打开实例。
///
/// 系统层与 Peco 层会影响所有团队（它们都继承这两层），只刷自己等于让其余团队
/// 一直拿着旧快照；改别的团队只影响自己。作用域由调用方显式给出，不再从
/// 「team_id 等于 Peco」里推断。
///
/// console 也必须刷：它持有自己的一份 provider 层列表，却不在 Hub 里 ——
/// 只查已打开空间列表时，只有 console、没有空间的团队永远看不到改动。
async fn reload_open_spaces(state: &AppState, scope: &ReloadScope) -> ReloadOutcome {
    let mut outcome = ReloadOutcome::default();
    match open_dirs(state, scope).await {
        Ok(dirs) => {
            for dir in dirs {
                // 库里有行不代表实例还活着：`WorkspaceHub` 是唯一实例表。
                let Some(handle) = state.hub.get(Path::new(&dir)) else {
                    continue;
                };
                match handle.workspace().reload_providers() {
                    Ok(invalidated) => {
                        outcome.reloaded += 1;
                        info!(scope = ?scope, work_dir = %dir, invalidated, "provider config reloaded");
                    }
                    Err(e) => {
                        warn!(scope = ?scope, work_dir = %dir, error = %e, "provider reload failed");
                        outcome.failures.push(format!("{dir}: {e}"));
                    }
                }
            }
        }
        Err(message) => outcome.failures.push(message),
    }

    let consoles = state.spaces.reload_console_providers(scope);
    outcome.consoles_reloaded = consoles.reloaded;
    outcome.failures.extend(consoles.failures);
    outcome
}

/// 该作用域下的已打开空间目录；列不出来时返回值就是给用户看的那句失败文案。
async fn open_dirs(state: &AppState, scope: &ReloadScope) -> Result<Vec<String>, String> {
    let dirs = match scope {
        ReloadScope::All => workspaces::open_work_dirs(&state.pool).await,
        ReloadScope::Team(team_id) => {
            workspaces::open_work_dirs_by_team(&state.pool, team_id).await
        }
    };
    dirs.map_err(|e| {
        warn!(scope = ?scope, error = %e, "cannot list open spaces; skipping provider reload");
        format!("cannot list open spaces: {e}")
    })
}
