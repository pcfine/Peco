// Skill 管理 API

mod handler;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(handler::list))
        .route("/import", post(handler::import_skill))
        .route(
            "/{name}",
            get(handler::get)
                .put(handler::upsert)
                .delete(handler::delete_skill),
        )
        .route("/{name}/export", get(handler::export_skill))
}
