//! 对话产物（artifact）——工具声明的图片 / 视频 / 音频 / 文档。
//!
//! # 数据流
//!
//! ```text
//! skill 脚本 stdout
//!   └─ "@@PECO_ARTIFACT {json}"  ← 声明
//!        └─ peco-core 在工具结果入历史前剥离（模型看不到）
//!             └─ LooperEvent::ToolResult { artifacts }
//!                  └─ 本模块：校验路径 → 落库 → SSE 推送
//! ```
//!
//! 路径校验在本模块：只接受落在 `<data_dir>/work/<user_id>/output/` 之下的文件，
//! canonicalize 后按**路径组件**比较前缀（同时消解 `..` 与符号链接）。
//!
//! 这条边界的目的是**防模型误声明**（写错目录、把临时文件当成果），不是防恶意
//! 本地脚本 —— skill 以同一 OS 用户运行，本来就能读任意路径；注册后把文件换成
//! 符号链接也能绕过取流时的复核。别把这里当沙箱。
//!
//! # 同一路径重复声明
//!
//! 去重键是 `(conversation_id, rel_path)`，重复声明返回 `Ok(None)`，既不更新
//! 元数据也不发事件 —— 侧栏不会出现第二条。这依赖「产物路径天然带日期/唯一
//! 后缀」（media-gen 现状）。若某天出现「覆盖写同一路径」的 skill，用户会看不到
//! 更新，届时需要改成 `ON CONFLICT ... DO UPDATE` 并照常发事件。

pub mod handler;

use std::path::{Path, PathBuf};

use axum::Router;
use axum::extract::State;
use axum::routing::get;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::warn;

use peco_core::artifacts::ArtifactDecl;
use peco_core::workdir::WorkDirs;

use crate::chat::sse::ChatSseEvent;
use crate::db::artifacts as db_artifacts;
use crate::state::AppState;

/// 产物 DTO —— 列表端点与 SSE 事件共用同一形状。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactItem {
    pub id: String,
    pub conversation_id: String,
    /// `image` / `video` / `audio` / `document`
    pub kind: String,
    pub mime: String,
    pub title: String,
    pub size_bytes: i64,
    /// 取流地址（需带 `Authorization: Bearer`）。
    ///
    /// 浏览器给 `<img src>` / `<video src>` **不会**带 Authorization，所以前端
    /// 必须用 `fetch` 取 blob 再挂 object URL（阶段 2 按此实现）。这也是端点
    /// 保留 Range 的意义：换签名直出后即可直接喂给 `<video>`。
    pub url: String,
    /// 下载地址（`Content-Disposition: attachment`）
    pub download_url: String,
    pub created_at: String,
    /// 文件当前是否仍存在；被清理后为 false（UI 显示「已过期」）
    pub available: bool,
}

impl ArtifactItem {
    pub fn from_row(row: &db_artifacts::ArtifactRow, available: bool) -> Self {
        Self {
            id: row.id.clone(),
            conversation_id: row.conversation_id.clone(),
            kind: row.kind.clone(),
            mime: row.mime.clone(),
            title: row.title.clone(),
            size_bytes: row.size_bytes,
            url: format!("/api/artifacts/{}/content", row.id),
            download_url: format!("/api/artifacts/{}/download", row.id),
            created_at: row.created_at.clone(),
            available,
        }
    }
}

/// 受保护的产物路由：内容与下载。
pub fn router() -> Router<std::sync::Arc<AppState>> {
    Router::new()
        .route("/{id}/content", get(handler::content))
        .route("/{id}/download", get(handler::download))
}

/// 该用户的产物根（`<data_dir>/work/<user_id>`）。
///
/// workspace 布局不标准时返回 `None` —— 与 `WorkDirs` 的 fail-safe 一致：
/// 宁可不登记，也不写到猜出来的位置。
pub fn work_dirs(manager: &crate::workspace::WorkspaceManager, user_id: &str) -> Option<WorkDirs> {
    let workspace = manager.workspace_dir(user_id);
    WorkDirs::from_workspace_root(&workspace)
}

/// 同 [`work_dirs`]，额外给出工作层根路径（列表端点判「文件是否还在」用）。
pub fn work_root(
    manager: &crate::workspace::WorkspaceManager,
    user_id: &str,
) -> Option<(PathBuf, WorkDirs)> {
    work_dirs(manager, user_id).map(|w| (w.root().to_path_buf(), w))
}

