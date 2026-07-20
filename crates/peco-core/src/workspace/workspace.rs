// ============================================================================
// Workspace — 用户隔离的核心抽象
// ============================================================================

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::agent::Agent;
use crate::config::{SystemConfig, UserConfig};
use crate::knowledge::KnowledgeManager;
use crate::skills::GlobalSkillList;

use super::deps::{AgentLoader, KnowledgeAccess, SkillProvider, ToolDependencies};
use super::error::WorkspaceError;
use super::tool_register::ToolRegister;
use crate::tools::ToolExecutor;

// ============================================================================
// Workspace
// ============================================================================

pub struct Workspace {
    user_id: String,
    root: PathBuf,
    config: UserConfig,
    skill_registry: Arc<RwLock<GlobalSkillList>>,
    knowledge_manager: Arc<KnowledgeManager>,
    agent_cache: RwLock<HashMap<String, Arc<Agent>>>,
}

impl Workspace {
    pub fn open(
        root: PathBuf,
        user_id: String,
        system_config: &SystemConfig,
    ) -> Result<Self, WorkspaceError> {
        std::fs::create_dir_all(&root).map_err(|e| {
            WorkspaceError::WorkspaceDir(format!(
                "failed to create workspace dir '{}': {e}",
                root.display()
            ))
        })?;

        for subdir in &["skills", "knowledge", "agents"] {
            let dir = root.join(subdir);
            if !dir.exists() {
                std::fs::create_dir_all(&dir).map_err(|e| {
                    WorkspaceError::WorkspaceDir(format!(
                        "failed to create '{}': {e}",
                        dir.display()
                    ))
                })?;
            }
        }

        let config = UserConfig::load(system_config, &root)?;

        let user_skills_dir = root.join("skills");
        let mut registry = GlobalSkillList::new(user_skills_dir.clone());
        if user_skills_dir.exists()
            && let Err(e) = registry.init()
        {
            tracing::warn!(error = %e, "Failed to scan user skills");
        }
        let skill_registry = Arc::new(RwLock::new(registry));

        let kb_dir = root.join("knowledge");
        let knowledge_manager = Arc::new(KnowledgeManager::new(kb_dir));

        Ok(Self {
            user_id,
            root,
            config,
            skill_registry,
            knowledge_manager,
            agent_cache: RwLock::new(HashMap::new()),
        })
    }

    // ── 访问器 ──────────────────────────────────────────────────────

