// ============================================================================
// AppState — 应用全局共享状态
// ============================================================================

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use peco_core::config::SystemConfig;
use sqlx::SqlitePool;

use peco_core::workflow::WorkflowEvent;
use tokio::sync::broadcast;

use crate::config::ServerConfig;
use crate::peco::active::PecoActiveRuns;
use crate::workflow::WorkflowEventSource;
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

    // ── Peco 子系统 ──────────────────────────────────────────────────
    /// 活跃 Peco 运行注册表（runner 与 SSE 连接解耦的枢纽）。
    pub peco_runs: Arc<PecoActiveRuns>,
    /// 自动整理总开关（`memory.consolidation.enabled` 的构造期快照）。
    ///
    /// 同时也是活动时间戳的采集开关 —— 关闭时 `record_activity` 直接返回，
    /// 不写 map（零开销），cron 任务也不注册。
    pub consolidation_enabled: bool,
    /// 每用户最近一次认证活动时刻（自动整理的空闲判定输入）。
    ///
    /// 进程内内存，不落库：空闲判定是调度优化而非正确性前置，重启后
    /// 丢失只会让用户晚一轮被整理（保守方向）。只在认证成功后写入。
    pub last_activity: Arc<Mutex<HashMap<String, Instant>>>,
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

        // 创建 WorkspaceManager（LRU 容量 128；持有连接池用于审计注入）
        let workspace_manager = Arc::new(WorkspaceManager::new(
            config.data_dir.clone(),
            system_config,
            128,
            db.clone(),
        ));

        Self {
            db,
            jwt_secret: config.jwt_secret.clone(),
            data_dir: config.data_dir.clone(),
            workspace_manager,
            cron_scheduler,
            peco_runs: Arc::new(PecoActiveRuns::new()),
            consolidation_enabled: crate::peco::config::PecoConfig::default()
                .memory
                .consolidation
                .enabled,
            last_activity: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 覆盖自动整理总开关（灰度开启与测试构造用；启动后不再变更）。
    pub fn with_consolidation_enabled(mut self, enabled: bool) -> Self {
        self.consolidation_enabled = enabled;
        self
    }

    /// 记录用户的一次认证活动时刻。
    ///
    /// 总开关关闭时直接返回（不采集成活时间戳，零开销）。锁中毒
    /// （前次持锁线程 panic）时放弃本次记录并 warn —— 活动时间戳是调度
    /// 优化，不是认证的正确性前置，绝不因此阻断请求。
    pub fn record_activity(&self, user_id: &str) {
        if !self.consolidation_enabled {
            return;
        }
        match self.last_activity.lock() {
            Ok(mut map) => {
                map.insert(user_id.to_string(), Instant::now());
            }
            Err(e) => tracing::warn!(
                user_id = %user_id,
                error = %e,
                "last_activity lock poisoned, activity not recorded"
            ),
        }
    }

    /// 快照活动时间戳表（cron tick 读一次，避免持锁跨 await）。
    pub fn activity_snapshot(&self) -> HashMap<String, Instant> {
        match self.last_activity.lock() {
            Ok(map) => map.clone(),
            Err(e) => {
                tracing::warn!(error = %e, "last_activity lock poisoned, treating as empty");
                HashMap::new()
            }
        }
    }

    /// 为指定用户创建 workflow 持久化实例（per-user pattern）。
    pub fn workflow_persister_for(&self, user_id: &str) -> SqliteWorkflowPersister {
        SqliteWorkflowPersister::new(self.db.clone(), user_id.to_string())
    }
}

impl WorkflowEventSource for AppState {
    fn subscribe_events(&self, run_id: &str) -> Option<broadcast::Receiver<WorkflowEvent>> {
        crate::workflow::active::subscribe_events(run_id)
    }
}
