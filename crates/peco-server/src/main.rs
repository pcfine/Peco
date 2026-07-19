// ============================================================================
// peco-server 入口 — 初始化 → 构建 Router → 启动 Axum Server
// ============================================================================

use std::net::SocketAddr;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;

use peco_server::config::ServerConfig;
use peco_server::db;
use peco_server::state::AppState;
use peco_server::task::CronScheduler;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── 1. 初始化 tracing ──────────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                EnvFilter::new("peco_server=info,tower_http=info")
            }),
        )
        .init();

    // ── 2. 加载 .env ───────────────────────────────────────────────────────
    dotenvy::dotenv().ok();

    // ── 3. 加载初步配置（获取 database_url 和 data_dir）─────────────────
    //    此时还不依赖 DB，JWT 先用环境变量或随机值
    let config_prelim = ServerConfig::from_env()?;
    tracing::info!(
        host = %config_prelim.host,
        port = config_prelim.port,
        data_dir = %config_prelim.data_dir.display(),
        "Preliminary configuration loaded"
    );

    // ── 4. 确保数据目录存在 ──────────────────────────────────────────────
    tokio::fs::create_dir_all(&config_prelim.data_dir).await?;
    tokio::fs::create_dir_all(config_prelim.data_dir.join("sessions")).await?;

    // ── 5. 创建 SQLite 连接池 + 运行迁移 ──────────────────────────────────
    let pool = db::connect(&config_prelim.database_url).await?;
    db::run_migrations(&pool).await?;

    // ── 6. 重新加载完整配置（含 DB 持久化的 JWT 密钥）───────────────────
    //    DB 已就绪，JWT 密钥支持三层降级：环境变量 → DB → 随机生成+持久化
    let config = ServerConfig::from_env_with_db(&pool).await?;
    tracing::info!(
        host = %config.host,
        port = config.port,
        data_dir = %config.data_dir.display(),
        "Full configuration loaded (with JWT persistence)"
    );

    // ── 7. 创建 CronScheduler ───────────────────────────────────────────────
    let cron_scheduler = Arc::new(
        CronScheduler::new()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create CronScheduler: {e}"))?,
    );

    // ── 8. 创建 AppState ───────────────────────────────────────────────────
    let state = Arc::new(AppState::new(&config, pool, cron_scheduler.clone()).await);

    // ── 9. 从 DB 加载已启用的 Task 并注册到调度器 ─────────────────────────
    match db::tasks::list_all_enabled(&state.db).await {
        Ok(enabled_tasks) => {
            let count = enabled_tasks.len();
            for task in &enabled_tasks {
                match state
                    .task_scheduler
                    .add_task(
                        task.id.clone(),
                        task.cron_expr.clone(),
                        task.agent_id.clone(),
                        task.user_id.clone(),
                        task.prompt.clone(),
                        state.db.clone(),
                        Arc::clone(&state),
                    )
                    .await
                {
                    Ok(uuid) => {
                        tracing::info!(
                            task_id = %task.id,
                            task_name = %task.name,
                            job_uuid = %uuid,
                            "Loaded scheduled task"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            task_id = %task.id,
                            task_name = %task.name,
                            error = %e,
                            "Failed to register scheduled task on startup"
                        );
                    }
                }
            }
            tracing::info!(count = count, "Scheduled tasks loaded from database");
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load scheduled tasks from database");
        }
    }

    // ── 10. 启动调度器 ──────────────────────────────────────────────────────
    cron_scheduler
        .start()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start CronScheduler: {e}"))?;
    tracing::info!(job_count = cron_scheduler.job_count().await, "CronScheduler started");

    // ── 11. 构建 Router（启用 API 限流）───────────────────────────────────
    let app = peco_server::build_router_with_limits(state, true);

    // ── 12. 绑定端口并启动 ──────────────────────────────────────────────────
    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    tracing::info!("Server starting on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(cron_scheduler))
        .await?;

    Ok(())
}

/// 优雅关闭：监听 SIGTERM / SIGINT (Ctrl+C)，收到信号后关闭调度器。
async fn shutdown_signal(cron_scheduler: Arc<CronScheduler>) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("Shutdown signal received, gracefully shutting down...");

    // 1. 关闭调度器（停止所有定时任务）
    if let Err(e) = cron_scheduler.shutdown().await {
        tracing::error!(error = %e, "Failed to shut down CronScheduler");
    } else {
        tracing::info!("CronScheduler shut down");
    }
    // 2. DB 连接池在 drop 时自动关闭
}
