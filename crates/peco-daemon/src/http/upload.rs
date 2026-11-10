//! `POST /api/upload` —— 上传图片并返回 `/uploads/...` URL。

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum_extra::extract::Multipart;
use serde::Serialize;
use uuid::Uuid;

use crate::api_error::ApiError;
use crate::chat::images::{MAX_IMAGE_SIZE, validate_image};
use crate::state::AppState;

/// 上传请求体上限：图片本身 10 MB，另留 multipart 编码边界等开销。
pub const MAX_UPLOAD_BODY_BYTES: usize = MAX_IMAGE_SIZE + 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct UploadResponse {
    pub url: String,
}

pub async fn upload_image(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResponse>), ApiError> {
    let mut content_type: Option<String> = None;
    let mut data: Vec<u8> = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("file") {
            content_type = field.content_type().map(str::to_string);
            data = field
                .bytes()
                .await
                .map_err(|e| ApiError::BadRequest(format!("failed to read upload: {e}")))?
                .to_vec();
            break;
        }
    }

    let ext = validate_image(content_type.as_deref(), &data)?;
    let filename = format!("{}.{ext}", Uuid::new_v4());
    let dir = state.config.peco_home.join("uploads").join("teams");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create upload dir: {e}")))?;
    tokio::fs::write(dir.join(&filename), &data)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to write uploaded file: {e}")))?;

    let url = format!("/uploads/teams/{filename}");
    tracing::info!(url = %url, size = data.len(), "image uploaded");

    Ok((StatusCode::CREATED, Json(UploadResponse { url })))
}
