// ============================================================================
// AgentManager — Agent 生命周期管理
// ============================================================================
//
// 职责：
// - 扫描 agents/ 目录，缓存 Tier-1 元数据（name + description）
// - 加载完整 Agent 并缓存（Tier-2）
// - Agent 文件 CRUD（save / delete）
// - 实现 AgentAccess / SkillProvider / KnowledgeAccess trait，供 ToolDependencies 使用

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use tracing::{debug, info, warn};

use crate::agent::agent_config::{AgentProfile, split_frontmatter};
use crate::agent::{Agent, AgentError};
use crate::config::{McpConfig, UserConfig};
use crate::knowledge::KnowledgeManager;
use crate::mcp::McpConfigStore;
use crate::skills::SkillRegister;
use crate::workspace::{
    AgentAccess, KnowledgeAccess, SkillProvider, ToolDependencies, WorkspaceError,
};

// ── AgentMeta ──────────────────────────────────────────────────────────

/// Tier-1 元数据：从 agent.md frontmatter 解析的最少信息。
#[derive(Debug, Clone)]
pub struct AgentMeta {
    pub name: String,
    pub description: String,
}

// ── AgentManager ───────────────────────────────────────────────────────

/// Agent 生命周期管理器。
///
/// 两级缓存：
/// - Tier 1：`metas` — 扫描目录时缓存的 frontmatter 摘要（name + description）
/// - Tier 2：`cache` — 完整加载的 Agent 实例
///
/// 同时实现 [`AgentAccess`]、[`SkillProvider`]、[`KnowledgeAccess`]，
/// 可直接作为 [`ToolDependencies`] 的组成部分。
pub struct AgentManager {
    agents_dir: PathBuf,
    user_id: String,
    user_config: UserConfig,
    /// MCP 配置的共享持有者（与 `user_config.mcp` 解耦，支持独立热重载）。
    mcp_config: McpConfigStore,
    skill_registry: Arc<SkillRegister>,
    knowledge_manager: Arc<KnowledgeManager>,
    /// Tier-1 元数据缓存（name → AgentMeta）
    metas: RwLock<HashMap<String, AgentMeta>>,
    /// Tier-2 完整 Agent 实例缓存（name → Arc<Agent>）
    cache: RwLock<HashMap<String, Arc<Agent>>>,
}

impl AgentManager {
    /// 创建新的 AgentManager。
    ///
    /// 创建后应调用 [`init`](Self::init) 扫描目录并缓存元数据。
    pub fn new(
        agents_dir: PathBuf,
        user_id: String,
        user_config: UserConfig,
        mcp_config: McpConfigStore,
        skill_registry: Arc<SkillRegister>,
        knowledge_manager: Arc<KnowledgeManager>,
    ) -> Self {
        Self {
            agents_dir,
            user_id,
            user_config,
            mcp_config,
            skill_registry,
            knowledge_manager,
            metas: RwLock::new(HashMap::new()),
            cache: RwLock::new(HashMap::new()),
        }
    }

    // ── 初始化 ───────────────────────────────────────────────────────

    /// 扫描 `agents/` 目录，解析每个 `agent.md` 的 frontmatter，
    /// 缓存 Tier-1 元数据。返回成功扫描的 Agent 数量。
    pub fn init(&self) -> Result<usize, AgentError> {
        let mut metas = self
            .metas
            .write()
            .map_err(|e| AgentError::Config(format!("agent metas lock poisoned: {e}")))?;
        metas.clear();

        if !self.agents_dir.exists() {
            return Ok(0);
        }

        let entries: Vec<_> = std::fs::read_dir(&self.agents_dir)
            .map_err(|e| AgentError::Config(format!("failed to read agents dir: {e}")))?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .collect();

        for entry in entries {
            let md_path = entry.path().join("agent.md");
            if !md_path.exists() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            match Self::parse_meta(&md_path) {
                Ok(meta) => {
                    debug!(agent = %name, "Agent metadata cached");
                    metas.insert(name, meta);
                }
                Err(e) => {
                    warn!(agent = %name, error = %e, "Failed to parse agent metadata");
                }
            }
        }

        Ok(metas.len())
    }

    /// 解析单个 agent.md 的 frontmatter，仅提取 name + description。
    fn parse_meta(md_path: &Path) -> Result<AgentMeta, AgentError> {
        let raw = std::fs::read_to_string(md_path).map_err(|source| AgentError::Io {
            path: md_path.to_path_buf(),
            source,
        })?;
        let (frontmatter_str, _) =
            split_frontmatter(&raw).map_err(AgentError::InvalidFrontmatter)?;
        let profile: AgentProfile = serde_yaml::from_str(frontmatter_str)?;
        Ok(AgentMeta {
            name: profile.agent.name,
            description: profile.agent.description,
        })
    }

