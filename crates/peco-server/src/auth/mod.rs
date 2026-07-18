// ============================================================================
// Auth 模块 — 路由、中间件、JWT 工具
// ============================================================================

mod handler;
pub mod jwt;
mod middleware;

pub use middleware::AuthUser;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use crate::state::AppState;

/// 返回认证相关路由（公开，无需 JWT）。
///
/// # 路由
/// - `POST /register` — 注册新用户
/// - `POST /login` — 登录
/// - `GET /me` — 获取当前用户（需 JWT）
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/register", post(handler::register))
        .route("/login", post(handler::login))
        .route("/me", get(handler::me))
}
