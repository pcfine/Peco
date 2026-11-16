//! Agent 管理：路由与处理函数。
//!
//! Agent 是「按团队能力根」的资源：配置文件在 `agents/<name>/agent.md`，DB 只有
//! 轻量索引；索引的 `user_id` 恒为 [`crate::LOCAL_USER_ID`]，团队归属由
//! [`crate::auth::ResourceScope`] 给出。

mod handler;

use axum::Router;
use axum::routing::get;

use crate::state::AppState;

/// # 路由
/// - `GET /` — Agent 列表
/// - `POST /` — 创建 Agent
/// - `GET /{id}` — Agent 详情
/// - `PATCH /{id}` — 更新 Agent
/// - `DELETE /{id}` — 删除 Agent
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(handler::list).post(handler::create))
        .route(
            "/{id}",
            get(handler::get)
                .patch(handler::update)
                .delete(handler::delete),
        )
}
