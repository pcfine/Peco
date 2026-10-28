//! 产物内容 / 下载端点。
//!
//! 走 `ServeFile` 而不是整块读入：视频拖进度条依赖 `Range`，前端 `<video>`
//! 默认就会发 `Range: bytes=0-`，自己实现容易漏掉 206 与 `Content-Range`。

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::HeaderValue;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::auth::AuthUser;
use crate::db::artifacts as db_artifacts;
use crate::db::artifacts::ArtifactRow;
use crate::error::ApiError;
use crate::state::AppState;

/// 内联返回。需带 `Authorization` —— 浏览器 `<img src>` 不带 Bearer，
/// 前端须用 fetch 取 blob（阶段 2）。
pub async fn content(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(id): Path<String>,
    req: Request,
) -> Result<Response, ApiError> {
    let (row, path) = lookup(&state, &user, &id).await?;
    serve(&path, &row, req, false).await
}

/// 下载返回（`Content-Disposition: attachment`，强制落盘不渲染）。
pub async fn download(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(id): Path<String>,
    req: Request,
) -> Result<Response, ApiError> {
    let (row, path) = lookup(&state, &user, &id).await?;
    serve(&path, &row, req, true).await
}

/// 归属校验 + 路径定位。
///
/// 只按 `id + user_id` 查：他人产物一律 404（不泄露存在性）。
async fn lookup(
    state: &AppState,
    user: &AuthUser,
    id: &str,
) -> Result<(ArtifactRow, PathBuf), ApiError> {
    let row = db_artifacts::find_by_id_and_user(&state.db, id, &user.user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("artifact '{id}' not found")))?;

    let (root, _) = super::work_root(&state.workspace_manager, &user.user_id);
    let path = root.join(&row.rel_path);

    if !path.is_file() {
        // 文件被清理但登记还在 —— 与列表端点的 `available: false` 对齐。
        return Err(ApiError::NotFound(format!(
            "artifact '{id}' file is no longer available"
        )));
    }
    Ok((row, path))
}

async fn serve(
    path: &PathBuf,
    row: &ArtifactRow,
    req: Request,
    attachment: bool,
) -> Result<Response, ApiError> {
    let res = ServeFile::new(path)
        .oneshot(req)
        .await
        .map_err(|e| ApiError::Internal(format!("artifact serve failed: {e}")))?;

    let mut res = res.map(Body::new);
    let headers = res.headers_mut();

    // DB 里的 MIME 优先于按扩展名嗅探（声明可覆盖）；内联时再按白名单收窄。
    // 下载走 attachment，浏览器不渲染，保留真实类型即可。
    let mime = if attachment {
        row.mime.as_str()
    } else {
        inline_mime(&row.mime)
    };
    if let Ok(value) = HeaderValue::from_str(mime) {
        headers.insert(CONTENT_TYPE, value);
    }
    // 即便 MIME 被降级为 octet-stream，也禁止浏览器自作主张嗅探。
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    let disposition = if attachment {
        format!(
            "attachment; filename*=UTF-8''{}",
            rfc5987_encode(&row.title)
        )
    } else {
        format!("inline; filename*=UTF-8''{}", rfc5987_encode(&row.title))
    };
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(CONTENT_DISPOSITION, value);
    }

    Ok(res.into_response())
}

/// 内联返回的类型白名单。
///
/// skill 脚本可以声明任意 MIME：`text/html` 或 `image/svg+xml` 在同源内联返回
/// 就是存储型 XSS。名单外的类型一律降级为 `application/octet-stream`，
/// 浏览器只会下载不会渲染。文档类（PDF/文本）要预览时再单独放开。
fn inline_mime(mime: &str) -> &'static str {
    // 先截掉参数：`video/mp4; codecs=avc1` 是完全合法的声明，不能因为带参数
    // 就降级成二进制流（那样 <video> 永远播不了）。
    let base = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "image/jpeg" => "image/jpeg",
        "image/png" => "image/png",
        "image/webp" => "image/webp",
        "image/gif" => "image/gif",
        "image/avif" => "image/avif",
        "image/bmp" => "image/bmp",
        "image/heic" => "image/heic",
        "video/mp4" => "video/mp4",
        "video/webm" => "video/webm",
        "video/quicktime" => "video/quicktime",
        "video/x-matroska" => "video/x-matroska",
        "audio/mpeg" => "audio/mpeg",
        "audio/mp4" | "audio/x-m4a" => "audio/mp4",
        "audio/wav" | "audio/x-wav" => "audio/wav",
        "audio/ogg" => "audio/ogg",
        "audio/webm" => "audio/webm",
        _ => "application/octet-stream",
    }
}

/// RFC 5987 `attr-char` 之外一律百分号编码（中文标题走 `filename*=UTF-8''`）。
fn rfc5987_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        let c = *byte as char;
        let allowed = c.is_ascii_alphanumeric()
            || matches!(
                c,
                '!' | '#' | '$' | '&' | '+' | '-' | '.' | '^' | '_' | '`' | '|' | '~'
            );
        if allowed {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_mime_rejects_renderable_active_content() {
        // 脚本可声明任意 MIME —— 这两种在同源内联就是存储型 XSS
        assert_eq!(inline_mime("text/html"), "application/octet-stream");
        assert_eq!(inline_mime("image/svg+xml"), "application/octet-stream");
        assert_eq!(
            inline_mime("application/javascript"),
            "application/octet-stream"
        );
        // 媒体保留，大小写与空白不影响
        assert_eq!(inline_mime("Image/JPEG"), "image/jpeg");
        assert_eq!(inline_mime(" video/mp4 "), "video/mp4");
        assert_eq!(inline_mime("audio/mpeg"), "audio/mpeg");
        // 带参数与常见变体不能被误拦
        assert_eq!(inline_mime("video/mp4; codecs=avc1,mp4a"), "video/mp4");
        assert_eq!(inline_mime("audio/webm"), "audio/webm");
        assert_eq!(inline_mime("audio/x-wav"), "audio/wav");
        assert_eq!(inline_mime("image/avif"), "image/avif");
    }

    #[test]
    fn encodes_non_ascii_title() {
        let encoded = rfc5987_encode("红狐狸.jpg");
        assert!(encoded.is_ascii(), "must be ASCII header-safe: {encoded}");
        assert!(encoded.starts_with("%E7%BA%A2"));
        assert!(encoded.ends_with(".jpg"));
    }

    #[test]
    fn keeps_attr_chars_and_encodes_spaces() {
        assert_eq!(rfc5987_encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(rfc5987_encode("a b"), "a%20b");
        // 分号与引号会破坏 header 语义，必须编码
        assert_eq!(rfc5987_encode("a;b"), "a%3Bb");
        assert_eq!(rfc5987_encode("a\"b"), "a%22b");
    }
}
