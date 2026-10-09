// ============================================================================
// Peco 文件上传 — 图片与文档统一入口、按类型落盘与解析
// ============================================================================
//
// 上传：multipart 字段 `file` 按**扩展名**分类（显式白名单），图片复用
// `{data_dir}/uploads/chat/{user_id}/`（静态服务直出），文档落
// `{data_dir}/peco-files/{user_id}/`（**不返回 url** —— `/uploads` 无鉴权，
// 文档不得进入该目录）。原始文件名只进 sidecar，落盘与引用一律 uuid 名。
// 发送：stream 请求携带 `file_ids`，图片解析为 data URI 部件，文档经
// 服务端解析提取正文、截断到 token 预算后作为附件部件进入消息 Content。
//
// 架构前提：上游 Anthropic 端点不支持 document 块，附件只能以文本呈现，
// 故解析在服务端完成、模型侧只见提取出的正文。

use std::path::{Path as StdPath, PathBuf};
use std::time::Duration;

use axum_extra::extract::Multipart;
use model_provider::ContentPart;
use peco_core::agent::estimate_str_tokens;
use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::chat::images::{
    ALLOWED_MIME_TYPES, MAX_IMAGE_SIZE, chat_upload_dir, mime_from_extension,
};
use crate::error::ApiError;
use crate::state::AppState;

/// 附件分类结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Image,
    Document,
}

/// 上传响应：`id` 用于发送消息时引用，`url` 仅图片有（前端直接渲染）。
#[derive(Debug, Serialize)]
pub struct UploadedFile {
    pub id: String,
    pub kind: FileKind,
    /// 原始文件名（文档解析后作为附件标识呈现）。
    pub name: String,
    pub size: usize,
    /// 图片的静态访问 URL；文档恒缺省（不进静态目录）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// 允许上传的图片扩展名（与 `ALLOWED_MIME_TYPES` 对应）。
pub const IMAGE_EXT_WHITELIST: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// 允许上传的文档扩展名（docx 解析器未就绪，显式排除）。
pub const DOC_EXT_WHITELIST: &[&str] = &[
    "pdf", "md", "markdown", "txt", "text", "log", "html", "htm", "csv", "json", "yaml", "yml",
    "toml", "xml", "rs", "py", "js", "jsx", "ts", "tsx", "go", "java",
];

/// 单个文档大小上限：20 MB。
pub const MAX_DOC_SIZE: usize = 20 * 1024 * 1024;

/// 上传请求体上限：文档 20 MB + multipart 编码边界等开销。
pub const MAX_FILE_BODY_BYTES: usize = MAX_DOC_SIZE + 1024 * 1024;

/// 单轮附件总数上限（`file_ids` 与 `image_ids` 合并去重后计）。
pub const MAX_FILES_PER_TURN: usize = 8;

/// 文档提取正文的 token 预算。
pub const DOC_TEXT_MAX_TOKENS: usize = 8192;

/// 单个文档解析超时。
pub const PARSE_TIMEOUT: Duration = Duration::from_secs(10);

/// 解析失败/空正文的统一用户文案（服务端细节只进日志，不进响应）。
const PARSE_FAILED_MSG: &str = "未能从该文档提取文本（可能是扫描件），请改用文本型 PDF 或纯文本";

/// 文档存储目录：`{data_dir}/peco-files/{user_id}/`。
pub fn document_dir(data_dir: &StdPath, user_id: &str) -> PathBuf {
    data_dir.join("peco-files").join(user_id)
}

/// 取文件名的小写扩展名；无扩展名返回 `None`。
fn extension_of(filename: &str) -> Option<String> {
    StdPath::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
}

/// multipart 字段 `file` → 分类 + 落盘。
///
/// 只读一次 multipart：图片与文档分支各自落盘，避免二次消费。
pub async fn store_uploaded_file(
    state: &AppState,
    user_id: &str,
    mut multipart: Multipart,
) -> Result<UploadedFile, ApiError> {
    let mut filename: Option<String> = None;
    let mut content_type: Option<String> = None;
    let mut data: Vec<u8> = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("file") {
            filename = field.file_name().map(str::to_string);
            content_type = field.content_type().map(str::to_string);
            data = field
                .bytes()
                .await
                .map_err(|e| ApiError::BadRequest(format!("failed to read upload: {e}")))?
                .to_vec();
            break;
        }
    }

