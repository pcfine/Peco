// Skill Handler — 用户 workspace 级别 Skill 管理

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

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

fn skills_dir(user_id: &str, state: &AppState) -> std::path::PathBuf {
    state
        .workspace_manager
        .workspace_dir(user_id)
        .join("skills")
}

pub async fn list(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<SkillInfo>>, ApiError> {
    let dir = skills_dir(&user_id, &state);
    let mut skills = Vec::new();

    if dir.exists()
        && let Ok(entries) = std::fs::read_dir(&dir)
    {
        for entry in entries.flatten() {
            let skill_md = entry.path().join("SKILL.md");
            if skill_md.exists()
                && let Ok(content) = std::fs::read_to_string(&skill_md)
            {
                let name = entry.file_name().to_string_lossy().to_string();
                let description = extract_description(&content);
                skills.push(SkillInfo { name, description });
            }
        }
    }

    Ok(Json(skills))
}

pub async fn get(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let skill_md = skills_dir(&user_id, &state).join(&name).join("SKILL.md");
    if !skill_md.exists() {
        return Err(ApiError::NotFound(format!("skill '{name}' not found")));
    }
    let content = std::fs::read_to_string(&skill_md)
        .map_err(|e| ApiError::Internal(format!("failed to read SKILL.md: {e}")))?;
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
    let skill_dir = skills_dir(&user_id, &state).join(&name);
    std::fs::create_dir_all(&skill_dir)
        .map_err(|e| ApiError::Internal(format!("failed to create skill directory: {e}")))?;
    std::fs::write(skill_dir.join("SKILL.md"), &req.content)
        .map_err(|e| ApiError::Internal(format!("failed to write SKILL.md: {e}")))?;

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
    let skill_dir = skills_dir(&user_id, &state).join(&name);
    if skill_dir.exists() {
        std::fs::remove_dir_all(&skill_dir)
            .map_err(|e| ApiError::Internal(format!("failed to delete skill directory: {e}")))?;
    }
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
    let skill_dir = skills_dir(&user_id, &state).join(&name);
    if !skill_dir.exists() {
        return Err(ApiError::NotFound(format!("skill '{name}' not found")));
    }
    // Simple: return SKILL.md content as download
    let content = std::fs::read_to_string(skill_dir.join("SKILL.md"))
        .map_err(|e| ApiError::Internal(format!("failed to read SKILL.md: {e}")))?;
    Ok(content.into_bytes())
}

pub async fn import_skill(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let name = req["name"].as_str().unwrap_or("imported-skill");
    let content = req["content"].as_str().unwrap_or("");
    let skill_dir = skills_dir(&user_id, &state).join(name);
    std::fs::create_dir_all(&skill_dir)
        .map_err(|e| ApiError::Internal(format!("failed to create skill directory: {e}")))?;
    std::fs::write(skill_dir.join("SKILL.md"), content)
        .map_err(|e| ApiError::Internal(format!("failed to write SKILL.md: {e}")))?;

    Ok(Json(SuccessResponse {
        success: true,
        message: Some(format!("Skill '{name}' imported")),
    }))
}

fn extract_description(content: &str) -> String {
    for line in content.lines() {
        if line.starts_with("description:") {
            return line
                .trim_start_matches("description:")
                .trim()
                .trim_matches('"')
                .to_string();
        }
    }
    String::new()
}