    /// 重新扫描 `agents/` 目录，刷新 Tier-1 元数据缓存。
    /// 不清除 Tier-2 缓存 — 已加载的 Agent 实例（含 MCP 连接）不受影响。
    /// 返回重新发现的 Agent 数量。
    pub fn rescan(&self) -> Result<usize, AgentError> {
        self.init()
    }

    // ── Agent 加载 ───────────────────────────────────────────────────

    /// 加载 Agent（带 Tier-2 缓存）。
    ///
    /// 需要 `&Arc<Self>` 以构建递归的 [`ToolDependencies`]。
    pub fn load_cached(self: &Arc<Self>, name: &str) -> Result<Arc<Agent>, AgentError> {
        {
            let cache = self
                .cache
                .read()
                .map_err(|e| AgentError::Config(format!("agent cache lock poisoned: {e}")))?;
            if let Some(agent) = cache.get(name) {
                debug!(agent = %name, "Agent cache hit");
                return Ok(agent.clone());
            }
        }

        let agent = Arc::new(self.load_from_file(name, self.build_deps())?);

        {
            let mut cache = self
                .cache
                .write()
                .map_err(|e| AgentError::Config(format!("agent cache lock poisoned: {e}")))?;
            cache.insert(name.to_string(), agent.clone());
        }

        info!(agent = %name, "Agent loaded and cached");
        Ok(agent)
    }

    /// 非缓存加载（供 [`AgentAccess`] trait 实现路径使用）。
    fn load_uncached(&self, name: &str) -> Result<Arc<Agent>, AgentError> {
        let deps = self.build_deps_direct();
        let agent = self.load_from_file(name, deps)?;
        Ok(Arc::new(agent))
    }

    /// 从 agent.md 文件组装 Agent 实例。
    ///
    /// 合成 `UserConfig`：providers 不变，mcp 来自 `McpConfigStore` 快照。
    /// 这样已缓存的 Agent 获取的是加载时的 MCP 配置快照，不受后续热更新影响。
    fn load_from_file(&self, name: &str, deps: ToolDependencies) -> Result<Agent, AgentError> {
        let path = self.md_path(name);
        if !path.exists() {
            return Err(AgentError::Config(format!(
                "agent '{name}' not found at {}",
                path.display()
            )));
        }
        let mut effective_config = self.user_config.clone();
        effective_config.mcp = self.mcp_config.get();
        Agent::from_file(&path, &effective_config, &deps)
    }

    // ── 依赖构建 ────────────────────────────────────────────────────

    /// 构建 [`ToolDependencies`]（缓存路径）。
    ///
    /// 通过 `Arc<Self>` 将 `AgentManager` 自身作为 `dyn AgentAccess` 注入。
    /// 注意：trait 分发走 [`load_uncached`](Self::load_uncached)（非缓存路径）；
    /// Tier-2 缓存仅对直接调用 [`load_cached`](Self::load_cached) 的调用方生效。
    pub fn build_deps(self: &Arc<Self>) -> ToolDependencies {
        ToolDependencies {
            agent_access: self.clone() as Arc<dyn AgentAccess>,
            skill_provider: self.clone() as Arc<dyn SkillProvider>,
            knowledge_access: self.clone() as Arc<dyn KnowledgeAccess>,
            allowed_kbs: Vec::new(),
            workflow_access: None,
        }
    }

    /// 构建 [`ToolDependencies`]（非缓存路径）。
    ///
    /// 使用轻量 ref 结构避免 `Arc<Self>` 循环依赖，
    /// 供 [`AgentAccess`] trait 的 `load_agent(&self)` 实现使用。
    ///
    /// `AmAgentAccess` 在构造时获得 MCP 配置快照，保持"已构造 Agent 不受热更新影响"的语义。
    fn build_deps_direct(&self) -> ToolDependencies {
        let mut config = self.user_config.clone();
        config.mcp = self.mcp_config.get();
        ToolDependencies {
            agent_access: Arc::new(AmAgentAccess {
                agents_dir: self.agents_dir.clone(),
                user_id: self.user_id.clone(),
                user_config: config,
                skill_registry: self.skill_registry.clone(),
                knowledge_manager: self.knowledge_manager.clone(),
            }),
            skill_provider: Arc::new(AmSkillProvider {
                registry: self.skill_registry.clone(),
            }),
            knowledge_access: Arc::new(AmKnowledgeAccess {
                user_id: self.user_id.clone(),
                km: self.knowledge_manager.clone(),
            }),
            allowed_kbs: Vec::new(),
            workflow_access: None,
        }
    }