    let name = filename.ok_or_else(|| ApiError::BadRequest("missing file name".into()))?;
    let ext = extension_of(&name)
        .ok_or_else(|| ApiError::BadRequest("文件缺少扩展名，无法识别类型".into()))?;

    if data.is_empty() {
        return Err(ApiError::BadRequest("empty file".into()));
    }

    if IMAGE_EXT_WHITELIST.contains(&ext.as_str()) {
        store_image(state, user_id, &ext, content_type.as_deref(), &data).await
    } else if DOC_EXT_WHITELIST.contains(&ext.as_str()) {
        store_document(state, user_id, &name, &ext, &data).await
    } else if ext == "docx" {
        Err(ApiError::BadRequest(
            "暂不支持 docx，请转 PDF/Markdown/纯文本后重试".into(),
        ))
    } else {
        Err(ApiError::BadRequest(format!(
            "不支持的文件类型：{ext}。支持：{}",
            supported_list()
        )))
    }
    .map(|(id, kind, url)| UploadedFile {
        id,
        kind,
        name: name.clone(),
        size: data.len(),
        url,
    })
}

/// 支持的扩展名列表（错误文案用）。
fn supported_list() -> String {
    let mut all: Vec<&str> = IMAGE_EXT_WHITELIST.to_vec();
    all.extend_from_slice(DOC_EXT_WHITELIST);
    all.join(", ")
}

/// 图片落盘：校验 MIME/大小后写入 chat 上传目录，返回可直出的 URL。
async fn store_image(
    state: &AppState,
    user_id: &str,
    ext: &str,
    mime: Option<&str>,
    data: &[u8],
) -> Result<(String, FileKind, Option<String>), ApiError> {
    // MIME 与扩展名冲突时以扩展名为准，仅记录（分类依据是扩展名白名单）。
    match mime {
        Some(m) if !ALLOWED_MIME_TYPES.contains(&m) => {
            warn!(
                user_id,
                ext,
                mime = m,
                "upload mime conflicts with extension; extension wins"
            );
        }
        _ => {}
    }
    if data.len() > MAX_IMAGE_SIZE {
        return Err(ApiError::BadRequest(format!(
            "file too large: {} bytes (max {MAX_IMAGE_SIZE})",
            data.len()
        )));
    }

    let id = format!("{}.{ext}", Uuid::new_v4());
    let dir = chat_upload_dir(&state.data_dir, user_id);
    write_file(&dir, &id, data).await?;
    let url = format!("/uploads/chat/{user_id}/{id}");
    Ok((id, FileKind::Image, Some(url)))
}

/// 文档落盘：正文写 `{uuid}.{ext}`，原始文件名写同名 sidecar。
async fn store_document(
    state: &AppState,
    user_id: &str,
    name: &str,
    ext: &str,
    data: &[u8],
) -> Result<(String, FileKind, Option<String>), ApiError> {
    if data.len() > MAX_DOC_SIZE {
        return Err(ApiError::BadRequest(format!(
            "file too large: {} bytes (max {MAX_DOC_SIZE})",
            data.len()
        )));
    }

    let id = format!("{}.{ext}", Uuid::new_v4());
    let dir = document_dir(&state.data_dir, user_id);
    write_file(&dir, &id, data).await?;
    let sidecar = serde_json::json!({ "name": name });
    write_file(&dir, &sidecar_name(&id), sidecar.to_string().as_bytes()).await?;
    Ok((id, FileKind::Document, None))
}

/// `uuid.pdf` → `uuid.meta.json`（sidecar 与正文同 stem）。
fn sidecar_name(id: &str) -> String {
    StdPath::new(id)
        .with_extension("meta.json")
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{id}.meta.json"))
}

/// 建目录 + 写文件，错误收敛为 500。
async fn write_file(dir: &StdPath, filename: &str, data: &[u8]) -> Result<(), ApiError> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create upload dir: {e}")))?;
    tokio::fs::write(dir.join(filename), data)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to write uploaded file: {e}")))
}

