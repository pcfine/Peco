// Skill Handler — 用户 workspace 级别 Skill 管理

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;
use peco_core::skills::{SkillError, SkillResourceFile};
use tracing::info;

#[derive(Debug, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct UpsertSkillRequest {
    pub content: String, // SKILL.md content
    /// Optional Tier-3 resource files (scripts / references / assets).
    #[serde(default)]
    pub files: Vec<SkillResourceFile>,
}

#[derive(Debug, Deserialize)]
pub struct ImportSkillRequest {
    pub name: String,
    pub content: String,
    /// Optional Tier-3 resource files (scripts / references / assets).
    #[serde(default)]
    pub files: Vec<SkillResourceFile>,
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
    // 统一走 SkillRegister：名称/内容校验 + 原子写入 + 缓存刷新，
    // 与 Agent 工具路径（save_skill）共用同一套规则。
    ws.skill_registry()
        .save_skill_bundle(&name, &req.content, &req.files)
        .map_err(skill_error_to_api)?;

    refresh_skills_hash(&state, &user_id, &ws.skills_dir()).await;

    info!(user_id = %user_id, name = %name, files = req.files.len(), "Skill created/updated");
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

    refresh_skills_hash(&state, &user_id, &ws.skills_dir()).await;

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
    Json(req): Json<ImportSkillRequest>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("skill name is required".into()));
    }
    if req.content.trim().is_empty() {
        return Err(ApiError::BadRequest("skill content is required".into()));
    }

    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;

    // 与 PUT /skills/{name} 同一路径：校验 → 原子写 → 刷缓存。
    ws.skill_registry()
        .save_skill_bundle(name, &req.content, &req.files)
        .map_err(skill_error_to_api)?;

    refresh_skills_hash(&state, &user_id, &ws.skills_dir()).await;

    info!(user_id = %user_id, name = %name, files = req.files.len(), "Skill imported");
    Ok(Json(SuccessResponse {
        success: true,
        message: Some(format!("Skill '{name}' imported")),
    }))
}

// ── 内部辅助 ────────────────────────────────────────────────────────────────

/// 重新计算 skills 模块哈希并写入 DB（模块文件变更后调用）。
async fn refresh_skills_hash(state: &AppState, user_id: &str, skills_dir: &std::path::Path) {
    let hash = peco_core::workspace::hash::compute_skills_hash(skills_dir);
    let _ = crate::db::workspace_hashes::upsert_hash(&state.db, user_id, "skills", &hash).await;
}

/// 将 [`SkillError`] 映射为合适的 HTTP 状态码。
///
/// 校验类错误（名称 / frontmatter / 资源路径）是调用方的问题 → 400；
/// 其余（I/O 等）归为服务器内部错误 → 500。
fn skill_error_to_api(e: SkillError) -> ApiError {
    let msg = e.to_string();
    match e {
        SkillError::InvalidName { .. }
        | SkillError::InvalidFrontmatter { .. }
        | SkillError::NameMismatch { .. }
        | SkillError::InvalidResourcePath { .. } => ApiError::BadRequest(msg),
        SkillError::SkillMdNotFound(_) => ApiError::NotFound(msg),
        _ => ApiError::Internal(msg),
    }
}