    // ── 元数据查询 ──────────────────────────────────────────────────

    /// 返回所有已缓存 Agent 的 Tier-1 元数据列表。
    pub fn list_meta(&self) -> Vec<AgentMeta> {
        self.metas
            .read()
            .map(|m| {
                let mut metas: Vec<_> = m.values().cloned().collect();
                metas.sort_by(|a, b| a.name.cmp(&b.name));
                metas
            })
            .unwrap_or_default()
    }

    /// 返回所有已缓存 Agent 的名称列表。
    ///
    /// 注意：仅在 [`init`](Self::init) 后被调用才准确；
    /// 若初始化后新增了 Agent 目录但未重新 init，不会包含在内。
    pub fn list_names(&self) -> Vec<String> {
        self.list_meta().into_iter().map(|m| m.name).collect()
    }

    // ── 缓存管理 ────────────────────────────────────────────────────

    /// 使指定 Agent 的 Tier-2 缓存失效（下次加载时重新解析组装）。
    pub fn invalidate(&self, name: &str) {
        if let Ok(mut cache) = self.cache.write() {
            cache.remove(name);
            debug!(agent = %name, "Agent cache invalidated");
        }
    }

    /// 重新加载 MCP 配置。仅影响后续新加载的 Agent；
    /// 已缓存的 Agent 实例保持原有 MCP 连接。
    pub fn reload_mcp_config(&self, workspace_root: &Path, system_mcp: &McpConfig) -> usize {
        self.mcp_config.reload(workspace_root, system_mcp)
    }

    /// 刷新单个 Agent 的缓存（Tier-2 失效 + Tier-1 元数据更新）。
    ///
    /// 重新解析 agent.md 的 frontmatter 并更新 Tier-1 元数据。
    /// 同时使 Tier-2 缓存失效，确保下次 [`load_cached`] 重新解析完整 Agent。
    ///
    /// 如果 agent.md 文件不存在，则从两级缓存中移除该 Agent。
    /// 这适用于 agent.md 被外部删除的场景。
    pub fn refresh_one(&self, name: &str) {
        self.invalidate(name);

        let md_path = self.md_path(name);
        if md_path.exists() {
            match Self::parse_meta(&md_path) {
                Ok(meta) => {
                    if let Ok(mut metas) = self.metas.write() {
                        metas.insert(name.to_string(), meta);
                    }
                    debug!(agent = %name, "Agent metadata refreshed from disk");
                }
                Err(e) => {
                    warn!(agent = %name, error = %e, "Failed to parse agent metadata, keeping stale cache");
                }
            }
        } else {
            // Agent 目录已被删除 — 从两级缓存中清理
            if let Ok(mut metas) = self.metas.write() {
                metas.remove(name);
            }
            debug!(agent = %name, "Agent removed from caches (directory gone)");
        }
    }

    // ── 文件 CRUD ───────────────────────────────────────────────────

    /// 保存 Agent 的 `agent.md` 文件并刷新缓存。
    pub fn save(&self, name: &str, content: &str) -> Result<(), WorkspaceError> {
        // 使用与 parse_meta 相同的 serde_yaml 解析做验证，
        // 确保写盘的内容一定能被后续缓存识别。
        // split_frontmatter 仅检查 --- 分隔符，不验证 YAML 结构。
        let (_profile, _body) = crate::agent::agent_config::parse_agent_md(content)
            .map_err(|e| WorkspaceError::InvalidAgentFormat(e.to_string()))?;

        let dir = self.agents_dir.join(name);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("agent.md"), content)?;
        self.invalidate(name);

        // 刷新 Tier-1 元数据（已验证过 YAML 合法，此处不会失败）
        if let Ok(mut metas) = self.metas.write()
            && let Ok(meta) = Self::parse_meta(&dir.join("agent.md"))
        {
            metas.insert(name.to_string(), meta);
        }

        info!(agent = %name, "Agent file saved");
        Ok(())
    }

    /// 删除 Agent 目录并清除缓存。
    pub fn delete(&self, name: &str) -> Result<(), WorkspaceError> {
        let dir = self.agents_dir.join(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        self.invalidate(name);
        if let Ok(mut metas) = self.metas.write() {
            metas.remove(name);
        }
        info!(agent = %name, "Agent deleted");
        Ok(())
    }

    // ── 路径 ────────────────────────────────────────────────────────

    /// 返回指定 Agent 的 `agent.md` 文件路径。
    pub fn md_path(&self, name: &str) -> PathBuf {
        self.agents_dir.join(name).join("agent.md")
    }
}