/// 校验并登记一条声明。
///
/// 返回 `Ok(None)` 表示同一对话内已登记过（重复声明）；`Err` 表示被拒
/// （路径越界 / 文件不存在 / 用户无工作层），调用方记日志即可，不影响工具结果。
pub async fn persist_declaration(
    db: &SqlitePool,
    work: &WorkDirs,
    conversation_id: &str,
    user_id: &str,
    decl: &ArtifactDecl,
) -> Result<Option<ArtifactItem>, String> {
    let path = Path::new(&decl.path);
    if !path.is_absolute() {
        return Err(format!("artifact path must be absolute: {}", decl.path));
    }

    // canonicalize 同时消解 `..` 与符号链接 —— 前缀比较必须发生在它之后。
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("artifact not readable: {} ({e})", decl.path))?;
    let output = work
        .output()
        .canonicalize()
        .map_err(|e| format!("output dir unavailable: {} ({e})", work.output().display()))?;
    if !canonical.starts_with(&output) {
        return Err(format!(
            "artifact outside output dir: {} (only {} is publishable)",
            canonical.display(),
            output.display()
        ));
    }

    let root = work
        .root()
        .canonicalize()
        .map_err(|e| format!("work root unavailable: {e}"))?;
    let rel_path = canonical
        .strip_prefix(&root)
        .map_err(|_| format!("artifact outside work root: {}", canonical.display()))?
        .to_string_lossy()
        .replace('\\', "/");

    let meta =
        std::fs::metadata(&canonical).map_err(|e| format!("artifact metadata failed: {e}"))?;
    if !meta.is_file() {
        return Err(format!("artifact is not a file: {}", canonical.display()));
    }

    let mime = decl
        .mime
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| guess_mime(&canonical).to_string());
    let title = decl
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| {
            canonical
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "artifact".to_string())
        });

    let row = db_artifacts::ArtifactRow {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.to_string(),
        user_id: user_id.to_string(),
        kind: kind_of(&mime).to_string(),
        mime,
        title,
        rel_path,
        size_bytes: meta.len() as i64,
        created_at: String::new(), // 由 DB 默认值填充，回读不使用
    };
    // 去重由唯一索引 `(conversation_id, rel_path)` 保证 —— 并发两条流各自
    // 声明同一文件时，只有一条插得进去。
    let inserted = db_artifacts::insert(db, &row)
        .await
        .map_err(|e| format!("artifact insert failed: {e}"))?;
    if !inserted {
        return Ok(None);
    }

    // 回读以拿到 DB 生成的时间戳（与 list 端点形状一致）。
    let stored = db_artifacts::find_by_id_and_user(db, &row.id, user_id)
        .await
        .map_err(|e| format!("artifact readback failed: {e}"))?
        .ok_or_else(|| "artifact vanished after insert".to_string())?;
    Ok(Some(ArtifactItem::from_row(&stored, true)))
}

/// 由 MIME 推导展示大类。
pub fn kind_of(mime: &str) -> &'static str {
    let m = mime.to_ascii_lowercase();
    if m.starts_with("image/") {
        "image"
    } else if m.starts_with("video/") {
        "video"
    } else if m.starts_with("audio/") {
        "audio"
    } else {
        "document"
    }
}

/// 由扩展名推断 MIME（声明未给 `mime` 时使用）。
pub fn guess_mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("m4a") => "audio/mp4",
        Some("ogg") => "audio/ogg",
        Some("pdf") => "application/pdf",
        Some("md") => "text/markdown",
        Some("txt") => "text/plain",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("html") => "text/html",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

/// 落库产物并构造 `ToolResult` 事件。
///
/// **顺序契约**：先落库、后发事件 —— 事件里的 artifact id 是前端取流的唯一
/// 凭据，反过来会给出一个不存在的 id。抽成独立函数是为了让这条契约可测：
/// SSE 主流程要真实 LLM 才能跑，但这条路径本身不需要。
///
/// 登记失败只降级（[`persist_declarations`] 内部 warn），不影响事件发出 ——
/// 产物登记是旁路副作用，不该否决主对话。
// 参数与 `ChatSseEvent::ToolResult` 字段一一对应；拆成入参结构体只会多一层
// 且仍需逐字段搬运，保持扁平更贴近调用点。
#[allow(clippy::too_many_arguments)]
pub async fn build_tool_result_event(
    db: &SqlitePool,
    work: Option<&WorkDirs>,
    conversation_id: &str,
    user_id: &str,
    id: String,
    name: String,
    result: String,
    images: Vec<String>,
    artifacts: &[ArtifactDecl],
) -> ChatSseEvent {
    let items = persist_declarations(db, work, conversation_id, user_id, artifacts).await;
    ChatSseEvent::ToolResult {
        id,
        name,
        result,
        images,
        artifacts: items,
        conversation_id: conversation_id.to_string(),
    }
}

