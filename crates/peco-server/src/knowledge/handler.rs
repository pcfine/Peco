// ============================================================================
// Knowledge Handlers — 知识库 CRUD + 文件上传 + 文档管理
// ============================================================================

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum_extra::extract::Multipart;
use knowledge_base::manager::config::{
    BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig,
};
use peco_core::knowledge::hash_manifest::is_supported_extension_with;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::reconcile;
use crate::auth::AuthUser;
use crate::db::{documents, knowledge_bases};
use crate::error::ApiError;
use crate::state::AppState;
use tracing::{error, info, warn};

// ── Request / Response 类型 ─────────────────────────────────────────────────

/// 创建知识库请求。
#[derive(Debug, Deserialize)]
pub struct CreateKbRequest {
    /// 知识库名称（同一用户下唯一）。
    pub name: String,
    /// 描述信息。
    #[serde(default)]
    pub description: String,
    /// 嵌入模型，默认 BGE-small-zh。
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// 分块策略，默认重叠窗口。
    #[serde(default)]
    pub chunk_strategy: Option<ChunkStrategyRequest>,
}

/// 分块策略请求体。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ChunkStrategyRequest {
    #[serde(rename = "overlapping-window")]
    OverlappingWindow { size: usize, overlap: usize },
    #[serde(rename = "fixed-size")]
    FixedSize { size: usize },
    #[serde(rename = "sentence-based")]
    SentenceBased { max_chars: usize },
}

impl Default for ChunkStrategyRequest {
    fn default() -> Self {
        ChunkStrategyRequest::OverlappingWindow {
            size: 800,
            overlap: 200,
        }
    }
}

/// 知识库列表项响应。
#[derive(Debug, Serialize)]
pub struct KnowledgeBaseResponse {
    pub id: String,
    pub name: String,
    pub description: String,
    pub backend: String,
    pub embedding_model: String,
    pub document_count: usize,
    pub chunk_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

/// 文档列表项响应。
#[derive(Debug, Serialize)]
pub struct DocumentResponse {
    pub id: String,
    pub kb_id: String,
    pub filename: String,
    pub file_size: i64,
    pub mime_type: String,
    pub status: String,
    pub error_msg: Option<String>,
    pub created_at: String,
}

/// 同步结果响应。
#[derive(Debug, Serialize)]
pub struct SyncResponse {
    pub kb_name: String,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub skipped: usize,
    pub errors: Vec<(String, String)>,
    pub duration_ms: u64,
}

/// 简单成功响应。
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
}

/// 文档列表查询参数。
#[derive(Debug, Deserialize)]
pub struct DocumentListQuery {
    #[serde(default = "default_offset")]
    pub offset: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub status: Option<String>,
}

fn default_offset() -> i64 {
    0
}
fn default_limit() -> i64 {
    50
}

// ── 辅助函数 ─────────────────────────────────────────────────────────────────

/// `KbInfo` 不可得时回退的展示标签，取值与 REST 建库默认后端/模型一致。
const DEFAULT_BACKEND_LABEL: &str = "HelixDB";
const DEFAULT_EMBEDDING_LABEL: &str = "BGEBaseZHV15";

/// 从 `KbInfo` 取展示用 `(backend, embedding_model)`；不可得时用回退常量。
fn kb_labels(info: Option<&knowledge_base::KbInfo>) -> (String, String) {
    match info {
        Some(info) => (info.backend.clone(), info.embedding_model.clone()),
        None => (
            DEFAULT_BACKEND_LABEL.to_string(),
            DEFAULT_EMBEDDING_LABEL.to_string(),
        ),
    }
}

/// 解析嵌入模型字符串为 FastembedModelTypeSerde。
fn parse_embedding_model(s: Option<&str>) -> FastembedModelTypeSerde {
    match s {
        Some("bge-small-zh-v15") | Some("bge-small-zh-v1.5") => {
            FastembedModelTypeSerde::BGESmallZHV15
        }
        Some("bge-base-zh-v15") => FastembedModelTypeSerde::BGEBaseZHV15,
        Some("bge-large-zh-v15") => FastembedModelTypeSerde::BGELargeZHV15,
        Some("all-minilm-l6-v2q") | Some("all-MiniLM-L6-v2") => {
            FastembedModelTypeSerde::AllMiniLML6V2Q
        }
        Some("multilingual-e5-small") => FastembedModelTypeSerde::MultilingualE5Small,
        _ => FastembedModelTypeSerde::BGEBaseZHV15, // 默认中文模型
    }
}

