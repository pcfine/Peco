//! 知识库：路由、处理函数与 `documents` 表投影。
//!
//! 隔离维度是**团队**（`kb_scope` = team_id）而不是空间：同团队的空间共用一套
//! KB 命名空间，`documents` 行的归属由 `kb_id` 决定，`user_id` 恒为
//! [`crate::LOCAL_USER_ID`]。

pub mod handler;
pub mod reconcile;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post};

use crate::state::AppState;

/// 体积换算基数：错误文案里的 MB 一律按 MiB 计。
pub(crate) const BYTES_PER_MB: usize = 1024 * 1024;

/// 单个文档的大小上限：50 MB。
pub const MAX_DOC_BYTES: usize = 50 * BYTES_PER_MB;

/// 上传请求体上限：文档本身 [`MAX_DOC_BYTES`]，另留 1 MB 给 multipart 的
/// boundary、字段头与结尾分隔符。
///
/// 两个数不能并成一个：axum 在提取 multipart 时就按**请求体字节数**截断，
/// body 限额若等于文件限额，一个正好 50 MB 的文件会先被编码开销顶掉，
/// 永远走不到 handler 里按真实文件字节数判定的那道检查。
pub const MAX_UPLOAD_BODY_BYTES: usize = MAX_DOC_BYTES + BYTES_PER_MB;

/// 把「body 限额必须更大」钉在编译期：并成一个数就先把构建打红，
/// 而不是等用户传 50 MB 文件时才失败。
const _: () = assert!(MAX_UPLOAD_BODY_BYTES > MAX_DOC_BYTES);

/// # 路由
/// - `GET /` — 列出知识库
/// - `POST /` — 创建知识库
/// - `GET /{id}` — 详情
/// - `DELETE /{id}` — 删除
/// - `GET /{id}/documents` — 文档列表
/// - `POST /{id}/upload` — 上传文件
/// - `POST /{id}/sync` — 触发同步
/// - `DELETE /{id}/documents/{doc_id}` — 删除文档
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/",
            get(handler::list_knowledge_bases).post(handler::create_knowledge_base),
        )
        .route(
            "/{id}",
            get(handler::get_knowledge_base).delete(handler::delete_knowledge_base),
        )
        .route("/{id}/documents", get(handler::list_documents))
        // 只给这条路由抬高请求体限额：axum multipart 默认 2 MB，会在提取阶段
        // 截断大文件；其余知识库端点的请求体都是小 JSON，保持默认即可。
        .route(
            "/{id}/upload",
            post(handler::upload_document).layer(DefaultBodyLimit::max(MAX_UPLOAD_BODY_BYTES)),
        )
        .route("/{id}/sync", post(handler::sync_knowledge_base))
        .route("/{id}/documents/{doc_id}", delete(handler::delete_document))
}
