//! HTTP 层：把请求翻译成领域调用，再把结果翻译成响应。
//!
//! 这一层不碰 marker、不碰实例表状态机 —— 那些判断都在 `space` 与
//! `WorkspaceHub` 里，HTTP 只负责路由、请求体与错误体形状。

pub mod events;
pub mod peco;
pub mod space;
pub mod space_artifacts;
pub mod space_files;
pub mod space_overview;
pub mod tasks;
pub mod team;

use axum::Json;
use axum::Router;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Serialize;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::agent;
use crate::artifact;
use crate::chat;
use crate::error::DaemonError;
use crate::knowledge;
use crate::mcp_config;
use crate::provider;
use crate::skill;
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/spaces", get(space::list).post(space::create))
        .route(
            "/api/spaces/{ws_id}",
            get(space::detail).patch(space::update).delete(space::purge),
        )
        .route("/api/spaces/{ws_id}/close", post(space::close))
        .route("/api/spaces/{ws_id}/tasks", get(tasks::list_for_space))
        // 空间维度的只读视图：路径里已带 `ws_id`，不要求 `X-Peco-Space` 头。
        .route("/api/spaces/{ws_id}/artifacts", get(space_artifacts::list))
        .route("/api/spaces/{ws_id}/files", get(space_files::browse))
        .route(
            "/api/spaces/{ws_id}/overview",
            get(space_overview::overview),
        )
        .route("/api/tasks", get(tasks::list))
        .route("/api/tasks/{task_id}", get(tasks::detail))
        .route("/api/teams", get(team::list).post(team::create))
        .route("/api/teams/{team_id}", get(team::detail))
        // 团队级 provider 配置：providers.toml 属于团队能力根，不在某个空间里。
        .route(
            "/api/teams/{team_id}/providers",
            get(provider::list).put(provider::upsert),
        )
        .route(
            "/api/teams/{team_id}/providers/types",
            get(provider::list_types),
        )
        .route(
            "/api/teams/{team_id}/providers/test",
            post(provider::test_draft),
        )
        .route(
            "/api/teams/{team_id}/providers/{name}",
            get(provider::get).delete(provider::delete),
        )
        .route(
            "/api/teams/{team_id}/providers/{name}/test",
            post(provider::test_connection),
        )
        .route("/api/peco", get(peco::detail))
        // 台账事件流：看板靠它把「任务变状态」推进 UI，不轮询。
        .route("/api/events", get(events::stream))
        // 资源模块与 chat 沿用 peco-server 的路径：前端与既有 API 文档不需要改。
        // 每个 handler 都要求 `LocalUser`，空间选择器（X-Peco-Space / ?ws=）
        // 因此对这几条子树是强制项。
        .nest("/api/agents", agent::router())
        .nest("/api/skills", skill::router())
        .nest("/api/mcp", mcp_config::router())
        .nest("/api/knowledge", knowledge::router())
        .nest("/api/chat", chat::router())
        .nest("/api/artifacts", artifact::router())
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// 监听并阻塞到优雅退出信号。
pub async fn serve(state: AppState) -> std::io::Result<()> {
    let bind_addr = state.config.bind_addr;
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!(addr = %bind_addr, "http listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
}

async fn shutdown_signal() {
    match tokio::signal::ctrl_c().await {
        Ok(()) => tracing::info!("shutdown signal received; draining"),
        Err(e) => tracing::error!(error = %e, "cannot listen for shutdown signal"),
    }
}

/// 错误响应体 `{code, message}` 的唯一形状定义。
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
    /// 只有 `inflight_tasks` 带这两个计数；不序列化空值，避免给别的错误发明字段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inflight_dispatch: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inflight_foreign: Option<i64>,
}

impl IntoResponse for DaemonError {
    fn into_response(self) -> Response {
        let status = self.http_status();
        let (inflight_dispatch, inflight_foreign) = match &self {
            DaemonError::InflightTasks {
                inflight_dispatch,
                inflight_foreign,
                ..
            } => (Some(*inflight_dispatch), Some(*inflight_foreign)),
            _ => (None, None),
        };
        let body = ErrorBody {
            code: self.code(),
            message: self.to_string(),
            inflight_dispatch,
            inflight_foreign,
        };
        if status.is_server_error() {
            tracing::error!(code = body.code, error = %body.message, "request failed");
        }
        (status, Json(body)).into_response()
    }
}
