// Skill Handler — 用户 workspace 级别 Skill 管理

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;
use tracing::info;

#[derive(Debug, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct UpsertSkillRequest {
    pub content: String, // SKILL.md content
}

#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub async fn list(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<SkillInfo>>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let metas = ws.skill_registry().all_meta();
    let skills: Vec<SkillInfo> = metas
        .into_iter()
        .map(|m| SkillInfo {
            name: m.name,
            description: m.description,
        })
        .collect();
    info!(user_id = %user_id, count = skills.len(), "Skills listed");
    Ok(Json(skills))
}

pub async fn get(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let skill_md = ws.skills_dir().join(&name).join("SKILL.md");
    if !skill_md.exists() {
        return Err(ApiError::NotFound(format!("skill '{name}' not found")));
    }
    let content = std::fs::read_to_string(&skill_md)
        .map_err(|e| ApiError::Internal(format!("failed to read SKILL.md: {e}")))?;
    info!(user_id = %user_id, name = %name, "Skill fetched");
    Ok(Json(
        serde_json::json!({ "name": name, "content": content }),
    ))
}

pub async fn upsert(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(req): Json<UpsertSkillRequest>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let skill_dir = ws.skills_dir().join(&name);
    std::fs::create_dir_all(&skill_dir)
        .map_err(|e| ApiError::Internal(format!("failed to create skill directory: {e}")))?;
    std::fs::write(skill_dir.join("SKILL.md"), &req.content)
        .map_err(|e| ApiError::Internal(format!("failed to write SKILL.md: {e}")))?;

    // 通知 SkillRegister 刷新该 Skill 的缓存
    ws.reload_skill(&name);

    // 更新 skills 模块哈希
    let skills_hash = peco_core::workspace::hash::compute_skills_hash(&ws.skills_dir());
    let _ =
        crate::db::workspace_hashes::upsert_hash(&state.db, &user_id, "skills", &skills_hash).await;

    info!(user_id = %user_id, name = %name, "Skill created/updated");
    Ok(Json(SuccessResponse {
        success: true,
        message: Some(format!("Skill '{name}' saved")),
    }))
}

pub async fn delete_skill(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let skill_dir = ws.skills_dir().join(&name);
    if skill_dir.exists() {
        std::fs::remove_dir_all(&skill_dir)
            .map_err(|e| ApiError::Internal(format!("failed to delete skill directory: {e}")))?;
    }

    // 从 SkillRegister 缓存中移除
    ws.remove_skill(&name);

    // 更新 skills 模块哈希
    let skills_hash = peco_core::workspace::hash::compute_skills_hash(&ws.skills_dir());
    let _ =
        crate::db::workspace_hashes::upsert_hash(&state.db, &user_id, "skills", &skills_hash).await;

    info!(user_id = %user_id, name = %name, "Skill deleted");
    Ok(Json(SuccessResponse {
        success: true,
        message: Some(format!("Skill '{name}' deleted")),
    }))
}

pub async fn export_skill(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Vec<u8>, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let skill_dir = ws.skills_dir().join(&name);
    if !skill_dir.exists() {
        return Err(ApiError::NotFound(format!("skill '{name}' not found")));
    }
    // Simple: return SKILL.md content as download
    let content = std::fs::read_to_string(skill_dir.join("SKILL.md"))
        .map_err(|e| ApiError::Internal(format!("failed to read SKILL.md: {e}")))?;
    info!(user_id = %user_id, name = %name, "Skill exported");
    Ok(content.into_bytes())
}

pub async fn import_skill(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let name = req["name"].as_str().unwrap_or("imported-skill");
    let content = req["content"].as_str().unwrap_or("");
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let skill_dir = ws.skills_dir().join(name);
    std::fs::create_dir_all(&skill_dir)
        .map_err(|e| ApiError::Internal(format!("failed to create skill directory: {e}")))?;
    std::fs::write(skill_dir.join("SKILL.md"), content)
        .map_err(|e| ApiError::Internal(format!("failed to write SKILL.md: {e}")))?;

    // 通知 SkillRegister 刷新该 Skill 的缓存
    ws.reload_skill(name);

    // 更新 skills 模块哈希
    let skills_hash = peco_core::workspace::hash::compute_skills_hash(&ws.skills_dir());
    let _ =
        crate::db::workspace_hashes::upsert_hash(&state.db, &user_id, "skills", &skills_hash).await;

    info!(user_id = %user_id, name = %name, "Skill imported");
    Ok(Json(SuccessResponse {
        success: true,
        message: Some(format!("Skill '{name}' imported")),
    }))
}
