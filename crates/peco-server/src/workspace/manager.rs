// ============================================================================
// WorkspaceManager — 多用户 Workspace LRU 缓存管理
// ============================================================================

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use std::sync::RwLock;

use lru::LruCache;
use peco_core::agent::Agent;
use peco_core::config::SystemConfig;
use peco_core::workspace::Workspace;

use crate::error::ApiError;

/// 工作空间管理器 — LRU 缓存 Workspace 实例。
///
/// peco-server 特有（CLI 直接使用 Workspace::open()）。
/// 两级缓存架构：
/// - WorkspaceManager 缓存 Workspace（LRU）
/// - Workspace 内部缓存 Agent（HashMap）
pub struct WorkspaceManager {
    /// 数据根目录。
    data_dir: PathBuf,
    /// 系统级配置（所有 Workspace 共享）。
    system_config: Arc<SystemConfig>,
    /// LRU: user_id → Arc<Workspace>
    cache: RwLock<LruCache<String, Arc<Workspace>>>,
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

    /// 获取或初始化用户 Workspace（同步）。
    pub fn get(&self, user_id: &str) -> Result<Arc<Workspace>, ApiError> {
        // Use write lock + get() (not peek) to correctly track LRU recency.
        {
            let mut cache = self.cache.write().unwrap();
            if let Some(ws) = cache.get(user_id) {
                return Ok(ws.clone());
            }
        }
        // Lock released before expensive I/O.

        // Create workspace
        let root = self.workspace_dir(user_id);
        let ws = Workspace::open(root, user_id.to_string(), &self.system_config)
            .map_err(|e| ApiError::Internal(format!("failed to open workspace: {e}")))?;

        let ws = Arc::new(ws);

        // Re-acquire write lock and double-check before inserting.
        {
            let mut cache = self.cache.write().unwrap();
            // Another request may have opened the workspace while we were in I/O.
            if let Some(existing) = cache.get(user_id) {
                return Ok(existing.clone());
            }
            cache.put(user_id.to_string(), ws.clone());
        }

        tracing::info!(user_id = %user_id, "Workspace opened and cached");
        Ok(ws)
    }

    /// 获取 Agent（委托给 Workspace::load_agent_cached，带两级缓存）。
    pub fn get_agent(&self, user_id: &str, agent_name: &str) -> Result<Arc<Agent>, ApiError> {
        let ws = self.get(user_id)?;
        ws.agent_manager().load_cached(agent_name)
            .map_err(|e| ApiError::Internal(format!("failed to load agent '{agent_name}': {e}")))
    }

    /// 使指定用户的 Workspace 缓存失效。
    pub fn invalidate_user(&self, user_id: &str) {
        let mut cache = self.cache.write().unwrap();
        cache.pop(user_id);
        tracing::debug!(user_id = %user_id, "Workspace cache invalidated");
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

    /// 返回当前缓存的 Workspace 数量。
    #[allow(dead_code)]
    pub fn cache_size(&self) -> usize {
        self.cache.read().unwrap().len()
    }
}
