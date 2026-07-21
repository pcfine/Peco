// ============================================================================
// WorkSpace — 用户隔离的核心抽象
// ============================================================================

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::agent::AgentManager;
use crate::config::{SystemConfig, UserConfig};
use crate::knowledge::KnowledgeManager;
use crate::skills::SkillRegister;

use super::deps::{AgentLoader, KnowledgeAccess, SkillProvider};
use super::error::WorkspaceError;
use super::tool_register::ToolRegister;
use crate::tools::ToolExecutor;

// ============================================================================
// WorkSpace
// ============================================================================

pub struct WorkSpace {
    user_id: String,
    root: PathBuf,
    config: UserConfig,
    skill_registry: Arc<RwLock<SkillRegister>>,
    knowledge_manager: Arc<KnowledgeManager>,
    agent_manager: Arc<AgentManager>,
}

impl WorkSpace {
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
        let mut registry = SkillRegister::new(user_skills_dir.clone());
        if user_skills_dir.exists()
            && let Err(e) = registry.init()
        {
            tracing::warn!(error = %e, "Failed to scan user skills");
        }
        let skill_registry = Arc::new(RwLock::new(registry));

        let kb_dir = root.join("knowledge");
        let knowledge_manager = Arc::new(KnowledgeManager::new(kb_dir));

        let agents_dir = root.join("agents");
        let agent_manager = Arc::new(AgentManager::new(
            agents_dir,
            user_id.clone(),
            config.clone(),
            skill_registry.clone(),
            knowledge_manager.clone(),
        ));
        if let Err(e) = agent_manager.init() {
            tracing::warn!(error = %e, "Failed to scan agent metadata");
        }

        Ok(Self {
            user_id,
            root,
            config,
            skill_registry,
            knowledge_manager,
            agent_manager,
        })
    }

    // ── 管理器访问器 ─────────────────────────────────────────────────

    pub fn user_id(&self) -> &str {
        &self.user_id
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn config(&self) -> &UserConfig {
        &self.config
    }
    pub fn skill_registry(&self) -> &Arc<RwLock<SkillRegister>> {
        &self.skill_registry
    }
    pub fn knowledge_manager(&self) -> &Arc<KnowledgeManager> {
        &self.knowledge_manager
    }
    pub fn agent_manager(&self) -> &Arc<AgentManager> {
        &self.agent_manager
    }

    // ── Tool 组装 ────────────────────────────────────────────────────

    pub fn build_tool_executor(self: &Arc<Self>, tool_names: &[String]) -> Arc<dyn ToolExecutor> {
        let deps = self.agent_manager.build_deps();
        ToolRegister::build(tool_names, &deps)
    }

    // ── 路径辅助 ─────────────────────────────────────────────────────

    pub fn agents_dir(&self) -> PathBuf {
        self.root.join("agents")
    }
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }
}

// ============================================================================
// Narrow trait implementations — WorkSpace 作为编排者
// ============================================================================

impl AgentLoader for WorkSpace {
    fn load_agent(
        &self,
        name: &str,
    ) -> Result<Arc<crate::agent::Agent>, crate::agent::AgentError> {
        self.agent_manager.load_agent(name)
    }

    fn list_agent_names(&self) -> Vec<String> {
        self.agent_manager.list_names()
    }
}

impl SkillProvider for WorkSpace {
    fn skill_registry(&self) -> &Arc<RwLock<SkillRegister>> {
        &self.skill_registry
    }
}

impl KnowledgeAccess for WorkSpace {
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn knowledge_manager(&self) -> &Arc<KnowledgeManager> {
        &self.knowledge_manager
    }
}
