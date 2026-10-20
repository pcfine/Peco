// Skill Handler — 用户 workspace 级别 Skill 管理

use std::io::Read;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;
use peco_core::skills::{SkillError, SkillResourceFile, validate_name};
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
    check_skill_name(&name)?;
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
    check_skill_name(&name)?;
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
    check_skill_name(&name)?;
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

/// 校验 `{name}` 路径参数。
///
/// axum 会把 `%2F` 解码进 `{name}`，于是 `..%2F..%2Fetc` 变成 `../../etc`；
/// 未校验时 `skills_dir().join(name)` 会逃出 skills 根目录。读写两类端点
/// 都必须先过这道闸。
fn check_skill_name(name: &str) -> Result<(), ApiError> {
    validate_name(name).map_err(ApiError::BadRequest)
}

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

// ── 目录文件（只读） ────────────────────────────────────────────────────────

/// 预览上限：超出部分不返回，响应体标记 `truncated`。
const PREVIEW_LIMIT: u64 = 256 * 1024;
const MAX_LISTED_FILES: usize = 2000;
const MAX_WALK_DEPTH: usize = 8;

const BINARY_EXTS: &[&str] = &[
    "pyc", "pyo", "so", "dylib", "dll", "exe", "bin", "o", "a", "class", "wasm", "zip", "gz",
    "tar", "pdf", "png", "jpg", "jpeg", "gif", "webp", "ico", "bmp", "woff", "woff2", "ttf", "otf",
    "mp3", "mp4", "mov", "sqlite", "db",
];
const CODE_EXTS: &[&str] = &[
    "py", "sh", "bash", "zsh", "js", "mjs", "cjs", "ts", "tsx", "jsx", "css", "html", "htm", "xml",
    "rs", "go", "java", "c", "h", "cpp", "hpp", "rb", "php", "lua", "sql",
];
const DATA_EXTS: &[&str] = &["json", "jsonl", "csv", "toml", "yaml", "yml", "ini", "conf"];

#[derive(Debug, Serialize)]
pub struct SkillFileInfo {
    /// 相对 skill 根目录的路径，`/` 分隔。
    pub path: String,
    pub size: u64,
    /// RFC3339（UTC，秒精度）。
    pub mtime: String,
    /// markdown / code / data / env / text / binary
    pub kind: &'static str,
    /// 能否通过现有 API 保存（scripts/ references/ assets/ 且非 __pycache__）。
    pub writable: bool,
}

