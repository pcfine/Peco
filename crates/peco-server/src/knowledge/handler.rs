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
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::db::{documents, knowledge_bases};
use crate::error::ApiError;
use crate::state::AppState;

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

/// 解析嵌入模型字符串为 FastembedModelTypeSerde。
fn parse_embedding_model(s: Option<&str>) -> FastembedModelTypeSerde {
    match s {
        Some("bge-large-zh-v15") => FastembedModelTypeSerde::BGELargeZHV15,
        Some("all-minilm-l6-v2q") | Some("all-MiniLM-L6-v2") => {
            FastembedModelTypeSerde::AllMiniLML6V2Q
        }
        Some("multilingual-e5-small") => FastembedModelTypeSerde::MultilingualE5Small,
        _ => FastembedModelTypeSerde::BGESmallZHV15, // 默认中文模型
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

/// 允许上传的 MIME 类型。
fn is_allowed_mime(mime: &str) -> bool {
    matches!(
        mime,
        "application/pdf"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "text/html"
            | "text/markdown"
            | "text/plain"
            | "text/x-python"
            | "text/x-rust"
            | "text/x-go"
            | "application/javascript"
            | "text/typescript"
    ) || mime.starts_with("text/")
}

/// 获取用户 KnowledgeManager (via Workspace).
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

    let mut responses = Vec::with_capacity(rows.len());
    for row in &rows {
        // 尝试获取知识库统计信息
        let (doc_count, chunk_count) = match get_user_km(&state, &user_id) {
            Ok(km) => match km.list_kbs().await {
                Ok(infos) => {
                    let info = infos.iter().find(|i| i.name == row.name);
                    info.map(|i| (i.document_count, i.chunk_count))
                        .unwrap_or((0, 0))
                }
                Err(_) => (0, 0),
            },
            Err(_) => (0, 0),
        };

        responses.push(KnowledgeBaseResponse {
            id: row.id.clone(),
            name: row.name.clone(),
            description: row.description.clone(),
            backend: "LanceDB".to_string(),
            embedding_model: "BGESmallZHV15".to_string(),
            document_count: doc_count,
            chunk_count,
            created_at: row.created_at.clone(),
            updated_at: row.updated_at.clone(),
        });
    }

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
    if name.is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
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
    let kb_config = KbConfig {
        name: name.clone(),
        description: req.description.clone(),
        embedding_model,
        chunking,
        backend: BackendType::LanceDb,
        storage_path: None,
    };

    km.create_kb(kb_config)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create knowledge base: {e}")))?;

    let response = KnowledgeBaseResponse {
        id: kb_id,
        name,
        description: req.description,
        backend: "LanceDB".to_string(),
        embedding_model: "BGESmallZHV15".to_string(),
        document_count: 0,
        chunk_count: 0,
        created_at: String::new(), // 由 DB 填充
        updated_at: String::new(),
    };

    tracing::info!(
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

    let (doc_count, chunk_count) = match get_user_km(&state, &user_id) {
        Ok(km) => match km.list_kbs().await {
            Ok(infos) => {
                let info = infos.iter().find(|i| i.name == row.name);
                info.map(|i| (i.document_count, i.chunk_count))
                    .unwrap_or((0, 0))
            }
            Err(_) => (0, 0),
        },
        Err(_) => (0, 0),
    };

    Ok(Json(KnowledgeBaseResponse {
        id: row.id,
        name: row.name,
        description: row.description,
        backend: "LanceDB".to_string(),
        embedding_model: "BGESmallZHV15".to_string(),
        document_count: doc_count,
        chunk_count,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

/// `DELETE /api/knowledge/:id`
///
/// 删除知识库及其所有数据（SQLite 记录 + 磁盘文件 + LanceDB 数据）。
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

    // 3. 通过 KnowledgeBaseManager 删除知识库（含磁盘 + LanceDB 数据）
    if let Ok(km) = get_user_km(&state, &user_id)
        && let Err(e) = km.delete_kb(&kb_name).await
    {
        tracing::warn!(
            kb_name = %kb_name,
            error = %e,
            "KnowledgeBaseManager failed to delete KB data (may already be removed)"
        );
    }

    tracing::info!(
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

    // 验证 MIME 类型
    if !is_allowed_mime(&mime_type) {
        return Err(ApiError::BadRequest(format!(
            "unsupported file type: {mime_type}"
        )));
    }

    let file_size = data.len() as i64;

    // 保存文件到磁盘
    let sanitized = knowledge_base::sanitize_kb_name(&kb_name);
    let docs_dir = state
        .workspace_manager
        .workspace_dir(&user_id)
        .join("knowledge")
        .join(sanitized)
        .join("docs");
    tokio::fs::create_dir_all(&docs_dir)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create docs directory: {e}")))?;

    let filepath = docs_dir.join(&filename);
    tokio::fs::write(&filepath, &data)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to save file: {e}")))?;

    // 写入 documents 表（status=pending）
    let doc_id = Uuid::new_v4().to_string();
    documents::insert(
        &state.db,
        &documents::CreateDocumentParams {
            id: doc_id.clone(),
            kb_id: kb_id.clone(),
            filename: filename.clone(),
            filepath: filepath.to_string_lossy().to_string(),
            file_size,
            mime_type: mime_type.clone(),
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
                Ok(_report) => {
                    let _ =
                        documents::update_status(&db_for_bg, &doc_id_for_bg, "ready", None).await;
                    tracing::info!(
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
                    tracing::error!(
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

    // 更新 KB 时间戳
    let _ = knowledge_bases::touch(&state.db, &kb_id).await;

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
/// 删除单个文档（SQLite 记录 + 磁盘文件 + 向量数据）。
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

    tracing::info!(
        user_id = %user_id,
        kb_id = %kb_id,
        doc_id = %doc_id,
        filename = %doc.filename,
        "Document deleted"
    );

    Ok(Json(SuccessResponse { success: true }))
}
