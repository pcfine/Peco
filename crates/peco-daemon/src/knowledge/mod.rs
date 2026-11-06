//! 知识库：路由、处理函数与 `documents` 表投影。
//!
//! 隔离维度是**团队**（`kb_scope` = team_id）而不是空间：同团队的空间共用一套
//! KB 命名空间，`documents` 行的归属由 `kb_id` 决定，`user_id` 恒为
//! [`crate::LOCAL_USER_ID`]。

pub mod handler;
pub mod reconcile;

use axum::Router;
use axum::routing::{delete, get, post};

use crate::state::AppState;

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
        .route("/{id}/upload", post(handler::upload_document))
        .route("/{id}/sync", post(handler::sync_knowledge_base))
        .route("/{id}/documents/{doc_id}", delete(handler::delete_document))
}
