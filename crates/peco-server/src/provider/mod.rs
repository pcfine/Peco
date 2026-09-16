// Provider 管理 API — 用户 workspace 级别 providers.toml CRUD

mod handler;
mod probe;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(handler::list).put(handler::upsert))
        // 静态路径必须先于 `/{name}` 注册，否则会被路径参数吞掉
        .route("/types", get(handler::list_types))
        .route("/test", post(handler::test_draft))
        .route("/{name}", get(handler::get).delete(handler::delete))
        .route("/{name}/test", post(handler::test_connection))
}