/// 对一批声明逐条登记；失败只告警，不影响工具结果。
pub async fn persist_declarations(
    db: &SqlitePool,
    work: Option<&WorkDirs>,
    conversation_id: &str,
    user_id: &str,
    decls: &[ArtifactDecl],
) -> Vec<ArtifactItem> {
    let Some(work) = work else {
        warn!(
            user_id,
            count = decls.len(),
            "Artifact declarations dropped: workspace layout not recognized"
        );
        return Vec::new();
    };

    let mut items = Vec::new();
    for decl in decls {
        match persist_declaration(db, work, conversation_id, user_id, decl).await {
            Ok(Some(item)) => items.push(item),
            Ok(None) => {}
            Err(reason) => warn!(
                user_id,
                conversation_id,
                path = %decl.path,
                reason,
                "Artifact declaration rejected"
            ),
        }
    }
    items
}

/// 取某对话的产物行并映射为 DTO（含 `available` 的文件存在性判定）。
///
/// 归属校验与分页参数的 clamp 由调用方负责；分页差异下沉到 db 层，本函数只负责
/// 「取行 + stat + 映射」。「文件是否算存在」只有这一处实现 —— 复制一份就会漂移成
/// 面板说「还能下载」、点开却 404。
///
/// `page: None` 是全量正序（chat 列表沿用：与消息流方向一致）；
/// `Some((limit, offset))` 是倒序分页（产物面板要最近优先）。
pub(crate) async fn items_for_conversation(
    state: &AppState,
    user_id: &str,
    conversation_id: &str,
    page: Option<(i64, i64)>,
) -> Result<Vec<ArtifactItem>, crate::error::ApiError> {
    let rows = match page {
        Some((limit, offset)) => {
            db_artifacts::list_by_conversation_desc(&state.db, conversation_id, limit, offset)
                .await?
        }
        None => db_artifacts::list_by_conversation(&state.db, conversation_id).await?,
    };
    let work = work_root(&state.workspace_manager, user_id);
    Ok(rows
        .into_iter()
        .map(|row| {
            let available = work
                .as_ref()
                .map(|(root, _)| root.join(&row.rel_path).is_file())
                .unwrap_or(false);
            ArtifactItem::from_row(&row, available)
        })
        .collect())
}

/// 列出某对话的产物（文件是否仍在按需 stat）。
pub async fn list(
    state: State<std::sync::Arc<AppState>>,
    user: crate::auth::AuthUser,
    axum::extract::Path((_agent_id, id)): axum::extract::Path<(String, String)>,
) -> Result<axum::Json<Vec<ArtifactItem>>, crate::error::ApiError> {
    use crate::db::conversations;

    conversations::find_by_id_and_user(&state.db, &id, &user.user_id)
        .await?
        .ok_or_else(|| {
            crate::error::ApiError::NotFound(format!("conversation '{id}' not found"))
        })?;

    let items = items_for_conversation(&state, &user.user_id, &id, None).await?;
    Ok(axum::Json(items))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_is_derived_from_mime() {
        assert_eq!(kind_of("image/jpeg"), "image");
        assert_eq!(kind_of("video/mp4"), "video");
        assert_eq!(kind_of("audio/mpeg"), "audio");
        assert_eq!(kind_of("application/pdf"), "document");
        assert_eq!(kind_of("text/markdown"), "document");
    }

    #[test]
    fn mime_guessed_from_extension() {
        assert_eq!(guess_mime(Path::new("/a/b.JPG")), "image/jpeg");
        assert_eq!(guess_mime(Path::new("/a/b.mp4")), "video/mp4");
        assert_eq!(guess_mime(Path::new("/a/b.md")), "text/markdown");
        // 未知扩展名给保守值，绝不冒充可执行类型
        assert_eq!(
            guess_mime(Path::new("/a/b.bin")),
            "application/octet-stream"
        );
        assert_eq!(
            guess_mime(Path::new("/a/noext")),
            "application/octet-stream"
        );
    }
}
