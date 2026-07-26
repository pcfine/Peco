// ============================================================================
// peco-server — Axum Web 后端
// ============================================================================

use std::sync::Arc;

use axum::Router;
use tower_http::cors::CorsLayer;
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
pub mod knowledge;
pub mod middleware;
pub mod openapi;
pub mod personal_agent;
pub mod personal_assistant;
pub mod session_store;
pub mod state;
pub mod task;
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

    // 受保护的路由组（需要认证 + 限流）
    let protected_routes = Router::new()
        .nest("/api/agents", agent::router())
        .nest("/api/conversations", chat::conversation_router())
        .nest("/api/knowledge", knowledge::router())
        .nest("/api/tasks", task::router())
        .nest("/api/personal-agent", personal_agent::handler::router());

    let protected_routes = if enable_rate_limit {
        protected_routes.layer(middleware::rate_limit::rate_limit_layer(&secret))
    } else {
        protected_routes
    };

    Router::new()
        .merge(SwaggerUi::new("/docs").url("/api-docs/openapi.json", openapi::ApiDoc::openapi()))
        .nest("/api/auth", auth::router()) // 公开路由，不限流
        .merge(protected_routes)
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}