/// 读取文档 sidecar 中的原始文件名；缺失回退为引用 id。
async fn original_name(dir: &StdPath, id: &str) -> String {
    let path = dir.join(sidecar_name(id));
    match tokio::fs::read_to_string(&path).await {
        Ok(raw) => serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .unwrap_or_else(|| id.to_string()),
        Err(_) => id.to_string(),
    }
}

/// `file_ids`（含 `image_ids`）→ 消息部件：图片转 data URI，文档解析为附件部件。
///
/// 存储按用户目录隔离：引用不属于自己的文件一律 404。
pub async fn resolve_file_parts(
    state: &AppState,
    user_id: &str,
    ids: &[String],
) -> Result<Vec<ContentPart>, ApiError> {
    let mut parts = Vec::with_capacity(ids.len());
    for id in ids {
        crate::chat::images::validate_image_id(id)?;
        let ext = extension_of(id).unwrap_or_default();
        if IMAGE_EXT_WHITELIST.contains(&ext.as_str()) {
            parts.push(resolve_image(state, user_id, id).await?);
        } else if DOC_EXT_WHITELIST.contains(&ext.as_str()) {
            parts.push(resolve_document(state, user_id, id).await?);
        } else {
            return Err(ApiError::BadRequest(format!(
                "不支持的文件类型：{ext}。支持：{}",
                supported_list()
            )));
        }
    }
    Ok(parts)
}

/// 图片引用 → data URI 图片部件。
async fn resolve_image(state: &AppState, user_id: &str, id: &str) -> Result<ContentPart, ApiError> {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

    let path = chat_upload_dir(&state.data_dir, user_id).join(id);
    let data = tokio::fs::read(&path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ApiError::NotFound(format!("file '{id}' not found"))
        } else {
            ApiError::Internal(format!("failed to read file: {e}"))
        }
    })?;
    Ok(ContentPart::Image {
        url: format!(
            "data:{};base64,{}",
            mime_from_extension(id),
            BASE64_STANDARD.encode(data)
        ),
        detail: None,
    })
}

/// 文档引用 → 解析、截断后的附件部件。
async fn resolve_document(
    state: &AppState,
    user_id: &str,
    id: &str,
) -> Result<ContentPart, ApiError> {
    let dir = document_dir(&state.data_dir, user_id);
    let path = dir.join(id);
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Err(ApiError::NotFound(format!("file '{id}' not found")));
    }
    let name = original_name(&dir, id).await;

    let parsed = timeout(PARSE_TIMEOUT, async {
        let parser =
            knowledge_base::parsers::make_parser(&path).map_err(|e| (0u8, e.to_string()))?;
        let doc = parser
            .parse_file(&path)
            .await
            .map_err(|e| (1u8, e.to_string()))?;
        Ok::<_, (u8, String)>(doc.content)
    })
    .await;
    let content = match parsed {
        Err(_elapsed) => {
            warn!(user_id, file_id = id, "document parse timed out");
            return Err(ApiError::GatewayTimeout(
                "文档解析超时，请稍后重试或改用纯文本".into(),
            ));
        }
        Ok(Err((_, detail))) => {
            // 服务端路径/解析器原文只进日志，响应给可读文案
            warn!(user_id, file_id = id, error = %detail, "document parse failed");
            return Err(ApiError::BadRequest(PARSE_FAILED_MSG.into()));
        }
        Ok(Ok(content)) => content,
    };

    let text = truncate_to_tokens(&content, DOC_TEXT_MAX_TOKENS);
    if text.trim().is_empty() {
        warn!(user_id, file_id = id, "document yielded empty text");
        return Err(ApiError::BadRequest(PARSE_FAILED_MSG.into()));
    }
    debug!(user_id, file_id = id, chars = text.len(), "document parsed");
    Ok(ContentPart::Document {
        name,
        text: text.to_string(),
    })
}

