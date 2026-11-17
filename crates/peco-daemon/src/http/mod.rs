//! HTTP 层：把请求翻译成领域调用，再把结果翻译成响应。
//!
//! 这一层不碰 marker、不碰实例表状态机 —— 那些判断都在 `space` 与
//! `WorkspaceHub` 里，HTTP 只负责路由、请求体与错误体形状。

pub mod brand;
pub mod events;
pub mod peco;
pub mod space;
pub mod space_artifacts;
pub mod space_conversation;
pub mod space_files;
pub mod space_memory;
pub mod space_overview;
pub mod static_files;
pub mod tasks;
pub mod team;
pub mod upload;

use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde::Serialize;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
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
    let uploads_dir = state.config.peco_home.join("uploads");
    let router = Router::new()
        .route("/api/spaces", get(space::list).post(space::create))
        .route(
            "/api/spaces/{ws_id}",
            get(space::detail).patch(space::update).delete(space::purge),
        )
        .route("/api/spaces/{ws_id}/close", post(space::close))
        .route("/api/spaces/{ws_id}/open", post(space::open))
        .route("/api/spaces/{ws_id}/tasks", get(tasks::list_for_space))
        // 实时观察只读该空间的 TaskHub 内存：未打开时 409，不拿空列表冒充。
        // `live` 是静态段，与 `{task_id}` 子树不重叠（前者两段，后者三段）。
        .route("/api/spaces/{ws_id}/tasks/live", get(tasks::live))
        .route(
            "/api/spaces/{ws_id}/tasks/{task_id}/stream",
            get(tasks::stream),
        )
        .route(
            "/api/spaces/{ws_id}/tasks/{task_id}/cancel",
            post(tasks::cancel),
        )
        // 空间维度的只读视图：路径里已带 `ws_id`，不要求 `X-Peco-Space` 头。
        .route("/api/spaces/{ws_id}/artifacts", get(space_artifacts::list))
        // 空间单会话的惰性创建：前端点开空间时才调用，幂等。
        .route(
            "/api/spaces/{ws_id}/conversation/ensure",
            post(space_conversation::ensure),
        )
        // 清空这条会话的消息；会话行与其产物留着，重试也是同样的结果。
        .route(
            "/api/spaces/{ws_id}/conversation/clear",
            post(space_conversation::clear),
        )
        .route("/api/spaces/{ws_id}/files", get(space_files::browse))
        // 记忆浏览：`documents` 是静态段，与 `{doc_id}` 子树不重叠（前者四段、后者五段）。
        .route(
            "/api/spaces/{ws_id}/memory/documents",
            get(space_memory::list),
        )
        .route(
            "/api/spaces/{ws_id}/memory/documents/{doc_id}",
            get(space_memory::detail),
        )
        .route(
            "/api/spaces/{ws_id}/overview",
            get(space_overview::overview),
        )
        .route("/api/tasks", get(tasks::list))
        .route("/api/tasks/{task_id}", get(tasks::detail))
        .route("/api/teams", get(team::list).post(team::create))
        .route(
            "/api/teams/{team_id}",
            get(team::detail).patch(team::update).delete(team::delete),
        )
        .route(
            "/api/upload",
            post(upload::upload_image).layer(DefaultBodyLimit::max(upload::MAX_UPLOAD_BODY_BYTES)),
        )
        // 系统层 provider 配置：不带 team，与团队路由并存（同领域函数，两个入口）。
        // `/types` 与 `/test` 是静态段，优先于 `{name}`（axum 0.8 的路由优先级，
        // 与既有 `/teams/{id}/providers/test` 同一模式）。没有 by-name GET：
        // 零调用方，团队路由那条保留不变。
        .route(
            "/api/providers",
            get(provider::list_system).put(provider::upsert_system),
        )
        .route("/api/providers/types", get(provider::types_system))
        .route("/api/providers/test", post(provider::test_draft_system))
        .route("/api/providers/{name}", delete(provider::delete_system))
        .route(
            "/api/providers/{name}/test",
            post(provider::test_connection_system),
        )
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
        // 左栏品牌图：全局一张，不分空间也不分团队。
        .route("/api/brand", get(brand::get).put(brand::update))
        // 台账事件流：看板靠它把「任务变状态」推进 UI，不轮询。
        .route("/api/events", get(events::stream))
        // 资源模块与 chat 沿用 peco-server 的路径：前端与既有 API 文档不需要改。
        // chat 与 artifacts 要求 `LocalUser`，空间选择器（X-Peco-Space / ?ws=）
        // 因此对这两条子树是强制项；四个资源模块认 `ResourceScope`，空间与团队
        // 选择器都没有才 400。
        .nest("/api/agents", agent::router())
        .nest("/api/skills", skill::router())
        .nest("/api/mcp", mcp_config::router())
        .nest("/api/knowledge", knowledge::router())
        .nest("/api/chat", chat::router())
        .nest("/api/artifacts", artifact::router())
        .nest_service("/uploads", ServeDir::new(uploads_dir));

    // 前端产物是最后一道兜底：`/api` 与 `/uploads` 的未命中仍由各自的 404 语义
    // 决定，静态层缺席时行为与没有这一层时完全一致。
    let router = match static_files::service_from_env() {
        Some(spa) => router.fallback_service(spa),
        None => router,
    };

    // 两层跨领域中间件必须罩住 fallback：先挂 fallback 再 `.layer()`，静态响应
    // 才和 API 响应带同样的 CORS 头，日志里也不会只出现接口请求。
    router
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

/// 监听 SIGINT 与 SIGTERM，任一个都落到同一条优雅退出路径。
///
/// `systemctl stop` 发的是 SIGTERM：只等 SIGINT 的话它会被默认处置直接终止，
/// 在飞轮次断、sqlite 不 close、`daemon.pid` 残留（清理只在 Drop 里做一次）。
/// 信号注册失败时只留 journal 证据并保持 pending —— 装不上信号处理器不该让
/// 服务起不来，该让运维在日志里看见。
async fn shutdown_signal() {
    let interrupt = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {}
            Err(e) => {
                tracing::error!(error = %e, "cannot listen for SIGINT");
                std::future::pending::<()>().await
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    let signal = tokio::select! {
        _ = interrupt => "int",
        _ = terminate => "term",
    };
    tracing::info!(signal, "shutdown signal received; draining");
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
