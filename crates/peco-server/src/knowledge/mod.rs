// ============================================================================
// knowledge — 知识库模块
// ============================================================================
//
// 提供知识库 CRUD、文件上传、文档管理、增量同步的 HTTP API，
// 以及用户隔离的 Web 知识工具（用于 Agent 调用）。
//
// 路由：
//   GET    /api/knowledge                        — 列出知识库
//   POST   /api/knowledge                        — 创建知识库
//   GET    /api/knowledge/:id                    — 知识库详情
//   DELETE /api/knowledge/:id                    — 删除知识库
//   GET    /api/knowledge/:id/documents          — 文档列表
//   POST   /api/knowledge/:id/upload             — 上传文件
//   POST   /api/knowledge/:id/sync               — 同步知识库
//   DELETE /api/knowledge/:id/documents/:doc_id  — 删除文档

pub mod handler;

use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, get, post};

use crate::state::AppState;

/// 构建知识库路由。
pub fn router() -> Router<Arc<AppState>> {
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
