// ============================================================================
// AppState — 应用全局共享状态
// ============================================================================

use std::path::PathBuf;
use std::sync::Arc;

use peco_core::config::SystemConfig;
use sqlx::SqlitePool;

use crate::config::ServerConfig;
use crate::workflow::persister::SqliteWorkflowPersister;
use crate::workflow::scheduler::CronScheduler;
use crate::workspace::WorkspaceManager;

/// 应用全局状态，所有 handler 通过 `State<Arc<AppState>>` 访问。
pub struct AppState {
    /// SQLite 连接池。
    pub db: SqlitePool,
    /// JWT 签名密钥。
    pub jwt_secret: String,
    /// 数据存储根目录。
    pub data_dir: PathBuf,
    /// 工作空间管理器。
    pub workspace_manager: Arc<WorkspaceManager>,

    // ── Workflow 子系统 ──────────────────────────────────────────────
    /// 定时调度器。
    pub cron_scheduler: Arc<CronScheduler>,
}

impl AppState {
    /// 创建 AppState 并确保数据目录存在。
    pub async fn new(
        config: &ServerConfig,
        db: SqlitePool,
        cron_scheduler: Arc<CronScheduler>,
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
            cron_scheduler,
        }
    }

    /// 为指定用户创建 workflow 持久化实例（per-user pattern）。
    pub fn workflow_persister_for(&self, user_id: &str) -> SqliteWorkflowPersister {
        SqliteWorkflowPersister::new(self.db.clone(), user_id.to_string())
    }
}
