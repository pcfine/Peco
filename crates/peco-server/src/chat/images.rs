// ============================================================================
// 会话图片上传 — 用户图片进入对话的上传与解析通路
// ============================================================================
//
// 上传：multipart 保存到 `{data_dir}/uploads/chat/{user_id}/`（按用户目录隔离），
// 返回文件名作为引用 id；静态服务经 `/uploads/chat/...` 直出供前端渲染。
// 发送：stream 请求携带 `image_ids`，此处解析为 data URI 图片部件进入
// 消息 Content —— 引用不属于自己的图片一律 404。

use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum_extra::extract::Multipart;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use model_provider::ContentPart;
use serde::Serialize;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::db::conversations;
use crate::error::ApiError;
use crate::state::AppState;

/// 允许上传的图片 MIME 类型。
const ALLOWED_MIME_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// 单张图片大小上限：10 MB。
const MAX_IMAGE_SIZE: usize = 10 * 1024 * 1024;

/// 上传响应：`id` 用于发送消息时引用，`url` 用于前端直接渲染。
#[derive(Debug, Serialize)]
pub struct ConversationImageResponse {
    pub id: String,
    pub url: String,
}

/// 会话图片存储目录：`{data_dir}/uploads/chat/{user_id}/`。
fn chat_upload_dir(data_dir: &StdPath, user_id: &str) -> PathBuf {
    data_dir.join("uploads").join("chat").join(user_id)
}

/// `POST /api/chat/{agentId}/conversations/{id}/images`
///
/// 接受 multipart form-data，字段名 `file`。会话归属校验通过后
/// 保存图片并返回引用 id 与访问 URL。
pub async fn upload_conversation_image(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
    Path((_agent_id, conv_id)): Path<(String, String)>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<ConversationImageResponse>), ApiError> {
    // 会话归属校验：仅对话所有者可向其上传图片
    conversations::find_by_id_and_user(&state.db, &conv_id, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    let mut content_type: Option<String> = None;
    let mut data: Vec<u8> = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("file") {
            content_type = field.content_type().map(|s| s.to_string());
            data = field
                .bytes()
                .await
                .map_err(|e| ApiError::BadRequest(format!("failed to read upload: {e}")))?
                .to_vec();
            break;
        }
    }

    let mime = content_type
        .ok_or_else(|| ApiError::BadRequest("missing 'file' field in upload".into()))?;
    if !ALLOWED_MIME_TYPES.contains(&mime.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "unsupported file type: {mime}. allowed: png, jpeg, gif, webp"
        )));
    }
    if data.is_empty() {
        return Err(ApiError::BadRequest("empty file".into()));
    }
    if data.len() > MAX_IMAGE_SIZE {
        return Err(ApiError::BadRequest(format!(
            "file too large: {} bytes (max {MAX_IMAGE_SIZE})",
            data.len()
        )));
    }

    let ext = match mime.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    };
    let filename = format!("{}.{ext}", Uuid::new_v4());
    let dir = chat_upload_dir(&state.data_dir, &user_id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create upload dir: {e}")))?;
    tokio::fs::write(dir.join(&filename), &data)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to write uploaded file: {e}")))?;

    let url = format!("/uploads/chat/{user_id}/{filename}");
    tracing::info!(
        user_id = %user_id,
        conversation_id = %conv_id,
        %url,
        size = data.len(),
        "Conversation image uploaded"
    );

    Ok((
        StatusCode::CREATED,
        Json(ConversationImageResponse { id: filename, url }),
    ))
}

/// 校验图片引用文件名（uuid 文件名字符集），拒绝路径遍历。
fn validate_image_id(id: &str) -> Result<(), ApiError> {
    let safe = !id.is_empty()
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if safe {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!("invalid image id: {id}")))
    }
}

/// 将用户引用的图片 id 解析为消息图片部件（data URI）。
///
/// 存储按用户目录隔离：引用不属于自己的图片一律 404。
pub(crate) async fn resolve_image_parts(
    state: &AppState,
    user_id: &str,
    image_ids: &[String],
) -> Result<Vec<ContentPart>, ApiError> {
    let mut parts = Vec::with_capacity(image_ids.len());
    for id in image_ids {
        validate_image_id(id)?;
        let path = chat_upload_dir(&state.data_dir, user_id).join(id);
        let data = tokio::fs::read(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ApiError::NotFound(format!("image '{id}' not found"))
            } else {
                ApiError::Internal(format!("failed to read image: {e}"))
            }
        })?;
        parts.push(ContentPart::Image {
            url: format!(
                "data:{};base64,{}",
                mime_from_extension(id),
                BASE64_STANDARD.encode(data)
            ),
            detail: None,
        });
    }
    Ok(parts)
}

fn mime_from_extension(filename: &str) -> &'static str {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_image_id_accepts_uuid_filenames() {
        assert!(validate_image_id("0f7d3a9c-1234-4a5b-8c6d-9e0f1a2b3c4d.png").is_ok());
        assert!(validate_image_id("a1B2_3-.jpg").is_ok());
    }

    #[test]
    fn test_validate_image_id_rejects_traversal_and_empty() {
        assert!(validate_image_id("..").is_err());
        assert!(validate_image_id("a/../b.png").is_err());
        assert!(validate_image_id("").is_err());
        assert!(validate_image_id("img/../../etc.png").is_err());
        assert!(validate_image_id("白.png").is_err());
    }

    #[test]
    fn test_mime_from_extension() {
        assert_eq!(mime_from_extension("a.png"), "image/png");
        assert_eq!(mime_from_extension("b.JPG"), "image/jpeg");
        assert_eq!(mime_from_extension("c"), "application/octet-stream");
    }
}
