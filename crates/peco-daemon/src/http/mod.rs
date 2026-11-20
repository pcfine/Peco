//! HTTP 层：把请求翻译成领域调用，再把结果翻译成响应。
//!
//! 这一层不碰 marker、不碰实例表状态机 —— 那些判断都在 `space` 与
//! `WorkspaceHub` 里，HTTP 只负责路由、请求体与错误体形状。

pub mod auth;
pub mod brand;
pub mod events;
pub mod peco;
pub mod schedules;
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

use std::net::SocketAddr;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde::Serialize;
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

/// 停机的连接宽限：到点仍未结束的连接被强制断开，停机不会无限期挂着。
///
/// 两条监听路径共用这一个数：拆成两个的话，一处调整会让另一处的上界悄悄漂移。
const CONN_GRACE: Duration = Duration::from_secs(2);

pub fn router(state: AppState) -> Router {
    let uploads_dir = state.config.peco_home.join("uploads");
    let router = Router::new()
        .route("/api/auth/status", get(auth::status))
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/password", post(auth::change_password))
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
        // 定时任务：列表类接口一律裸数组（与 `/api/spaces` 一致）。
        // `preview` 是静态段，与 `{id}` 子树不重叠（前者两段、后者三段）。
        .route(
            "/api/schedules",
            get(schedules::list).post(schedules::create),
        )
        .route("/api/schedules/preview", post(schedules::preview))
        .route(
            "/api/schedules/{id}",
            axum::routing::patch(schedules::update).delete(schedules::delete),
        )
        .route("/api/schedules/{id}/run", post(schedules::run))
        .route("/api/schedules/{id}/runs", get(schedules::runs))
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

    // 日志层在闸门外面：被闸门挡下的请求同样留痕。两层都挂在 fallback 之后才
    // 罩得住它。
    router
        .layer(from_fn_with_state(state.clone(), auth::require_auth))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// 监听并阻塞到优雅退出信号。
///
/// 返回时连接要么已全部自行结束，要么已按 [`CONN_GRACE`] 断掉 —— 停机序列据此
/// 排在它之后，不必再关心还有没有连接挂着。
///
/// 证书与私钥都配了才走 HTTPS；缺任一维持明文。
pub async fn serve(state: AppState) -> std::io::Result<()> {
    let bind_addr = state.config.bind_addr;
    // 先取出配置与闩：下面要把 `state` 移进路由，借着自己的字段移不过去。
    let config = state.config.clone();
    let shutdown = state.shutdown.clone();
    let service = router(state).into_make_service_with_connect_info::<SocketAddr>();

    match config.tls() {
        Some((cert, key)) => {
            let tls = tls_config(cert, key).await?;
            tracing::info!(
                addr = %bind_addr,
                cert = %cert.display(),
                "https listening"
            );
            let handle = axum_server::Handle::new();
            let serving = axum_server::bind_rustls(bind_addr, tls)
                .handle(handle.clone())
                .serve(service);
            serve_with_grace(handle, shutdown, serving).await
        }
        None => {
            tracing::info!(addr = %bind_addr, "http listening");
            let handle = axum_server::Handle::new();
            let serving = axum_server::bind(bind_addr)
                .handle(handle.clone())
                .serve(service);
            serve_with_grace(handle, shutdown, serving).await
        }
    }
}

/// 装 ring provider 并读证书。
///
/// `tls-rustls-no-provider` 把进程级 provider 的选择权留在这里：默认那条会拉
/// aws-lc-rs，构建链需要 cmake/nasm。装过一次之后再装是 no-op，重复调用无害。
async fn tls_config(
    cert: &std::path::Path,
    key: &std::path::Path,
) -> std::io::Result<axum_server::tls_rustls::RustlsConfig> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
        .await
        .map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "cannot load TLS material from '{}' and '{}': {e}",
                    cert.display(),
                    key.display()
                ),
            )
        })
}

/// 两条监听形态共用的停机序列：信号到达先置停机闩（各条永久 SSE 据此自行收尾），
/// 再让监听器停止接客，然后按 [`CONN_GRACE`] 断掉剩下的连接 —— 于是本函数返回的
/// 时刻与客户端配不配合无关。
async fn serve_with_grace<F>(
    handle: axum_server::Handle,
    shutdown: crate::shutdown::Shutdown,
    serving: F,
) -> std::io::Result<()>
where
    F: std::future::Future<Output = std::io::Result<()>>,
{
    let graceful = handle.clone();
    tokio::spawn(async move {
        shutdown_signal(shutdown).await;
        // 宽限只兜「客户端不理会关闭」：正常路径上各条流自己收尾，远快于它。
        graceful.graceful_shutdown(Some(CONN_GRACE));
    });

    serving.await
}

/// 监听 SIGINT 与 SIGTERM，任一个都落到同一条优雅退出路径：只置停机闩，收尾排在
/// `serve` 返回之后（见 [`crate::shutdown::drain`]），顺序由调用关系保证。
///
/// `systemctl stop` 发的是 SIGTERM：只等 SIGINT 的话它会被默认处置直接终止，
/// 在飞轮次断、sqlite 不 close、`daemon.pid` 残留（清理只在 Drop 里做一次）。
/// 信号注册失败时只留 journal 证据并保持 pending —— 装不上信号处理器不该让
/// 服务起不来，该让运维在日志里看见。
async fn shutdown_signal(shutdown: crate::shutdown::Shutdown) {
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
    shutdown.cancel();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 证书坏了要以 `io::Error` 带路径报出来：TLS 素材是部署输入，
    /// 不能在启动期 panic 成一段 backtrace，也不该静默降级成明文。
    #[tokio::test]
    async fn broken_tls_material_is_an_io_error_with_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");

        let err = tls_config(&cert, &key).await.unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("cert.pem"),
            "错误要指出是哪个文件：{message}"
        );
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);

        std::fs::write(&cert, "not a certificate").unwrap();
        std::fs::write(&key, "not a key").unwrap();
        let err = tls_config(&cert, &key).await.unwrap_err();
        assert!(
            err.to_string().contains("cert.pem") && err.to_string().contains("key.pem"),
            "两个文件都要出现在错误里：{err}"
        );
    }
}
