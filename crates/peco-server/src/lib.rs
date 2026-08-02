// ============================================================================
// peco-server — Axum Web 后端
// ============================================================================

#![recursion_limit = "256"]

use std::sync::Arc;

use axum::Router;
use axum::routing::post;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

pub mod agent;
pub mod assistant;
pub mod auth;
pub mod chat;
pub mod config;
pub mod db;
pub mod error;
pub mod file_watcher;
pub mod knowledge;
pub mod mcp_config;
pub mod middleware;
pub mod openapi;
pub mod peco;
pub mod personal_agent;
pub mod personal_assistant;
pub mod provider;
pub mod session_store;
pub mod skill;
pub mod state;
pub mod task;
pub mod upload;
pub mod usage;
pub mod workspace;

/// 构建完整的 Axum Router（不含限流层，供集成测试使用）。
///
/// 供 `main.rs` 和集成测试复用。
pub fn build_router(state: Arc<state::AppState>) -> Router {
    build_router_with_limits(state, false)
}

/// 构建完整的 Axum Router，可选择是否启用限流。
///
/// * `enable_rate_limit` — `true` 时添加 per-user 速率限制。
pub fn build_router_with_limits(state: Arc<state::AppState>, enable_rate_limit: bool) -> Router {
    let secret = state.jwt_secret.clone();

    // 静态文件服务：上传的图片等资源
    let uploads_dir = state.data_dir.join("uploads");

    // 受保护的路由组（需要认证 + 限流）
    let protected_routes = Router::new()
        .route("/api/upload", post(upload::upload))
        .nest("/api/peco", peco::handler::router())
        .nest("/api/chat", chat::router())
        .nest("/api/providers", provider::router())
        .nest("/api/agents", agent::router())
        .nest("/api/skills", skill::router())
        .nest("/api/mcp", mcp_config::router())
        .nest("/api/knowledge", knowledge::router())
        .nest("/api/tasks", task::router())
        .nest("/api/usage", usage::router())
        // DEPRECATED since 0.2.0: use /api/chat/:agentId/conversations instead
        .nest("/api/conversations", chat::conversation_router());

    let protected_routes = if enable_rate_limit {
        protected_routes.layer(middleware::rate_limit::rate_limit_layer(&secret))
    } else {
        protected_routes
    };

    Router::new()
        .merge(SwaggerUi::new("/docs").url("/api-docs/openapi.json", openapi::ApiDoc::openapi()))
        .nest("/api/auth", auth::router()) // 公开路由，不限流
        .nest_service("/uploads", ServeDir::new(uploads_dir))
        .merge(protected_routes)
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}
