// ============================================================================
// WorkspaceManager — 多用户 WorkSpace LRU 缓存管理 + 哈希增量同步
// ============================================================================

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use lru::LruCache;
use peco_core::agent::Agent;
use peco_core::config::SystemConfig;
use peco_core::workspace::{WorkSpace, hash};
use sqlx::SqlitePool;

use crate::error::ApiError;
use crate::file_watcher::FileWatcher;

/// 文件监控引用计数状态。
///
/// SSE 连接建立时 `acquire()` 递增，断开时 `release()` 递减。
/// 首次 acquire（0→1）启动 FileWatcher，末次 release（1→0）停止。
struct WatcherState {
    refs: AtomicUsize,
    /// 实际的 FileWatcher 句柄。None 表示未启动或已停止。
    handle: RwLock<Option<FileWatcher>>,
}

impl WatcherState {
    fn new() -> Self {
        Self {
            refs: AtomicUsize::new(0),
            handle: RwLock::new(None),
        }
    }

    /// 尝试启动 FileWatcher（若尚未启动）。
    ///
    /// 仅在 refs 从 0→1 时启动。
    fn acquire(
        &self,
        workspace_root: PathBuf,
        ws: &Arc<WorkSpace>,
        db: &SqlitePool,
        user_id: &str,
    ) {
        let prev = self.refs.fetch_add(1, Ordering::Relaxed);
        if prev == 0 {
            // 首次引用 → 启动 FileWatcher
            let watcher = FileWatcher::start_with_db(
                workspace_root,
                Arc::downgrade(ws),
                db.clone(),
                user_id.to_string(),
            );
            if let Ok(mut handle) = self.handle.write() {
                *handle = watcher;
            }
            tracing::info!(%user_id, "File watcher started (first SSE connection)");
        }
    }

    /// 尝试停止 FileWatcher（若引用计数归零）。
    fn release(&self) {
        let prev = self.refs.fetch_sub(1, Ordering::Relaxed);
        if prev == 1 {
            // 最后一个引用释放 → 停止 FileWatcher
            match self.handle.write() {
                Ok(mut handle) => {
                    if let Some(watcher) = handle.take() {
                        watcher.stop();
                    }
                }
                Err(_) => {
                    tracing::warn!("File watcher handle lock poisoned; watcher task may leak");
                }
            }
            tracing::info!("File watcher stopped (no active connections)");
        }
    }
}

/// LRU 缓存条目：WorkSpace + 文件监控引用计数。
///
/// FileWatcher 的生命周期由引用计数管理，不再跟随 WorkSpace 创建。
struct CacheEntry {
    ws: Arc<WorkSpace>,
    watcher_state: Arc<WatcherState>,
}

/// 工作空间管理器 — LRU 缓存 WorkSpace 实例 + 哈希增量同步。
///
/// peco-server 特有（CLI 直接使用 WorkSpace::open()）。
///
/// # 两级缓存架构
/// - WorkspaceManager 缓存 WorkSpace（LRU，容量 128）
/// - WorkSpace 内部缓存 Agent / Skill（HashMap）
///
/// # FileWatcher 生命周期
/// - 不再随 WorkSpace 创建启动
/// - 由 SSE 连接通过 `acquire_watcher()` / `release_watcher()` 管理
/// - 引用计数归零时自动停止
/// - LRU 驱逐时强制停止（Drop FileWatcher 发送关闭信号）
///
/// # 哈希增量同步
/// - 首次 `get_synced()` 计算各模块 SHA-256 → 对比 DB 中的 `workspace_hashes`
/// - 哈希匹配 → 跳过 DB 双向同步（WorkSpace::open() 仍从磁盘加载内存缓存）
/// - 哈希不匹配 → 全量扫描 → 双向同步 DB → 更新哈希
pub struct WorkspaceManager {
    /// 数据根目录。
    data_dir: PathBuf,
    /// 系统级配置（所有 WorkSpace 共享）。
    system_config: Arc<SystemConfig>,
    /// LRU: user_id → CacheEntry (WorkSpace + WatcherState)
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

    // ── 获取 WorkSpace ──────────────────────────────────────────────────

