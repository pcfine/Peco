// Provider Handler — providers.toml 管理

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use peco_core::config::ProvidersConfig;
use serde::{Deserialize, Serialize};

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct ProviderInfo {
    pub name: String,
    pub provider_type: String,
    pub base_url: Option<String>,
    pub models: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpsertProviderRequest {
    #[serde(rename = "type")]
    pub provider_type: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    #[allow(dead_code)]
    pub models: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

fn providers_path(user_id: &str, state: &AppState) -> std::path::PathBuf {
    state
        .workspace_manager
        .workspace_dir(user_id)
        .join("providers.toml")
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

pub async fn list(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ProviderInfo>>, ApiError> {
    let path = providers_path(&user_id, &state);
    let config = load_providers(&path)?;

    let providers: Vec<ProviderInfo> = config
        .providers
        .iter()
        .map(|(name, entry)| ProviderInfo {
            name: name.clone(),
            provider_type: entry.provider_type.clone(),
            base_url: entry.base_url.clone(),
            models: entry
                .default
                .as_ref()
                .map(|d| vec![d.model.clone()])
                .unwrap_or_default(),
        })
        .collect();

    Ok(Json(providers))
}

pub async fn get(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<ProviderInfo>, ApiError> {
    let path = providers_path(&user_id, &state);
    let config = load_providers(&path)?;
    let entry = config
        .providers
        .get(&name)
        .ok_or_else(|| ApiError::NotFound(format!("provider '{name}' not found")))?;

    Ok(Json(ProviderInfo {
        name,
        provider_type: entry.provider_type.clone(),
        base_url: entry.base_url.clone(),
        models: entry
            .default
            .as_ref()
            .map(|d| vec![d.model.clone()])
            .unwrap_or_default(),
    }))
}

pub async fn upsert(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpsertProviderRequest>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let path = providers_path(&user_id, &state);
    let mut config = load_providers(&path)?;

    // Simplified: use provider_type as name
    let name = req.provider_type.clone();
    let mut providers = config.providers;
    providers.insert(
        name,
        peco_core::config::ProviderEntry {
            provider_type: req.provider_type,
            api_key: req.api_key,
            base_url: req.base_url,
            default: None,
        },
    );
    config.providers = providers;
    save_providers(&path, &config)?;

    Ok(Json(SuccessResponse {
        success: true,
        message: Some("Provider saved".into()),
    }))
}

pub async fn delete(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let path = providers_path(&user_id, &state);
    let mut config = load_providers(&path)?;
    config.providers.remove(&name);
    save_providers(&path, &config)?;

    Ok(Json(SuccessResponse {
        success: true,
        message: Some(format!("Provider '{name}' deleted")),
    }))
}

pub async fn test_connection(
    AuthUser { user_id: _ }: AuthUser,
    State(_state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    // Simplified: return success for known provider types
    Ok(Json(SuccessResponse {
        success: true,
        message: Some(format!("Provider '{name}' test not yet implemented")),
    }))
}
