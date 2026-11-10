// ============================================================================
// 会话图片上传 — 用户图片进入对话的上传与解析通路
// ============================================================================
//
// 上传：multipart 保存到 `$PECO_HOME/uploads/chat/<ws_id>/`（按空间目录隔离），
// 返回文件名作为引用 id。
// 发送：stream 请求携带 `image_ids`，此处解析为 data URI 图片部件进入
// 消息 Content —— 引用不属于本空间的图片一律 404。
//
// 落盘根用 `$PECO_HOME` 而不是空间的 work 目录：图片是对话输入而非工作产物，
// 放进 work 目录会被 `artifact` 的「output 下即可发布」规则扫到。

use std::path::{Path as StdPath, PathBuf};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum_extra::extract::Multipart;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use model_provider::ContentPart;
use serde::Serialize;
use uuid::Uuid;

use crate::api_error::ApiError;
use crate::auth::LocalUser;
use crate::db::conversations;
use crate::state::AppState;

/// 允许上传的图片 MIME 类型。
pub(crate) const ALLOWED_MIME_TYPES: &[&str] =
    &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// 单张图片大小上限：10 MB。
pub(crate) const MAX_IMAGE_SIZE: usize = 10 * 1024 * 1024;

/// 上传请求体上限：图片本身 10 MB，另留 multipart 编码边界等开销。
/// 路由层用 `DefaultBodyLimit` 抬高 axum multipart 默认的 2 MB body 限制，
/// 否则本模块的大小检查永远不可达。
pub(crate) const MAX_UPLOAD_BODY_BYTES: usize = MAX_IMAGE_SIZE + 1024 * 1024;

/// 上传响应：`id` 用于发送消息时引用，`url` 用于前端直接渲染。
#[derive(Debug, Serialize)]
pub struct ConversationImageResponse {
    pub id: String,
    pub url: String,
}

/// 会话图片存储目录：`$PECO_HOME/uploads/chat/<ws_id>/`。
///
/// 单用户 daemon 用空间 id 替代用户 id 作目录名 —— 目录名必须随「谁的历史」
/// 变化，否则两个空间的同名引用（都是 uuid，不会撞）之外的文件权限就无从判定。
pub(crate) fn chat_upload_dir(home: &StdPath, ws_id: &str) -> PathBuf {
    home.join("uploads").join("chat").join(ws_id)
}

/// 校验图片 MIME 与大小，返回扩展名。
pub(crate) fn validate_image(
    content_type: Option<&str>,
    data: &[u8],
) -> Result<&'static str, ApiError> {
    let mime = content_type
        .ok_or_else(|| ApiError::BadRequest("missing 'file' field in upload".into()))?;
    if !ALLOWED_MIME_TYPES.contains(&mime) {
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
    Ok(match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    })
}

/// multipart 校验 + 落盘。
///
/// 字段名 `file`，MIME/大小校验通过后写入该空间的上传目录，
/// 返回 `(文件名, 访问 URL, 字节数)`。
pub(crate) async fn store_uploaded_image(
    state: &AppState,
    ws_id: &str,
    mut multipart: Multipart,
) -> Result<(String, String, usize), ApiError> {
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

    let ext = validate_image(content_type.as_deref(), &data)?;
    let filename = format!("{}.{ext}", Uuid::new_v4());
    let dir = chat_upload_dir(&state.config.peco_home, ws_id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create upload dir: {e}")))?;
    tokio::fs::write(dir.join(&filename), &data)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to write uploaded file: {e}")))?;

    let url = format!("/uploads/chat/{ws_id}/{filename}");
    Ok((filename, url, data.len()))
}

/// `POST /api/chat/{agentId}/conversations/{id}/images`
///
/// 接受 multipart form-data，字段名 `file`。会话归属校验通过后
/// 保存图片并返回引用 id 与访问 URL。
pub async fn upload_conversation_image(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path((_agent_id, conv_id)): Path<(String, String)>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<ConversationImageResponse>), ApiError> {
    // 会话归属校验：只能向本空间的会话上传图片
    conversations::find_by_id_and_workspace(&state.pool, &conv_id, &ws_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    let (filename, url, size) = store_uploaded_image(&state, &ws_id, multipart).await?;
    tracing::info!(
        ws_id = %ws_id,
        conversation_id = %conv_id,
        %url,
        size,
        "Conversation image uploaded"
    );

    Ok((
        StatusCode::CREATED,
        Json(ConversationImageResponse { id: filename, url }),
    ))
}

/// 校验图片引用文件名（uuid 文件名字符集），拒绝路径遍历。
pub(crate) fn validate_image_id(id: &str) -> Result<(), ApiError> {
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
/// 存储按空间目录隔离：引用不属于本空间的图片一律 404。
pub(crate) async fn resolve_image_parts(
    state: &AppState,
    ws_id: &str,
    image_ids: &[String],
) -> Result<Vec<ContentPart>, ApiError> {
    let mut parts = Vec::with_capacity(image_ids.len());
    for id in image_ids {
        validate_image_id(id)?;
        let path = chat_upload_dir(&state.config.peco_home, ws_id).join(id);
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

pub(crate) fn mime_from_extension(filename: &str) -> &'static str {
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
    fn validate_image_maps_mime_to_extension() {
        assert_eq!(validate_image(Some("image/png"), b"x").unwrap(), "png");
        assert_eq!(validate_image(Some("image/jpeg"), b"x").unwrap(), "jpg");
        assert_eq!(validate_image(Some("image/webp"), b"x").unwrap(), "webp");
    }

    #[test]
    fn validate_image_rejects_bad_mime_and_empty() {
        assert!(validate_image(Some("image/svg+xml"), b"x").is_err());
        assert!(validate_image(Some("image/png"), b"").is_err());
        assert!(validate_image(None, b"x").is_err());
    }

    #[test]
    fn test_mime_from_extension() {
        assert_eq!(mime_from_extension("a.png"), "image/png");
        assert_eq!(mime_from_extension("b.JPG"), "image/jpeg");
        assert_eq!(mime_from_extension("c"), "application/octet-stream");
    }

    /// 上传目录必须落在空间名下：两个空间共用一处就等于没有隔离。
    #[test]
    fn upload_dir_is_scoped_by_space() {
        let a = chat_upload_dir(StdPath::new("/home"), "ws_a");
        let b = chat_upload_dir(StdPath::new("/home"), "ws_b");
        assert_eq!(a, StdPath::new("/home/uploads/chat/ws_a"));
        assert_ne!(a, b);
    }
}