// ── Trait 实现：Arc<AgentManager> 可直接作为 ToolDependencies 的组成部分 ─

impl AgentAccess for AgentManager {
    fn load_agent(&self, name: &str) -> Result<Arc<Agent>, AgentError> {
        self.load_uncached(name)
    }

    fn list_agent_names(&self) -> Vec<String> {
        self.list_names()
    }

    fn save_agent(&self, name: &str, content: &str) -> Result<(), String> {
        self.save(name, content).map_err(|e| e.to_string())
    }
}

impl SkillProvider for AgentManager {
    fn skill_registry(&self) -> &Arc<SkillRegister> {
        &self.skill_registry
    }
}

impl KnowledgeAccess for AgentManager {
    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn knowledge_manager(&self) -> &Arc<KnowledgeManager> {
        &self.knowledge_manager
    }
}

// ============================================================================
// 内部辅助结构 — 非缓存 AgentAccess trait 路径
// ============================================================================
//
// 当通过 `AgentAccess::load_agent(&self)` 加载子 Agent 时，
// 方法签名只提供 `&self`，无法获得 `Arc<Self>`。
// 因此使用轻量的字段快照来构建递归的 ToolDependencies，
// 避免 Arc 循环依赖。

/// 非缓存的 AgentAccess 实现（用于递归子 Agent 加载）。
///
/// 每次调用 `load_agent` 都会重新解析 agent.md 并组装完整的 Agent，
/// 不经过 Tier-2 缓存。`save_agent` 直接写入文件系统，
/// 不刷新 AgentManager 缓存（这些路径下 Agent 总是从磁盘加载）。
struct AmAgentAccess {
    agents_dir: PathBuf,
    user_id: String,
    user_config: UserConfig,
    skill_registry: Arc<SkillRegister>,
    knowledge_manager: Arc<KnowledgeManager>,
}

impl AgentAccess for AmAgentAccess {
    fn load_agent(&self, name: &str) -> Result<Arc<Agent>, AgentError> {
        let path = self.agents_dir.join(name).join("agent.md");
        if !path.exists() {
            return Err(AgentError::Config(format!(
                "agent '{name}' not found at {}",
                path.display()
            )));
        }

        let deps = ToolDependencies {
            agent_access: Arc::new(AmAgentAccess {
                agents_dir: self.agents_dir.clone(),
                user_id: self.user_id.clone(),
                user_config: self.user_config.clone(),
                skill_registry: self.skill_registry.clone(),
                knowledge_manager: self.knowledge_manager.clone(),
            }),
            skill_provider: Arc::new(AmSkillProvider {
                registry: self.skill_registry.clone(),
            }),
            knowledge_access: Arc::new(AmKnowledgeAccess {
                user_id: self.user_id.clone(),
                km: self.knowledge_manager.clone(),
            }),
            allowed_kbs: Vec::new(),
            workflow_access: None,
        };

        let agent = Agent::from_file(&path, &self.user_config, &deps)?;
        Ok(Arc::new(agent))
    }

    fn list_agent_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.agents_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if entry.path().join("agent.md").exists() {
                        names.push(name);
                    }
                }
            }
        }
        names.sort();
        names
    }

    fn save_agent(&self, name: &str, content: &str) -> Result<(), String> {
        // 使用与 AgentManager::save() 相同的验证路径
        crate::agent::agent_config::parse_agent_md(content)
            .map_err(|e| format!("Invalid agent.md format: {e}"))?;

        let dir = self.agents_dir.join(name);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create agent directory: {e}"))?;
        std::fs::write(dir.join("agent.md"), content)
            .map_err(|e| format!("Failed to write agent.md: {e}"))?;

        Ok(())
    }
}

struct AmSkillProvider {
    registry: Arc<SkillRegister>,
}

impl SkillProvider for AmSkillProvider {
    fn skill_registry(&self) -> &Arc<SkillRegister> {
        &self.registry
    }
}

struct AmKnowledgeAccess {
    user_id: String,
    km: Arc<KnowledgeManager>,
}

impl KnowledgeAccess for AmKnowledgeAccess {
    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn knowledge_manager(&self) -> &Arc<KnowledgeManager> {
        &self.km
    }
}