    /// 获取或初始化用户 WorkSpace（同步，无 DB 同步）。
    ///
    /// 用于不需要哈希检测的场景（如内部调用）。
    pub fn get(&self, user_id: &str) -> Result<Arc<WorkSpace>, ApiError> {
        {
            let mut cache = self.cache.write().unwrap();
            if let Some(entry) = cache.get(user_id) {
                return Ok(entry.ws.clone());
            }
        }

        let root = self.workspace_dir(user_id);
        let ws = self.open_workspace(user_id, &root)?;
        let ws = Arc::new(ws);
        let watcher_state = Arc::new(WatcherState::new());

        let entry = CacheEntry {
            ws: ws.clone(),
            watcher_state,
        };

        {
            let mut cache = self.cache.write().unwrap();
            if let Some(existing) = cache.get(user_id) {
                return Ok(existing.ws.clone());
            }
            cache.put(user_id.to_string(), entry);
        }

        tracing::info!(user_id = %user_id, "WorkSpace opened (no DB sync)");
        Ok(ws)
    }

    /// 获取或初始化用户 WorkSpace（异步，含哈希对比 + DB 双向同步）。
    ///
    /// # 流程
    ///
    /// 1. LRU 缓存命中 → 直接返回
    /// 2. 计算文件系统各模块哈希 → 对比 DB `workspace_hashes`
    /// 3. 哈希全部匹配 → `WorkSpace::open()`（仍需要初始化内存缓存）
    /// 4. 某模块哈希不匹配 → 全量扫描 → 双向同步 DB → 更新哈希
    pub async fn get_synced(
        &self,
        user_id: &str,
        db: &SqlitePool,
    ) -> Result<Arc<WorkSpace>, ApiError> {
        // 1. LRU 缓存命中
        {
            let mut cache = self.cache.write().unwrap();
            if let Some(entry) = cache.get(user_id) {
                return Ok(entry.ws.clone());
            }
        }

        let root = self.workspace_dir(user_id);

        // 2. 计算文件系统哈希
        let current_hashes = self.compute_all_hashes(&root);

        // 3. 对比 DB 哈希
        let db_hashes = crate::db::workspace_hashes::get_hashes(db, user_id)
            .await
            .unwrap_or_default();

        let mut changed_modules: HashSet<String> = HashSet::new();
        for (module, current_hash) in &current_hashes {
            match db_hashes.get(module) {
                Some(db_hash) if db_hash == current_hash => {
                    tracing::debug!(%user_id, module, "hash matched, skipping sync");
                }
                _ => {
                    changed_modules.insert(module.clone());
                }
            }
        }

        // 4. 打开 WorkSpace（总会初始化内存缓存）
        let ws = self.open_workspace(user_id, &root)?;
        let ws = Arc::new(ws);

        // 5. 对变更的模块执行双向同步
        if !changed_modules.is_empty() {
            tracing::info!(
                %user_id,
                modules = ?changed_modules,
                "Workspace modules changed, syncing"
            );
            self.resync_changed_modules(user_id, db, &ws, &changed_modules, &current_hashes)
                .await;
        } else {
            tracing::info!(%user_id, "All workspace hashes matched, no sync needed");
        }

        // 6. 首次访问且 DB 无哈希记录时也写入哈希
        if db_hashes.is_empty() {
            crate::db::workspace_hashes::upsert_hashes_batch(db, user_id, &current_hashes)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(%user_id, error = %e, "Failed to persist initial workspace hashes");
                });
        }

        let watcher_state = Arc::new(WatcherState::new());
        let entry = CacheEntry {
            ws: ws.clone(),
            watcher_state,
        };

        {
            let mut cache = self.cache.write().unwrap();
            if let Some(existing) = cache.get(user_id) {
                return Ok(existing.ws.clone());
            }
            cache.put(user_id.to_string(), entry);
        }

        tracing::info!(%user_id, "WorkSpace opened with hash sync");
        Ok(ws)
    }

    /// 强制全量重新同步（忽略哈希，始终执行双向同步）。
    pub async fn force_resync(&self, user_id: &str, db: &SqlitePool) -> Result<(), ApiError> {
        let root = self.workspace_dir(user_id);
        let ws = self.get(user_id)?;

        // 清除哈希记录 → 下次启动时必然重新计算
        crate::db::workspace_hashes::delete_hashes(db, user_id)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(%user_id, error = %e, "Failed to clear workspace hashes");
            });

        let current_hashes = self.compute_all_hashes(&root);
        let all_modules: HashSet<String> = current_hashes.keys().cloned().collect();

        self.resync_changed_modules(user_id, db, &ws, &all_modules, &current_hashes)
            .await;

        crate::db::workspace_hashes::upsert_hashes_batch(db, user_id, &current_hashes)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(%user_id, error = %e, "Failed to persist workspace hashes");
            });

        tracing::info!(%user_id, "WorkSpace force resync completed");
        Ok(())
    }

    // ── Agent 加载 ──────────────────────────────────────────────────────

    /// 获取 Agent（委托给 WorkSpace::load_agent_cached，带两级缓存）。
    pub fn get_agent(&self, user_id: &str, agent_name: &str) -> Result<Arc<Agent>, ApiError> {
        let ws = self.get(user_id)?;
        ws.agent_manager()
            .load_cached(agent_name)
            .map_err(|e| ApiError::Internal(format!("failed to load agent '{agent_name}': {e}")))
    }

    // ── 缓存管理 ────────────────────────────────────────────────────────

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

    // ── FileWatcher 引用计数管理 ────────────────────────────────────────

    /// 增加 FileWatcher 引用计数（SSE 连接建立时调用）。
    ///
    /// 首次引用时自动启动 FileWatcher。
    pub fn acquire_watcher(&self, user_id: &str, db: &SqlitePool) {
        // 确保 WorkSpace 已加载
        if let Ok(ws) = self.get(user_id) {
            let cache = self.cache.read().unwrap();
            if let Some(entry) = cache.peek(user_id) {
                entry
                    .watcher_state
                    .acquire(self.workspace_dir(user_id), &ws, db, user_id);
            } else {
                // 极低概率：get() 和 peek() 之间条目被 LRU 驱逐
                tracing::warn!(
                    %user_id,
                    "WorkSpace evicted between get() and peek(); file watcher not started"
                );
            }
        }
    }

    /// 减少 FileWatcher 引用计数（SSE 连接断开时调用）。
    ///
    /// 末次引用释放时自动停止 FileWatcher。
    pub fn release_watcher(&self, user_id: &str) {
        let cache = self.cache.read().unwrap();
        if let Some(entry) = cache.peek(user_id) {
            entry.watcher_state.release();
        }
    }

    // ── 路径 / 配置 ─────────────────────────────────────────────────────

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

    // ── 私有方法 ────────────────────────────────────────────────────────

    /// 打开 WorkSpace（纯 I/O，不含缓存逻辑）。
    fn open_workspace(&self, user_id: &str, root: &std::path::Path) -> Result<WorkSpace, ApiError> {
        WorkSpace::open(root.to_path_buf(), user_id.to_string(), &self.system_config)
            .map_err(|e| ApiError::Internal(format!("failed to open workspace: {e}")))
    }

    /// 计算所有模块的哈希。
    fn compute_all_hashes(&self, root: &std::path::Path) -> HashMap<String, String> {
        let mut hashes = HashMap::new();
        hashes.insert(
            "agents".to_string(),
            hash::compute_agents_hash(&root.join("agents")),
        );
        hashes.insert(
            "skills".to_string(),
            hash::compute_skills_hash(&root.join("skills")),
        );
        hashes.insert(
            "workflows".to_string(),
            hash::compute_workflows_hash(&root.join("workflows")),
        );
        hashes.insert("mcp".to_string(), hash::compute_mcp_hash(root));
        hashes.insert("providers".to_string(), hash::compute_providers_hash(root));
        hashes
    }

    /// 对变更的模块执行双向同步。
    async fn resync_changed_modules(
        &self,
        user_id: &str,
        db: &SqlitePool,
        ws: &WorkSpace,
        changed_modules: &HashSet<String>,
        current_hashes: &HashMap<String, String>,
    ) {
        // Agents 模块：双向 DB 同步
        if changed_modules.contains("agents") {
            crate::db::sync::sync_agents_with_db(user_id, db, ws).await;
        }

        // Skills / Workflows / MCP / Providers：暂不涉及 DB，仅更新哈希
        // （内存缓存已在 WorkSpace::open() 中完成初始化）

        // 更新变更模块的哈希
        for module in changed_modules {
            if let Some(hash) = current_hashes.get(module)
                && let Err(e) =
                    crate::db::workspace_hashes::upsert_hash(db, user_id, module, hash).await
            {
                tracing::warn!(%user_id, module, error = %e, "Failed to update hash after sync");
            }
        }
    }
}