/// 解析分块策略请求体为 ChunkingStrategySerde。
fn parse_chunk_strategy(s: Option<ChunkStrategyRequest>) -> ChunkingStrategySerde {
    match s.unwrap_or_default() {
        ChunkStrategyRequest::OverlappingWindow { size, overlap } => {
            ChunkingStrategySerde::OverlappingWindow { size, overlap }
        }
        ChunkStrategyRequest::FixedSize { size } => ChunkingStrategySerde::FixedSize { size },
        ChunkStrategyRequest::SentenceBased { max_chars } => {
            ChunkingStrategySerde::SentenceBased { max_chars }
        }
    }
}

/// 组装 REST 建库路径的 `KbConfig`。
///
/// 后端固定为 HelixDB —— REST 建库默认走 HelixDB 图存储后端（与模板
/// `kb_config.json` 声明的后端一致）。端点不下发（`helix_url: None`），
/// 由 KB 连接时按 `PECO_KB_HELIX_URL` → `http://localhost:6970` 回退 ——
/// 端点因此是**部署级**配置，而非 per-KB 的 REST 入参。`connect()` 惰性
/// 建 reqwest client，只有 `init_schema()` 才触网。
fn build_kb_config(
    name: String,
    description: String,
    embedding_model: FastembedModelTypeSerde,
    chunking: ChunkingStrategySerde,
) -> KbConfig {
    KbConfig {
        name,
        description,
        embedding_model,
        chunking,
        backend: BackendType::HelixDb,
        storage_path: None,
        default_storage_mode: Default::default(),
        helix_url: None,
    }
}

/// 上传被接受 ⟺ 会被摄入：文件名合法，且 MIME 与扩展名都命中摄入集合。
///
/// 隐藏文件与含路径分隔符的名字既不被扫描收录，还可能写到 `docs/` 之外。
fn is_supported_upload(mime: &str, filename: &str) -> bool {
    is_supported_upload_with(mime, filename, knowledge_base::parsers::docling_enabled())
}

/// 同 [`is_supported_upload`]，但显式指定 Docling 开关以便脱离环境单测。
fn is_supported_upload_with(mime: &str, filename: &str, docling: bool) -> bool {
    // 此处 `Path` 是 axum 的提取器，故用全限定名指 std 的路径类型
    is_allowed_mime(mime)
        && is_supported_extension_with(std::path::Path::new(filename), docling)
        && is_acceptable_upload_name(filename)
}

/// 文件名是否可用作 `relative_path`：非空、非隐藏、不含路径分隔符。
fn is_acceptable_upload_name(filename: &str) -> bool {
    let name = filename.trim();
    !name.is_empty() && !name.starts_with('.') && !name.contains('/') && !name.contains('\\')
}

/// 允许上传的 MIME 类型。是否可摄入另由扩展名（含 Docling 开关）共同判定。
fn is_allowed_mime(mime: &str) -> bool {
    matches!(
        mime,
        "application/pdf"
            | "text/html"
            | "text/markdown"
            | "text/plain"
            | "text/x-python"
            | "text/x-rust"
            | "text/x-go"
            | "application/javascript"
            | "text/typescript"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    ) || mime.starts_with("text/")
}