#[derive(Debug, Serialize)]
pub struct SkillFileContent {
    pub path: String,
    pub size: u64,
    pub mtime: String,
    pub kind: &'static str,
    pub content: String,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum SkillFilesResponse {
    List { files: Vec<SkillFileInfo> },
    Content(SkillFileContent),
}

#[derive(Debug, Deserialize)]
pub struct FileQuery {
    pub path: Option<String>,
}

/// `GET /api/skills/{name}/files` — 目录清单；带 `?path=` 时返回该文件内容。
pub async fn files(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<FileQuery>,
) -> Result<Json<SkillFilesResponse>, ApiError> {
    check_skill_name(&name)?;
    let ws = state
        .workspace_manager
        .get_synced(&user_id, &state.db)
        .await?;
    let skill_dir = ws.skills_dir().join(&name);
    if !skill_dir.join("SKILL.md").is_file() {
        return Err(ApiError::NotFound(format!("skill '{name}' not found")));
    }

    match query.path.as_deref() {
        None => {
            let mut files = Vec::new();
            walk_dir(&skill_dir, "", 0, &mut files);
            files.sort_by(|a, b| a.path.cmp(&b.path));
            info!(user_id = %user_id, name = %name, count = files.len(), "Skill files listed");
            Ok(Json(SkillFilesResponse::List { files }))
        }
        Some(rel) => {
            let path = resolve_within(&skill_dir, rel)?;
            let meta = std::fs::metadata(&path)
                .map_err(|_| ApiError::NotFound(format!("file '{rel}' not found")))?;
            if !meta.is_file() {
                return Err(ApiError::BadRequest(format!("'{rel}' is not a file")));
            }
            let size = meta.len();
            let kind = classify(&path);
            if kind == "binary" {
                return Err(ApiError::UnsupportedMedia(format!(
                    "'{rel}' is a binary file"
                )));
            }

            let mut handle = std::fs::File::open(&path)
                .map_err(|e| ApiError::Internal(format!("failed to open '{rel}': {e}")))?;
            let mut buf = Vec::new();
            handle
                .by_ref()
                .take(PREVIEW_LIMIT)
                .read_to_end(&mut buf)
                .map_err(|e| ApiError::Internal(format!("failed to read '{rel}': {e}")))?;
            let truncated = size > PREVIEW_LIMIT;
            if !truncated && buf.contains(&0) {
                return Err(ApiError::UnsupportedMedia(format!(
                    "'{rel}' is a binary file"
                )));
            }
            // 截断可能切断多字节字符，此时用有损转换；未截断则严格校验。
            let content = if truncated {
                String::from_utf8_lossy(&buf).into_owned()
            } else {
                String::from_utf8(buf)
                    .map_err(|_| ApiError::UnsupportedMedia(format!("'{rel}' is not UTF-8")))?
            };

            info!(user_id = %user_id, name = %name, path = %rel, "Skill file read");
            Ok(Json(SkillFilesResponse::Content(SkillFileContent {
                path: rel.to_string(),
                size,
                mtime: mtime_rfc3339(&meta),
                kind,
                content,
                truncated,
            })))
        }
    }
}

/// 递归收集文件（不跟随符号链接；根级 SKILL.md 由前端固定行承担，不重复列出）。
fn walk_dir(root: &std::path::Path, prefix: &str, depth: usize, out: &mut Vec<SkillFileInfo>) {
    if depth > MAX_WALK_DEPTH || out.len() >= MAX_LISTED_FILES {
        return;
    }
    let dir = if prefix.is_empty() {
        root.to_path_buf()
    } else {
        root.join(prefix)
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        if out.len() >= MAX_LISTED_FILES {
            return;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if file_type.is_dir() {
            walk_dir(root, &rel, depth + 1, out);
            continue;
        }
        if !file_type.is_file() || rel == "SKILL.md" {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let path = entry.path();
        out.push(SkillFileInfo {
            kind: classify(&path),
            writable: is_writable(&rel),
            size: meta.len(),
            mtime: mtime_rfc3339(&meta),
            path: rel,
        });
    }
}

/// 现有写入 API 的白名单：三个 Tier-3 前缀，且不是字节码缓存。
fn is_writable(rel: &str) -> bool {
    let Some((top, _)) = rel.split_once('/') else {
        return false;
    };
    matches!(top, "scripts" | "references" | "assets") && !rel.contains("__pycache__/")
}

fn classify(path: &std::path::Path) -> &'static str {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name == ".env" || name.starts_with(".env.") {
        return if name.ends_with(".example") {
            "text"
        } else {
            "env"
        };
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("md" | "markdown" | "mdx") => "markdown",
        Some(e) if BINARY_EXTS.contains(&e) => "binary",
        Some(e) if CODE_EXTS.contains(&e) => "code",
        Some(e) if DATA_EXTS.contains(&e) => "data",
        _ => {
            if sniff_binary(path) {
                "binary"
            } else {
                "text"
            }
        }
    }
}

/// 读前 4KB 判断：含 NUL 或非 UTF-8 即视为二进制。
fn sniff_binary(path: &std::path::Path) -> bool {
    use std::io::Read as _;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 4096];
    let Ok(n) = f.read(&mut buf) else {
        return false;
    };
    let head = &buf[..n];
    head.contains(&0) || std::str::from_utf8(head).is_err()
}

/// 解析相对路径并确保落在 skill 目录内（阻断 `..`、绝对路径与符号链接逃逸）。
fn resolve_within(skill_dir: &std::path::Path, rel: &str) -> Result<std::path::PathBuf, ApiError> {
    let trimmed = rel.trim();
    if trimmed.is_empty()
        || trimmed.contains('\0')
        || trimmed.contains('\\')
        || trimmed.starts_with('/')
    {
        return Err(ApiError::BadRequest("invalid file path".into()));
    }
    let mut joined = std::path::PathBuf::new();
    for part in trimmed.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(ApiError::BadRequest("invalid file path".into()));
        }
        joined.push(part);
    }

    let root = skill_dir
        .canonicalize()
        .map_err(|e| ApiError::Internal(format!("failed to resolve skill dir: {e}")))?;
    let target = root
        .join(&joined)
        .canonicalize()
        .map_err(|_| ApiError::NotFound(format!("file '{rel}' not found")))?;
    if !target.starts_with(&root) {
        return Err(ApiError::BadRequest("invalid file path".into()));
    }
    Ok(target)
}

fn mtime_rfc3339(meta: &std::fs::Metadata) -> String {
    meta.modified()
        .ok()
        .map(chrono::DateTime::<chrono::Utc>::from)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}
