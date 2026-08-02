// ============================================================================
// WorkspaceManager — 多用户 WorkSpace LRU 缓存管理
// ============================================================================

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use std::sync::RwLock;

use lru::LruCache;
use peco_core::agent::Agent;
use peco_core::config::SystemConfig;
use peco_core::workspace::WorkSpace;

use crate::error::ApiError;
use crate::file_watcher::FileWatcher;

/// LRU 缓存条目：WorkSpace + 可选的文件监控句柄。
///
/// 当条目被 LRU 驱逐时，`FileWatcher` 的 Drop 会自动停止后台监控任务。
struct CacheEntry {
    ws: Arc<WorkSpace>,
    /// 文件监控句柄。Drop 时发送关闭信号。
    _watcher: Option<FileWatcher>,
}

/// 工作空间管理器 — LRU 缓存 WorkSpace 实例。
///
/// peco-server 特有（CLI 直接使用 WorkSpace::open()）。
/// 两级缓存架构：
/// - WorkspaceManager 缓存 WorkSpace（LRU）
/// - WorkSpace 内部缓存 Agent（HashMap）
///
/// 每个新创建的 WorkSpace 会自动启动文件监控（通过 `notify` crate），
/// 当 WorkSpace 被 LRU 驱逐时，文件监控自动停止。
pub struct WorkspaceManager {
    /// 数据根目录。
    data_dir: PathBuf,
    /// 系统级配置（所有 WorkSpace 共享）。
    system_config: Arc<SystemConfig>,
    /// LRU: user_id → CacheEntry (WorkSpace + FileWatcher)
    cache: RwLock<LruCache<String, CacheEntry>>,
}

impl WorkspaceManager {
    /// 创建新的 WorkspaceManager。
    pub fn new(data_dir: PathBuf, system_config: Arc<SystemConfig>, capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).unwrap();
        Self {
            data_dir,
            system_config,
            cache: RwLock::new(LruCache::new(cap)),
        }
    }

    /// 获取或初始化用户 WorkSpace（同步）。
    ///
    /// 首次创建时会自动启动文件监控。
    pub fn get(&self, user_id: &str) -> Result<Arc<WorkSpace>, ApiError> {
        // Use write lock + get() (not peek) to correctly track LRU recency.
        {
            let mut cache = self.cache.write().unwrap();
            if let Some(entry) = cache.get(user_id) {
                return Ok(entry.ws.clone());
            }
        }
        // Lock released before expensive I/O.

        // Create workspace
        let root = self.workspace_dir(user_id);
        let ws = WorkSpace::open(root, user_id.to_string(), &self.system_config)
            .map_err(|e| ApiError::Internal(format!("failed to open workspace: {e}")))?;

        let ws = Arc::new(ws);

        // Start file watcher for this workspace
        let watcher = FileWatcher::start(self.workspace_dir(user_id), Arc::downgrade(&ws));

        if watcher.is_none() {
            tracing::warn!(user_id = %user_id, "File watcher failed to start for workspace");
        }

        let entry = CacheEntry {
            ws: ws.clone(),
            _watcher: watcher,
        };

        // Re-acquire write lock and double-check before inserting.
        {
            let mut cache = self.cache.write().unwrap();
            // Another request may have opened the workspace while we were in I/O.
            if let Some(existing) = cache.get(user_id) {
                return Ok(existing.ws.clone());
            }
            cache.put(user_id.to_string(), entry);
        }

        tracing::info!(user_id = %user_id, "WorkSpace opened and cached");
        Ok(ws)
    }

    /// 获取 Agent（委托给 WorkSpace::load_agent_cached，带两级缓存）。
    pub fn get_agent(&self, user_id: &str, agent_name: &str) -> Result<Arc<Agent>, ApiError> {
        let ws = self.get(user_id)?;
        ws.agent_manager()
            .load_cached(agent_name)
            .map_err(|e| ApiError::Internal(format!("failed to load agent '{agent_name}': {e}")))
    }

    /// 使指定用户的 WorkSpace 缓存失效（同时停止文件监控）。
    pub fn invalidate_user(&self, user_id: &str) {
        let mut cache = self.cache.write().unwrap();
        // LRU::pop 返回 CacheEntry；drop 时 FileWatcher 的 Drop 停止监控
        cache.pop(user_id);
        tracing::debug!(user_id = %user_id, "WorkSpace cache invalidated");
    }

    /// 使指定用户的指定 Agent 缓存失效。
    pub fn invalidate_agent(&self, user_id: &str, agent_name: &str) -> Result<(), ApiError> {
        let ws = self.get(user_id)?;
        ws.agent_manager().invalidate(agent_name);
        Ok(())
    }

    /// 获取指定用户的 workspace 目录路径。
    pub fn workspace_dir(&self, user_id: &str) -> PathBuf {
        self.data_dir.join("workspaces").join(user_id)
    }

    /// 返回系统级配置引用。
    pub fn system_config(&self) -> &SystemConfig {
        &self.system_config
    }

    /// 返回当前缓存的 WorkSpace 数量。
    #[allow(dead_code)]
    pub fn cache_size(&self) -> usize {
        self.cache.read().unwrap().len()
    }
}