/// 获取用户 KnowledgeManager (via WorkSpace).
fn get_user_km(
    state: &AppState,
    user_id: &str,
) -> Result<std::sync::Arc<peco_core::knowledge::KnowledgeManager>, ApiError> {
    let ws = state.workspace_manager.get(user_id)?;
    Ok(ws.knowledge_manager().clone())
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `GET /api/knowledge`
///
/// 列出当前用户的所有知识库。
pub async fn list_knowledge_bases(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<KnowledgeBaseResponse>>, ApiError> {
    let rows = knowledge_bases::list_by_user(&state.db, &user_id).await?;

    let infos = match get_user_km(&state, &user_id) {
        Ok(km) => km.list_kbs().await.unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    let mut responses = Vec::with_capacity(rows.len());
    for row in &rows {
        // 真实后端 / 嵌入模型来自 KbInfo（kb_config.json），不再硬编码
        let info = infos.iter().find(|i| i.name == row.name);
        let (backend, embedding_model) = kb_labels(info);
        let (doc_count, chunk_count) = info
            .map(|i| (i.document_count, i.chunk_count))
            .unwrap_or((0, 0));

        responses.push(KnowledgeBaseResponse {
            id: row.id.clone(),
            name: row.name.clone(),
            description: row.description.clone(),
            backend,
            embedding_model,
            document_count: doc_count,
            chunk_count,
            created_at: row.created_at.clone(),
            updated_at: row.updated_at.clone(),
        });
    }

    info!(user_id = %user_id, count = responses.len(), "Knowledge bases listed");
    Ok(Json(responses))
}

/// `POST /api/knowledge`
///
/// 创建新知识库。
pub async fn create_knowledge_base(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateKbRequest>,
) -> Result<(StatusCode, Json<KnowledgeBaseResponse>), ApiError> {
    let name = req.name.trim().to_string();
    if let Err(msg) = knowledge_base::validate_kb_name(&name) {
        return Err(ApiError::BadRequest(msg));
    }

    // 检查同名知识库是否已存在
    if let Some(existing) =
        knowledge_bases::find_by_name_and_user(&state.db, &name, &user_id).await?
    {
        return Err(ApiError::Conflict(format!(
            "knowledge base '{}' already exists (id: {})",
            name, existing.id
        )));
    }

    // 生成 UUID
    let kb_id = Uuid::new_v4().to_string();

    // 解析配置
    let embedding_model = parse_embedding_model(req.embedding_model.as_deref());
    let chunking = parse_chunk_strategy(req.chunk_strategy);

    // 写入 SQLite
    knowledge_bases::insert(
        &state.db,
        &knowledge_bases::CreateKbParams {
            id: kb_id.clone(),
            user_id: user_id.clone(),
            name: name.clone(),
            description: req.description.clone(),
        },
    )
    .await?;

    // 在 KnowledgeBaseManager 中创建知识库
    let km = get_user_km(&state, &user_id)?;
    let kb_config = build_kb_config(
        name.clone(),
        req.description.clone(),
        embedding_model,
        chunking,
    );

    let info = km
        .create_kb(kb_config)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create knowledge base: {e}")))?;

    let response = KnowledgeBaseResponse {
        id: kb_id,
        name,
        description: req.description,
        backend: info.backend,
        embedding_model: info.embedding_model,
        document_count: 0,
        chunk_count: 0,
        created_at: String::new(), // 由 DB 填充
        updated_at: String::new(),
    };

    info!(
        user_id = %user_id,
        kb_id = %response.id,
        name = %response.name,
        "Knowledge base created"
    );

    Ok((StatusCode::CREATED, Json(response)))
}

/// `GET /api/knowledge/:id`
///
/// 获取单个知识库详情。
pub async fn get_knowledge_base(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(kb_id): Path<String>,
) -> Result<Json<KnowledgeBaseResponse>, ApiError> {
    let row = knowledge_bases::find_by_id_and_user(&state.db, &kb_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("knowledge base '{kb_id}' not found")))?;

    let infos = match get_user_km(&state, &user_id) {
        Ok(km) => km.list_kbs().await.unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let info = infos.iter().find(|i| i.name == row.name);
    let (backend, embedding_model) = kb_labels(info);
    let (doc_count, chunk_count) = info
        .map(|i| (i.document_count, i.chunk_count))
        .unwrap_or((0, 0));

    info!(user_id = %user_id, kb_id = %kb_id, name = %row.name, "Knowledge base fetched");
    Ok(Json(KnowledgeBaseResponse {
        id: row.id,
        name: row.name,
        description: row.description,
        backend,
        embedding_model,
        document_count: doc_count,
        chunk_count,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

/// `DELETE /api/knowledge/:id`
///
/// 删除知识库（SQLite 记录 + 磁盘上的 KB 目录）。
///
/// KB 后端的向量/图谱数据不在此清理。
pub async fn delete_knowledge_base(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(kb_id): Path<String>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let row = knowledge_bases::find_by_id_and_user(&state.db, &kb_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("knowledge base '{kb_id}' not found")))?;

    let kb_name = row.name.clone();

    // 1. 删除关联文档的 SQLite 记录
    // (使用一条 raw SQL 删除所有关联文档)
    let _ = sqlx::query("DELETE FROM documents WHERE kb_id = ?")
        .bind(&kb_id)
        .execute(&state.db)
        .await;

    // 2. 删除知识库的 SQLite 记录
    knowledge_bases::delete(&state.db, &kb_id).await?;

    // 3. 通过 KnowledgeBaseManager 删除知识库目录（含 kb_config.json 与其下数据文件）
    if let Ok(km) = get_user_km(&state, &user_id)
        && let Err(e) = km.delete_kb(&kb_name).await
    {
        warn!(
            kb_name = %kb_name,
            error = %e,
            "KnowledgeBaseManager failed to delete KB data (may already be removed)"
        );
    }

    info!(
        user_id = %user_id,
        kb_id = %kb_id,
        kb_name = %kb_name,
        "Knowledge base deleted"
    );

    Ok(Json(SuccessResponse { success: true }))
}

/// `GET /api/knowledge/:id/documents`
///
/// 列出知识库中的文档，支持分页和按状态过滤。
pub async fn list_documents(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(kb_id): Path<String>,
    Query(params): Query<DocumentListQuery>,
) -> Result<Json<Vec<DocumentResponse>>, ApiError> {
    // 验证 KB 归属
    knowledge_bases::find_by_id_and_user(&state.db, &kb_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("knowledge base '{kb_id}' not found")))?;

    let rows = documents::list_by_kb(
        &state.db,
        &kb_id,
        params.offset,
        params.limit,
        params.status.as_deref(),
    )
    .await?;

    let responses: Vec<DocumentResponse> = rows
        .iter()
        .map(|r| DocumentResponse {
            id: r.id.clone(),
            kb_id: r.kb_id.clone(),
            filename: r.filename.clone(),
            file_size: r.file_size,
            mime_type: r.mime_type.clone(),
            status: r.status.clone(),
            error_msg: r.error_msg.clone(),
            created_at: r.created_at.clone(),
        })
        .collect();

    info!(user_id = %user_id, kb_id = %kb_id, count = responses.len(), "Documents listed");
    Ok(Json(responses))
}

/// `POST /api/knowledge/:id/upload`
///
/// 上传文件到知识库。文件保存后异步触发解析管道。
pub async fn upload_document(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(kb_id): Path<String>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<DocumentResponse>), ApiError> {
    // 验证 KB 归属
    let kb_row = knowledge_bases::find_by_id_and_user(&state.db, &kb_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("knowledge base '{kb_id}' not found")))?;

    let kb_name = kb_row.name.clone();
    let kb_name_for_bg = kb_name.clone();
    let kb_id_for_bg = kb_id.clone();
    let user_id_for_bg = user_id.clone();
    let db_for_bg = state.db.clone();
    let wsm_for_bg = state.workspace_manager.clone();

    // 解析 multipart 中的文件字段
    let mut filename: Option<String> = None;
    let mut mime_type: Option<String> = None;
    let mut data: Vec<u8> = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().map(|s| s.to_string());
        let file_name = field.file_name().map(|s| s.to_string());
        let content_type = field.content_type().map(|s| s.to_string());

        if name.as_deref() == Some("file") {
            filename = file_name;
            mime_type = content_type;
            data = field
                .bytes()
                .await
                .map_err(|e| ApiError::BadRequest(format!("failed to read uploaded file: {e}")))?
                .to_vec();
        }
    }

    let filename = filename.ok_or_else(|| ApiError::BadRequest("file field is required".into()))?;
    let mime_type = mime_type.unwrap_or_else(|| "application/octet-stream".to_string());

    // 验证文件名与类型：只有会被摄入的文件才接受
    if !is_supported_upload(&mime_type, &filename) {
        return Err(ApiError::BadRequest(format!(
            "file cannot be ingested (invalid name or unsupported type): {filename} ({mime_type})"
        )));
    }

    let file_size = data.len() as i64;

    // 保存文件到磁盘
    let docs_dir = reconcile::docs_dir(&state.workspace_manager.workspace_dir(&user_id), &kb_name);
    tokio::fs::create_dir_all(&docs_dir)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create docs directory: {e}")))?;

    let filepath = docs_dir.join(&filename);
    tokio::fs::write(&filepath, &data)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to save file: {e}")))?;

    // 用返回的行 id 回写状态：重复上传时它可能与新建的候选 id 不同。
    let doc_id = documents::upsert_pending(
        &state.db,
        &documents::CreateDocumentParams {
            kb_id: kb_id.clone(),
            filename: filename.clone(),
            filepath: filepath.to_string_lossy().to_string(),
            file_size,
            mime_type: mime_type.clone(),
            relative_path: filename.clone(),
        },
    )
    .await?;

    // 后台异步处理：同步知识库以摄入文件
    let doc_id_for_bg = doc_id.clone();
    tokio::spawn(async move {
        // 更新状态为 processing
        let _ = documents::update_status(&db_for_bg, &doc_id_for_bg, "processing", None).await;

        match wsm_for_bg.get(&user_id_for_bg) {
            Ok(ws) => match ws.knowledge_manager().sync_kb(&kb_name_for_bg).await {
                Ok(report) => {
                    // 终态由 manifest 投影决定，解析失败的文件不能被无条件标成 ready
                    let docs_dir = reconcile::docs_dir(
                        &wsm_for_bg.workspace_dir(&user_id_for_bg),
                        &kb_name_for_bg,
                    );
                    if let Err(e) = reconcile::reconcile_documents(
                        &db_for_bg,
                        ws.knowledge_manager(),
                        &kb_id_for_bg,
                        &kb_name_for_bg,
                        &docs_dir,
                        &report,
                    )
                    .await
                    {
                        warn!(
                            doc_id = %doc_id_for_bg,
                            kb = %kb_name_for_bg,
                            error = %e,
                            "Document reconciliation failed"
                        );
                    }
                    info!(
                        doc_id = %doc_id_for_bg,
                        kb = %kb_name_for_bg,
                        "Document processed successfully"
                    );
                }
                Err(e) => {
                    let _ = documents::update_status(
                        &db_for_bg,
                        &doc_id_for_bg,
                        "error",
                        Some(&e.to_string()),
                    )
                    .await;
                    error!(
                        doc_id = %doc_id_for_bg,
                        kb = %kb_name_for_bg,
                        error = %e,
                        "Document processing failed"
                    );
                }
            },
            Err(e) => {
                let _ = documents::update_status(
                    &db_for_bg,
                    &doc_id_for_bg,
                    "error",
                    Some(&format!("workspace error: {e}")),
                )
                .await;
            }
        }
    });

    let response = DocumentResponse {
        id: doc_id,
        kb_id,
        filename,
        file_size,
        mime_type,
        status: "pending".to_string(),
        error_msg: None,
        created_at: String::new(),
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// `POST /api/knowledge/:id/sync`
///
/// 手动触发知识库同步。
pub async fn sync_knowledge_base(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(kb_id): Path<String>,
) -> Result<Json<SyncResponse>, ApiError> {
    let row = knowledge_bases::find_by_id_and_user(&state.db, &kb_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("knowledge base '{kb_id}' not found")))?;

    let km = get_user_km(&state, &user_id)?;
    let report = km
        .sync_kb(&row.name)
        .await
        .map_err(|e| ApiError::Internal(format!("sync failed: {e}")))?;

    // 同步结果投影到 documents 表，让 REST 面与 KB 后端一致
    let docs_dir = reconcile::docs_dir(&state.workspace_manager.workspace_dir(&user_id), &row.name);
    reconcile::reconcile_documents(&state.db, &km, &kb_id, &row.name, &docs_dir, &report).await?;

    // 更新 KB 时间戳
    let _ = knowledge_bases::touch(&state.db, &kb_id).await;

    info!(user_id = %user_id, kb_id = %kb_id, name = %row.name, added = report.added, updated = report.updated, "Knowledge base synced");
    Ok(Json(SyncResponse {
        kb_name: report.kb_name,
        added: report.added,
        updated: report.updated,
        removed: report.removed,
        skipped: report.skipped,
        errors: report.errors,
        duration_ms: report.duration_ms,
    }))
}

/// `DELETE /api/knowledge/:id/documents/:doc_id`
///
/// 删除单个文档（SQLite 记录 + 磁盘文件）。
pub async fn delete_document(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path((kb_id, doc_id)): Path<(String, String)>,
) -> Result<Json<SuccessResponse>, ApiError> {
    // 验证 KB 归属
    knowledge_bases::find_by_id_and_user(&state.db, &kb_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("knowledge base '{kb_id}' not found")))?;

    // 查找文档记录
    let doc = documents::find_by_id(&state.db, &doc_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("document '{doc_id}' not found")))?;

    if doc.kb_id != kb_id {
        return Err(ApiError::Forbidden(
            "document does not belong to this knowledge base".into(),
        ));
    }

    // 删除磁盘文件
    let filepath = std::path::PathBuf::from(&doc.filepath);
    if filepath.exists() {
        let _ = tokio::fs::remove_file(&filepath).await;
    }

    // 从 SQLite 删除
    documents::delete(&state.db, &doc_id).await?;

    info!(
        user_id = %user_id,
        kb_id = %kb_id,
        doc_id = %doc_id,
        filename = %doc.filename,
        "Document deleted"
    );

    Ok(Json(SuccessResponse { success: true }))
}

// ── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 上传准入必须与摄入扫描的扩展名集合一致，否则会被接受却永不被索引。
    #[test]
    fn upload_gate_matches_ingestion_extension_set() {
        for (mime, filename) in [("text/markdown", "a.md"), ("application/pdf", "a.pdf")] {
            assert!(
                is_supported_upload_with(mime, filename, false),
                "{filename} 应被接受"
            );
        }

        // Docling 关闭时 docx 不属于可摄入集合，准入必须同样拒绝
        assert!(
            !is_supported_upload_with(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                "a.docx",
                false
            ),
            "Docling 关闭时 .docx 上传必须被拒绝"
        );

        assert!(
            !is_supported_upload_with("text/plain", "server.log", false),
            ".log 不会被摄入，必须在入口拒绝"
        );
    }

    /// Docling 开启时 docx/pptx/xlsx 的 MIME 与扩展名同时命中 → 准入。
    #[test]
    fn upload_gate_accepts_docling_formats_when_enabled() {
        for (mime, name) in [
            (
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                "a.docx",
            ),
            (
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                "a.pptx",
            ),
            (
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                "a.xlsx",
            ),
        ] {
            assert!(
                !is_supported_upload_with(mime, name, false),
                "{name} 关闭时应拒"
            );
            assert!(
                is_supported_upload_with(mime, name, true),
                "{name} 开启时应收"
            );
        }
    }

    /// 隐藏文件与含路径分隔符的名字必须在入口拒绝。
    #[test]
    fn upload_rejects_hidden_and_path_like_filenames() {
        for filename in [".hidden.md", "sub/a.md", "../a.md", "sub\\a.md", "   "] {
            assert!(
                !is_supported_upload("text/markdown", filename),
                "{filename} 不应被接受"
            );
        }

        assert!(is_supported_upload("text/markdown", "a.md"));
        assert!(is_supported_upload("application/pdf", "a.pdf"));
    }

    /// REST 建库路径默认走 HelixDB 后端。
    ///
    /// 只覆盖纯逻辑：handler 本身依赖 `AppState`（SQLite 池 + 迁移 +
    /// workspace 目录）与 `AuthUser`，本文件没有现成的测试夹具，搭一套
    /// 的成本远高于被测决策本身。`build_kb_config` 是 `create_knowledge_base`
    /// 中唯一决定后端的表达式，覆盖它即覆盖该决策。
    #[test]
    fn create_kb_config_defaults_to_helixdb() {
        let cfg = build_kb_config(
            "test-kb".to_string(),
            "desc".to_string(),
            FastembedModelTypeSerde::BGEBaseZHV15,
            ChunkingStrategySerde::default(),
        );

        assert!(matches!(cfg.backend, BackendType::HelixDb));
        // 端点仍是部署级回退（PECO_KB_HELIX_URL → localhost:6970），不随 REST 入参下发
        assert!(cfg.helix_url.is_none());
        // 存储路径由 KB 自行派生，REST 不指定
        assert!(cfg.storage_path.is_none());
        assert_eq!(cfg.name, "test-kb");
        assert_eq!(cfg.description, "desc");
    }
}
