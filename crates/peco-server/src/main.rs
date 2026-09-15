// ============================================================================
// peco-server 入口 — 初始化 → 构建 Router → 启动 Axum Server
// ============================================================================

use std::net::SocketAddr;
use std::sync::Arc;

use peco_server::config::ServerConfig;
use peco_server::db;
use peco_server::state::AppState;
use peco_server::workflow::scheduler::CronScheduler;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── 1. 加载 .env ───────────────────────────────────────────────────────
    // 先于 tracing 初始化，让 .env 中的 RUST_LOG / PECO_LOG_* 配置生效。
    // dotenvy 不覆盖已存在的环境变量，后续 ServerConfig 的读取不受影响。
    dotenvy::dotenv().ok();

    // ── 2. 初始化 tracing ──────────────────────────────────────────────────
    // stdout + 按大小轮转的日志文件双写；默认过滤器与 PECO_LOG_* 环境变量
    // 见 `peco_server::logging` 模块文档。
    peco_server::logging::init_tracing();

    // ── 3. 加载初步配置（获取 database_url 和 data_dir）─────────────────
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
    let config = ServerConfig::from_env_with_db(&pool).await?;
    tracing::info!(
        host = %config.host,
        port = config.port,
        data_dir = %config.data_dir.display(),
        "Full configuration loaded (with JWT persistence)"
    );

    // ── 7. 清理僵尸 Workflow 执行记录 ───────────────────────────────────
    match db::workflow_executions::mark_zombies_failed(&pool).await {
        Ok(count) if count > 0 => tracing::info!(
            zombie_count = count,
            "Marked zombie workflow executions as failed"
        ),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "Failed to mark zombie workflow executions"),
    }

    // ── 8. 创建 CronScheduler ───────────────────────────────────────────────
    let cron_scheduler = Arc::new(
        CronScheduler::new()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create CronScheduler: {e}"))?,
    );

    // ── 9. 创建 AppState ───────────────────────────────────────────────────
    let state = Arc::new(AppState::new(&config, pool, cron_scheduler.clone()).await);

    // ── 10. 从 DB 加载已启用的 Workflow 调度并注册 ────────────────────────
    match db::workflow_schedules::list_all_enabled(&state.db).await {
        Ok(schedules) => {
            let count = schedules.len();
            for schedule in &schedules {
                match state
                    .cron_scheduler
                    .add_workflow(
                        schedule.workflow_name.clone(),
                        schedule.cron_expr.clone(),
                        schedule.timezone.clone(),
                        schedule.user_id.clone(),
                        state.db.clone(),
                        Arc::clone(&state),
                    )
                    .await
                {
                    Ok(uuid) => {
                        tracing::info!(
                            workflow = %schedule.workflow_name,
                            user_id = %schedule.user_id,
                            job_uuid = %uuid,
                            "Loaded scheduled workflow"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            workflow = %schedule.workflow_name,
                            user_id = %schedule.user_id,
                            cron = %schedule.cron_expr,
                            error = %e,
                            "Failed to register scheduled workflow on startup"
                        );
                    }
                }
            }
            tracing::info!(count = count, "Scheduled workflows loaded from database");
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load scheduled workflows from database");
        }
    }

    // ── 10b. 注册 Peco 记忆自动整理 cron（总开关关闭时为空操作）──────────
    if let Err(e) = peco_server::peco::memory::cron::register(&state).await {
        tracing::warn!(error = %e, "Failed to register memory consolidation cron job");
    }

    // ── 11. 启动调度器 ──────────────────────────────────────────────────────
    cron_scheduler
        .start()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start CronScheduler: {e}"))?;
    tracing::info!(
        job_count = cron_scheduler.job_count().await,
        "CronScheduler started"
    );

    // ── 12. 构建 Router（启用 API 限流）───────────────────────────────────
    let app = peco_server::build_router_with_limits(state, true);

    // ── 13. 绑定端口并启动 ──────────────────────────────────────────────────
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
