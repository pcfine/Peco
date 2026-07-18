// ============================================================================
// AppState — 应用全局共享状态
// ============================================================================

use std::path::PathBuf;
use std::sync::Arc;

use sqlx::SqlitePool;

use crate::agent::AgentRegistry;
use crate::config::ServerConfig;
use crate::knowledge::manager::WebKnowledgeManager;
use crate::task::CronScheduler;

/// 应用全局状态，所有 handler 通过 `State<Arc<AppState>>` 访问。
pub struct AppState {
    /// SQLite 连接池。
    pub db: SqlitePool,
    /// JWT 签名密钥。
    pub jwt_secret: String,
    /// 数据存储根目录。
    pub data_dir: PathBuf,
    /// Agent 实例注册表（跨用户共享，LRU 缓存）。
    pub agent_registry: Arc<AgentRegistry>,
    /// Web 层知识库管理器（用户隔离）。
    pub web_knowledge_manager: Arc<WebKnowledgeManager>,
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
            tracing::warn!(
                error = %e,
                dir = %sessions_dir.display(),
                "Failed to create sessions directory"
            );
        }

        // 确保 agents 子目录存在
        let agents_dir = config.data_dir.join("agents");
        if let Err(e) = tokio::fs::create_dir_all(&agents_dir).await {
            tracing::warn!(
                error = %e,
                dir = %agents_dir.display(),
                "Failed to create agents directory"
            );
        }

        // 确保 knowledge 子目录存在
        let knowledge_dir = config.data_dir.join("knowledge");
        if let Err(e) = tokio::fs::create_dir_all(&knowledge_dir).await {
            tracing::warn!(
                error = %e,
                dir = %knowledge_dir.display(),
                "Failed to create knowledge directory"
            );
        }

        let web_knowledge_manager = Arc::new(WebKnowledgeManager::new(config.data_dir.clone()));

        Self {
            db,
            jwt_secret: config.jwt_secret.clone(),
            data_dir: config.data_dir.clone(),
            agent_registry: Arc::new(AgentRegistry::new(128, web_knowledge_manager.clone())),
            web_knowledge_manager,
            task_scheduler,
        }
    }
}
