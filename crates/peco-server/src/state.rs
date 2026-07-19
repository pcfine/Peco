// ============================================================================
// AppState — 应用全局共享状态
// ============================================================================

use std::path::PathBuf;
use std::sync::Arc;

use peco_core::config::SystemConfig;
use sqlx::SqlitePool;

use crate::config::ServerConfig;
use crate::task::CronScheduler;
use crate::workspace::WorkspaceManager;

/// 应用全局状态，所有 handler 通过 `State<Arc<AppState>>` 访问。
pub struct AppState {
    /// SQLite 连接池。
    pub db: SqlitePool,
    /// JWT 签名密钥。
    pub jwt_secret: String,
    /// 数据存储根目录。
    pub data_dir: PathBuf,
    /// 工作空间管理器（替代 agent_registry + web_knowledge_manager）。
    pub workspace_manager: Arc<WorkspaceManager>,
    /// 定时任务调度器。
    pub task_scheduler: Arc<CronScheduler>,
}

impl AppState {
    /// 创建 AppState 并确保数据目录存在。
    pub async fn new(
        config: &ServerConfig,
        db: SqlitePool,
        task_scheduler: Arc<CronScheduler>,
    ) -> Self {
        if let Err(e) = tokio::fs::create_dir_all(&config.data_dir).await {
            tracing::warn!(
                error = %e,
                data_dir = %config.data_dir.display(),
                "Failed to create data directory"
            );
        }

        // 确保 sessions 子目录存在
        let sessions_dir = config.data_dir.join("sessions");
        if let Err(e) = tokio::fs::create_dir_all(&sessions_dir).await {
            tracing::warn!(error = %e, dir = %sessions_dir.display(), "Failed to create sessions directory");
        }

        // 确保 workspaces 子目录存在
        let workspaces_dir = config.data_dir.join("workspaces");
        if let Err(e) = tokio::fs::create_dir_all(&workspaces_dir).await {
            tracing::warn!(error = %e, dir = %workspaces_dir.display(), "Failed to create workspaces directory");
        }

        // 确保 knowledge 子目录存在
        let knowledge_dir = config.data_dir.join("knowledge");
        if let Err(e) = tokio::fs::create_dir_all(&knowledge_dir).await {
            tracing::warn!(error = %e, dir = %knowledge_dir.display(), "Failed to create knowledge directory");
        }

        // 加载系统级配置
        let system_config = Arc::new(SystemConfig::load());

        // 创建 WorkspaceManager（LRU 容量 128）
        let workspace_manager = Arc::new(WorkspaceManager::new(
            config.data_dir.clone(),
            system_config,
            128,
        ));

        Self {
            db,
            jwt_secret: config.jwt_secret.clone(),
            data_dir: config.data_dir.clone(),
            workspace_manager,
            task_scheduler,
        }
    }
}
