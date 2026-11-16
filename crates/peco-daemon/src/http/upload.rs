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

/// 落盘目录白名单：目录名只来自这张表，调用方给的是用途而不是路径。
fn target_dir(purpose: Option<&str>) -> Result<&'static str, ApiError> {
    match purpose {
        None | Some("teams") => Ok("teams"),
        Some("brand") => Ok("brand"),
        Some(other) => Err(ApiError::BadRequest(format!(
            "unsupported upload purpose: {other}. allowed: teams, brand"
        ))),
    }
}

pub async fn upload_image(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResponse>), ApiError> {
    let mut content_type: Option<String> = None;
    let mut data: Vec<u8> = Vec::new();
    let mut purpose: Option<String> = None;
    let mut seen_file = false;

    // 读到结尾而不再对 `file` 提前 break：`purpose` 可以和 `file` 任意先后。
    while let Ok(Some(field)) = multipart.next_field().await {
        let Some(name) = field.name().map(str::to_string) else {
            continue;
        };
        match name.as_str() {
            // 只认第一个 `file`：后来的同名字段不能让先到的那张图被悄悄换掉。
            "file" if !seen_file => {
                seen_file = true;
                content_type = field.content_type().map(str::to_string);
                data = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("failed to read upload: {e}")))?
                    .to_vec();
            }
            "purpose" if purpose.is_none() => {
                purpose =
                    Some(field.text().await.map_err(|e| {
                        ApiError::BadRequest(format!("failed to read purpose: {e}"))
                    })?);
            }
            _ => {}
        }
    }

    let dir_name = target_dir(purpose.as_deref())?;
    let ext = validate_image(content_type.as_deref(), &data)?;
    let filename = format!("{}.{ext}", Uuid::new_v4());
    let dir = state.config.peco_home.join("uploads").join(dir_name);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create upload dir: {e}")))?;
    tokio::fs::write(dir.join(&filename), &data)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to write uploaded file: {e}")))?;

    let url = format!("/uploads/{dir_name}/{filename}");
    tracing::info!(url = %url, size = data.len(), "image uploaded");

    Ok((StatusCode::CREATED, Json(UploadResponse { url })))
}
