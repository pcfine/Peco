// ============================================================================
// 文件上传 Handler — Agent 图标等静态资源
// ============================================================================

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum_extra::extract::Multipart;
use serde::Serialize;
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

/// 允许上传的图片 MIME 类型。
const ALLOWED_MIME_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// 最大文件大小：5 MB。
const MAX_FILE_SIZE: usize = 5 * 1024 * 1024;

/// 文件扩展名映射。
fn extension_from_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    }
}

/// 上传成功响应。
#[derive(Debug, Serialize)]
pub struct UploadResponse {
    pub url: String,
}

/// `POST /api/upload`
///
/// 接受 multipart form-data，字段名 `file`。
/// 保存文件到 `{data_dir}/uploads/agents/{uuid}.{ext}` 并返回访问 URL。
pub async fn upload(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResponse>), ApiError> {
    let mut content_type: Option<String> = None;
    let mut data: Vec<u8> = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().map(|s| s.to_string());

        if name.as_deref() == Some("file") {
            content_type = field.content_type().map(|s| s.to_string());
            data = field
                .bytes()
                .await
                .map_err(|e| ApiError::BadRequest(format!("failed to read upload: {e}")))?
                .to_vec();
        }
    }

    // 未找到 file 字段
    let mime = content_type
        .ok_or_else(|| ApiError::BadRequest("missing 'file' field in upload".into()))?;

    // 验证 MIME 类型
    if !ALLOWED_MIME_TYPES.contains(&mime.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "unsupported file type: {mime}. allowed: png, jpeg, gif, webp"
        )));
    }

    // 验证文件大小
    if data.len() > MAX_FILE_SIZE {
        return Err(ApiError::BadRequest(format!(
            "file too large: {} bytes (max {MAX_FILE_SIZE})",
            data.len()
        )));
    }

    if data.is_empty() {
        return Err(ApiError::BadRequest("empty file".into()));
    }

    // ── 保存文件 ──────────────────────────────────────────────────────────
    let ext = extension_from_mime(&mime);
    let filename = format!("{}.{ext}", Uuid::new_v4());
    let uploads_dir = state.data_dir.join("uploads").join("agents");
    tokio::fs::create_dir_all(&uploads_dir)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create upload dir: {e}")))?;

    let filepath = uploads_dir.join(&filename);
    tokio::fs::write(&filepath, &data)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to write uploaded file: {e}")))?;

    let url = format!("/uploads/agents/{filename}");
    tracing::info!(%url, size = data.len(), "File uploaded");

    Ok((StatusCode::CREATED, Json(UploadResponse { url })))
}