/// 按 token 预算截断到 char 边界；预算内原样返回。
///
/// 判据只走 [`estimate_str_tokens`]（全项目唯一估算实现），二分字节偏移、
/// 每次向下吸附到 char 边界 —— 估算随前缀长度单调，谓词可二分。
fn truncate_to_tokens(s: &str, max_tokens: usize) -> &str {
    if estimate_str_tokens(s) <= max_tokens {
        return s;
    }
    let snap = |mut i: usize| {
        while i > 0 && !s.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let fits = |end: usize| estimate_str_tokens(&s[..snap(end)]) <= max_tokens;

    let mut lo = 0usize; // 恒满足（空前缀估算为 0）
    let mut hi = s.len(); // 不满足（调用方已确认整串超预算）
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    &s[..snap(lo)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use axum::extract::FromRequest;

    use crate::config::ServerConfig;
    use crate::workflow::scheduler::CronScheduler;

    /// 构造测试用 AppState（临时 DB + 临时数据目录）。
    async fn test_state() -> (Arc<AppState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();

        let config = ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
            database_url: url,
            jwt_secret: "test-secret-key-do-not-use-in-production".to_string(),
            data_dir: dir.path().to_path_buf(),
        };
        let scheduler = Arc::new(CronScheduler::new().await.unwrap());
        let state = AppState::new(&config, pool, scheduler).await;
        (Arc::new(state), dir)
    }

    /// 构造 multipart 请求体（字段 `file`）并提取为 `Multipart`。
    async fn multipart_of(filename: &str, data: &[u8]) -> Multipart {
        let boundary = "----PecoTestBoundary";
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let req = axum::http::Request::builder()
            .method("POST")
            .header(
                axum::http::header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(axum::body::Body::from(body))
            .expect("build multipart request");
        Multipart::from_request(req, &())
            .await
            .expect("parse multipart")
    }

    /// 上传辅助：固定 user `u1`。
    async fn upload(
        state: &AppState,
        filename: &str,
        data: &[u8],
    ) -> Result<UploadedFile, ApiError> {
        store_uploaded_file(state, "u1", multipart_of(filename, data).await).await
    }

    /// 直接向文档目录写入引用 id 与可选 sidecar（绕过上传端点）。
    async fn put_document(state: &AppState, user_id: &str, id: &str, body: &[u8]) {
        let dir = document_dir(&state.data_dir, user_id);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join(id), body).await.unwrap();
    }

    // ── 上传分类 ─────────────────────────────────────────────────────

    /// 两个白名单的所有扩展名 → kind；文档不返回 url，图片返回。
    #[tokio::test]
    async fn upload_classifies_each_whitelisted_extension() {
        let (state, _dir) = test_state().await;

        for ext in IMAGE_EXT_WHITELIST {
            let f = upload(&state, &format!("a.{ext}"), b"\x89PNG fake")
                .await
                .unwrap_or_else(|e| panic!("{ext}: {e:?}"));
            assert_eq!(f.kind, FileKind::Image, "{ext}");
            assert!(f.url.is_some(), "{ext} 应返回静态 url");
        }
        for ext in DOC_EXT_WHITELIST {
            let f = upload(&state, &format!("a.{ext}"), b"# body")
                .await
                .unwrap_or_else(|e| panic!("{ext}: {e:?}"));
            assert_eq!(f.kind, FileKind::Document, "{ext}");
            assert!(f.url.is_none(), "{ext} 不得返回 url");
        }
    }

    /// docx → 400（提示转格式）；未知扩展名 → 400；无扩展名 → 400；空文件 → 400。
    #[tokio::test]
    async fn upload_rejects_docx_unknown_missing_ext_and_empty() {
        let (state, _dir) = test_state().await;

        let err = upload(&state, "report.docx", b"PK..").await.unwrap_err();
        match err {
            ApiError::BadRequest(m) => {
                assert!(m.contains("docx"), "{m}");
                assert!(m.contains("PDF"), "{m}");
            }
            other => panic!("docx 应 400：{other:?}"),
        }

        let err = upload(&state, "tool.exe", b"MZ").await.unwrap_err();
        assert!(
            matches!(err, ApiError::BadRequest(ref m) if m.contains("不支持的文件类型")),
            "{err:?}"
        );

        let err = upload(&state, "README", b"hi").await.unwrap_err();
        assert!(
            matches!(err, ApiError::BadRequest(ref m) if m.contains("扩展名")),
            "{err:?}"
        );

        let err = upload(&state, "empty.txt", b"").await.unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(_)), "{err:?}");
    }

    /// 图片落 chat 上传目录并返回 `/uploads/chat/...`；文档落 peco-files
    /// 并把原始文件名写进 sidecar（D4/D9）。
    #[tokio::test]
    async fn upload_stores_image_url_and_document_sidecar() {
        let (state, _dir) = test_state().await;

        let img = upload(&state, "pic.png", b"\x89PNG fake").await.unwrap();
        assert_eq!(img.kind, FileKind::Image);
        let url = img.url.as_deref().expect("image url");
        assert_eq!(url, format!("/uploads/chat/u1/{}", img.id));
        assert!(
            crate::chat::images::chat_upload_dir(&state.data_dir, "u1")
                .join(&img.id)
                .exists()
        );

        let doc = upload(&state, "报告.md", "# 标题".as_bytes())
            .await
            .unwrap();
        assert_eq!(doc.kind, FileKind::Document);
        assert_eq!(doc.name, "报告.md");
        assert!(doc.url.is_none());
        let dir = document_dir(&state.data_dir, "u1");
        assert!(dir.join(&doc.id).exists());
        // 文档不在静态目录下（/uploads 无鉴权）
        assert!(
            !crate::chat::images::chat_upload_dir(&state.data_dir, "u1")
                .join(&doc.id)
                .exists()
        );
        let sidecar = tokio::fs::read_to_string(dir.join(sidecar_name(&doc.id)))
            .await
            .unwrap();
        assert!(sidecar.contains("报告.md"), "{sidecar}");
        // 落盘名是 uuid 而非原始名
        assert_ne!(doc.id, doc.name);
    }

    // ── resolve_file_parts ───────────────────────────────────────────

    #[tokio::test]
    async fn resolve_missing_document_is_404() {
        let (state, _dir) = test_state().await;
        let err = resolve_file_parts(&state, "u1", &["missing.txt".to_string()])
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
    }

    /// 存储按用户目录隔离：引用他人目录下的文档 → 404。
    #[tokio::test]
    async fn resolve_cross_user_document_is_404() {
        let (state, _dir) = test_state().await;
        put_document(&state, "u2", "abc.txt", b"secret").await;

        let err = resolve_file_parts(&state, "u1", &["abc.txt".to_string()])
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
    }

    /// 非法 id（含 `..`）→ 400。
    #[tokio::test]
    async fn resolve_rejects_traversal_id() {
        let (state, _dir) = test_state().await;
        for id in ["../a.txt", "..", "a/../b.txt"] {
            let err = resolve_file_parts(&state, "u1", &[id.to_string()])
                .await
                .unwrap_err();
            assert!(matches!(err, ApiError::BadRequest(_)), "{id}: {err:?}");
        }
    }

    /// 非 UTF-8 / 空文件 → 400，且文案固定、不含服务端路径（D10）。
    #[tokio::test]
    async fn resolve_unparsable_document_is_400_without_paths() {
        let (state, dir) = test_state().await;
        put_document(&state, "u1", "bin.txt", &[0xff, 0xfe, 0x00, 0x41]).await;
        put_document(&state, "u1", "empty.txt", b"").await;

        for id in ["bin.txt", "empty.txt"] {
            let err = resolve_file_parts(&state, "u1", &[id.to_string()])
                .await
                .unwrap_err();
            match &err {
                ApiError::BadRequest(m) => {
                    assert_eq!(m, PARSE_FAILED_MSG, "{id}");
                    assert!(
                        !m.contains(&dir.path().display().to_string()) && !m.contains("peco-files"),
                        "错误文案不得含服务端路径：{m}"
                    );
                }
                other => panic!("{id} 应 400：{other:?}"),
            }
        }
    }

    /// 正常文档 → `Document` 部件：sidecar 原名 + 解析正文 + token 预算。
    #[tokio::test]
    async fn resolve_document_yields_named_part_within_budget() {
        let (state, _dir) = test_state().await;
        let id = "abc-123.txt";
        let long = "汉".repeat(40_000);
        put_document(&state, "u1", id, long.as_bytes()).await;
        tokio::fs::write(
            document_dir(&state.data_dir, "u1").join(sidecar_name(id)),
            r#"{"name":"笔记.txt"}"#,
        )
        .await
        .unwrap();

        let parts = resolve_file_parts(&state, "u1", &[id.to_string()])
            .await
            .unwrap();
        assert_eq!(parts.len(), 1);
        let ContentPart::Document { name, text } = &parts[0] else {
            panic!("应为 Document：{:?}", parts[0]);
        };
        assert_eq!(name, "笔记.txt");
        assert!(
            peco_core::agent::estimate_str_tokens(text) <= DOC_TEXT_MAX_TOKENS,
            "超预算"
        );
        assert!(text.starts_with('汉'));
    }

    /// sidecar 缺失时回退为引用 id（D4：不因缺 sidecar 失败）。
    #[tokio::test]
    async fn resolve_without_sidecar_falls_back_to_id() {
        let (state, _dir) = test_state().await;
        put_document(&state, "u1", "xyz.md", b"# hi").await;

        let parts = resolve_file_parts(&state, "u1", &["xyz.md".to_string()])
            .await
            .unwrap();
        let ContentPart::Document { name, text } = &parts[0] else {
            panic!("应为 Document：{:?}", parts[0]);
        };
        assert_eq!(name, "xyz.md");
        assert!(text.contains("hi"));
    }

    #[test]
    fn extension_of_lowercases_and_detects_absence() {
        assert_eq!(extension_of("a.PNG").as_deref(), Some("png"));
        assert_eq!(extension_of("archive.tar.gz").as_deref(), Some("gz"));
        assert_eq!(extension_of("noext"), None);
        assert_eq!(extension_of(".hidden"), None);
    }

    #[test]
    fn classification_covers_both_whitelists() {
        for ext in IMAGE_EXT_WHITELIST {
            assert!(IMAGE_EXT_WHITELIST.contains(ext));
        }
        for ext in DOC_EXT_WHITELIST {
            assert!(DOC_EXT_WHITELIST.contains(ext), "{ext}");
        }
        // docx 解析器未就绪：必须落在两个白名单之外
        assert!(!DOC_EXT_WHITELIST.contains(&"docx"));
        assert!(!IMAGE_EXT_WHITELIST.contains(&"docx"));
        // exe / csv 分类：csv 是文档，exe 两者皆非
        assert!(DOC_EXT_WHITELIST.contains(&"csv"));
        assert!(!DOC_EXT_WHITELIST.contains(&"exe"));
        assert!(!IMAGE_EXT_WHITELIST.contains(&"exe"));
    }

    #[test]
    fn sidecar_name_keeps_stem() {
        assert_eq!(sidecar_name("abc-123.pdf"), "abc-123.meta.json");
        assert_eq!(sidecar_name("abc-123.tar.gz"), "abc-123.tar.meta.json");
    }

    #[test]
    fn truncate_keeps_small_input_intact() {
        let s = "短文本";
        assert_eq!(truncate_to_tokens(s, DOC_TEXT_MAX_TOKENS), s);
    }

    #[test]
    fn truncate_cjk_stays_within_budget_and_char_boundary() {
        // 全 CJK：0.6 tok/字 → 超过 8192 预算的大串
        let s = "汉".repeat(40_000);
        let out = truncate_to_tokens(&s, DOC_TEXT_MAX_TOKENS);
        assert!(!out.is_empty());
        assert!(out.chars().all(|c| c == '汉'));
        assert!(estimate_str_tokens(out) <= DOC_TEXT_MAX_TOKENS);
    }

    #[test]
    fn truncate_multibyte_mixed_never_panics() {
        let mut s = String::new();
        for i in 0..20_000 {
            s.push_str(match i % 3 {
                0 => "中",
                1 => "a",
                _ => "é",
            });
        }
        let out = truncate_to_tokens(&s, DOC_TEXT_MAX_TOKENS);
        assert!(estimate_str_tokens(out) <= DOC_TEXT_MAX_TOKENS);
        // char 边界：切片必须仍可无损解析
        assert_eq!(out.len(), s[..out.len()].len());
    }

    #[test]
    fn truncate_zero_budget_yields_empty() {
        assert_eq!(truncate_to_tokens("abc", 0), "");
    }
}