    pub fn user_id(&self) -> &str {
        &self.user_id
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn config(&self) -> &UserConfig {
        &self.config
    }
    pub fn skill_registry(&self) -> &Arc<RwLock<GlobalSkillList>> {
        &self.skill_registry
    }
    pub fn knowledge_manager(&self) -> &Arc<KnowledgeManager> {
        &self.knowledge_manager
    }

    // ── Agent 操作（缓存版，需要 Arc<Self>）─────────────────────────

    /// 从 agents/{name}/agent.md 加载 Agent（带缓存）。
    /// 需要 `Arc<Workspace>` 以构建 ToolDependencies。
    pub fn load_agent_cached(
        self: &Arc<Self>,
        name: &str,
    ) -> Result<Arc<Agent>, crate::agent::AgentError> {
        {
            let cache = self.agent_cache.read().map_err(|e| {
                crate::agent::AgentError::Config(format!("agent cache lock poisoned: {e}"))
            })?;
            if let Some(agent) = cache.get(name) {
                tracing::debug!(agent = %name, "Agent cache hit");
                return Ok(agent.clone());
            }
        }

        let agent = self.load_agent_uncached(name)?;
        let agent = Arc::new(agent);

        {
            let mut cache = self.agent_cache.write().map_err(|e| {
                crate::agent::AgentError::Config(format!("agent cache lock poisoned: {e}"))
            })?;
            cache.insert(name.to_string(), agent.clone());
        }

        tracing::info!(agent = %name, "Agent loaded and cached");
        Ok(agent)
    }

    /// 从文件加载 Agent（不走缓存，不需要 Arc<Self>）。
    fn load_agent_uncached(&self, name: &str) -> Result<Agent, crate::agent::AgentError> {
        let path = self.agent_md_path(name);
        if !path.exists() {
            return Err(crate::agent::AgentError::Config(format!(
                "agent '{name}' not found at {}",
                path.display()
            )));
        }

        let deps = self.build_deps_direct();
        Agent::from_file(&path, &self.config, &self.skill_registry, &deps)
    }

    /// 直接构建 ToolDependencies（用于内部 load_agent_uncached）。
    fn build_deps_direct(&self) -> ToolDependencies {
        // Build agent_loader using the Workspace's own load_agent_uncached.
        // We create a thin wrapper that delegates.
        let agent_loader: Arc<dyn AgentLoader> = Arc::new(UncachedAgentLoader {
            workspace: WorkspaceRef {
                user_id: self.user_id.clone(),
                root: self.root.clone(),
                config: self.config.clone(),
                skill_registry: self.skill_registry.clone(),
                knowledge_manager: self.knowledge_manager.clone(),
            },
        });

        ToolDependencies {
            agent_loader,
            skill_provider: Arc::new(WsSkillProvider {
                registry: self.skill_registry.clone(),
            }),
            knowledge_access: Arc::new(WsKnowledgeAccess {
                user_id: self.user_id.clone(),
                km: self.knowledge_manager.clone(),
            }),
        }
    }

    pub fn list_agent_names(&self) -> Vec<String> {
        let agents_dir = self.agents_dir();
        if !agents_dir.exists() {
            return Vec::new();
        }
        let mut names = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&agents_dir) {
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

    pub fn invalidate_agent_cache(&self, name: &str) {
        if let Ok(mut cache) = self.agent_cache.write() {
            cache.remove(name);
            tracing::debug!(agent = %name, "Agent cache invalidated");
        }
    }

    // ── Tool 组装（需要 Arc<Self>）──────────────────────────────────

    pub fn tool_dependencies(self: &Arc<Self>) -> ToolDependencies {
        ToolDependencies {
            agent_loader: self.clone() as Arc<dyn AgentLoader>,
            skill_provider: self.clone() as Arc<dyn SkillProvider>,
            knowledge_access: self.clone() as Arc<dyn KnowledgeAccess>,
        }
    }

    pub fn build_tool_executor(self: &Arc<Self>, tool_names: &[String]) -> Arc<dyn ToolExecutor> {
        let deps = self.tool_dependencies();
        ToolRegister::build(tool_names, &deps)
    }

    // ── Agent 文件 CRUD ──────────────────────────────────────────────

    pub fn save_agent(&self, name: &str, content: &str) -> Result<(), WorkspaceError> {
        crate::agent::agent_config::split_frontmatter(content)
            .map_err(WorkspaceError::InvalidAgentFormat)?;

        let dir = self.agents_dir().join(name);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("agent.md");
        std::fs::write(&path, content)?;
        self.invalidate_agent_cache(name);
        tracing::info!(agent = %name, "Agent file saved");
        Ok(())
    }

    pub fn delete_agent(&self, name: &str) -> Result<(), WorkspaceError> {
        let dir = self.agents_dir().join(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        self.invalidate_agent_cache(name);
        tracing::info!(agent = %name, "Agent deleted");
        Ok(())
    }

    // ── 路径辅助 ─────────────────────────────────────────────────────

    pub fn agents_dir(&self) -> PathBuf {
        self.root.join("agents")
    }
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }
    pub fn agent_md_path(&self, name: &str) -> PathBuf {
        self.agents_dir().join(name).join("agent.md")
    }
}

// ============================================================================
// Narrow trait implementations — Arc<Workspace> 可直接作为 dyn Trait
// ============================================================================

impl AgentLoader for Workspace {
    fn load_agent(&self, name: &str) -> Result<Arc<Agent>, crate::agent::AgentError> {
        // When called through the trait, load without caching (no Arc<Self> available).
        let agent = self.load_agent_uncached(name)?;
        Ok(Arc::new(agent))
    }

    fn list_agent_names(&self) -> Vec<String> {
        Workspace::list_agent_names(self)
    }
}

impl SkillProvider for Workspace {
    fn skill_registry(&self) -> &Arc<RwLock<GlobalSkillList>> {
        &self.skill_registry
    }
}

impl KnowledgeAccess for Workspace {
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn knowledge_manager(&self) -> &Arc<KnowledgeManager> {
        &self.knowledge_manager
    }
}

// ============================================================================
// Lightweight wrapper for uncached AgentLoader
// ============================================================================

/// A minimal snapshot of Workspace data needed by UncachedAgentLoader.
struct WorkspaceRef {
    user_id: String,
    root: PathBuf,
    config: UserConfig,
    skill_registry: Arc<RwLock<GlobalSkillList>>,
    knowledge_manager: Arc<KnowledgeManager>,
}

/// AgentLoader implementation used inside `build_deps_direct`.
/// Loads agents without caching.
struct UncachedAgentLoader {
    workspace: WorkspaceRef,
}

impl AgentLoader for UncachedAgentLoader {
    fn load_agent(&self, name: &str) -> Result<Arc<Agent>, crate::agent::AgentError> {
        let path = self
            .workspace
            .root
            .join("agents")
            .join(name)
            .join("agent.md");
        if !path.exists() {
            return Err(crate::agent::AgentError::Config(format!(
                "agent '{name}' not found at {}",
                path.display()
            )));
        }

        // Build deps re-using the Workspace's shared KnowledgeManager.
        let deps = ToolDependencies {
            agent_loader: Arc::new(UncachedAgentLoader {
                workspace: WorkspaceRef {
                    user_id: self.workspace.user_id.clone(),
                    root: self.workspace.root.clone(),
                    config: self.workspace.config.clone(),
                    skill_registry: self.workspace.skill_registry.clone(),
                    knowledge_manager: self.workspace.knowledge_manager.clone(),
                },
            }),
            skill_provider: Arc::new(WsSkillProvider {
                registry: self.workspace.skill_registry.clone(),
            }),
            knowledge_access: Arc::new(WsKnowledgeAccess {
                user_id: self.workspace.user_id.clone(),
                km: self.workspace.knowledge_manager.clone(),
            }),
        };

        let agent = Agent::from_file(
            &path,
            &self.workspace.config,
            &self.workspace.skill_registry,
            &deps,
        )?;
        Ok(Arc::new(agent))
    }

    fn list_agent_names(&self) -> Vec<String> {
        let agents_dir = self.workspace.root.join("agents");
        if !agents_dir.exists() {
            return Vec::new();
        }
        let mut names = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&agents_dir) {
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
}

// ── Concrete wrapper structs for the other traits ─────────────────────

struct WsSkillProvider {
    registry: Arc<RwLock<GlobalSkillList>>,
}

impl SkillProvider for WsSkillProvider {
    fn skill_registry(&self) -> &Arc<RwLock<GlobalSkillList>> {
        &self.registry
    }
}

struct WsKnowledgeAccess {
    user_id: String,
    km: Arc<KnowledgeManager>,
}

impl KnowledgeAccess for WsKnowledgeAccess {
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn knowledge_manager(&self) -> &Arc<KnowledgeManager> {
        &self.km
    }
}
