//! Agent 管理：路由与处理函数。
//!
//! Agent 是「按空间（实为团队能力根）」的资源：配置文件在
//! `agents/<name>/agent.md`，DB 只有轻量索引；索引的 `user_id` 恒为
//! [`crate::LOCAL_USER_ID`]，空间归属只体现在本模块要求 `LocalUser`。

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
