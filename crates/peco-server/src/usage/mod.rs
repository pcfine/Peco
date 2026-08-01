// Token 用量统计 API

mod handler;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/summary", get(handler::get_summary))
}
