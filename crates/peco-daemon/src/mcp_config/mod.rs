//! MCP 配置：路由与处理函数。
//!
//! 配置存在工作空间的 `mcpconfig.json`，写路径统一走
//! `WorkSpace::replace_mcp_config`（写盘与内存切换在同一次锁内完成）。

mod handler;

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

/// # 路由
/// - `GET /` — 读取配置
/// - `PUT /` — 整体替换配置
/// - `POST /{name}/test` — 试连一个 server
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(handler::get_config).put(handler::update_config))
        .route("/{name}/test", post(handler::test_connection))
}
