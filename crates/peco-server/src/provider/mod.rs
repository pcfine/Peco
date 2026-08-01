// Provider 管理 API — 用户 workspace 级别 providers.toml CRUD

mod handler;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(handler::list).put(handler::upsert))
        .route("/{name}", get(handler::get).delete(handler::delete))
        .route("/{name}/test", post(handler::test_connection))
}
