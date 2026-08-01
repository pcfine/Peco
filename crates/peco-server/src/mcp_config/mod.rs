// MCP 配置管理 API

mod handler;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(handler::get_config).put(handler::update_config))
        .route("/{name}/test", post(handler::test_connection))
}
