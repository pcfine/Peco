// ============================================================================
// 窄 Trait 接口 — 替代 Arc<WorkSpace> 注入
// ============================================================================

use std::sync::{Arc, RwLock};

use crate::agent::{Agent, AgentError};
use crate::knowledge::KnowledgeManager;
use crate::skills::SkillRegister;

// ============================================================================
// AgentLoader — DelegateSubAgent / RunParallelSubAgents 需要
// ============================================================================

pub trait AgentLoader: Send + Sync {
    fn load_agent(&self, name: &str) -> Result<Arc<Agent>, AgentError>;
    fn list_agent_names(&self) -> Vec<String>;
}

// ============================================================================
// SkillProvider — ReadSkill 需要
// ============================================================================

pub trait SkillProvider: Send + Sync {
    fn skill_registry(&self) -> &Arc<RwLock<SkillRegister>>;
}

// ============================================================================
// KnowledgeAccess — 5 个知识工具需要
// ============================================================================

pub trait KnowledgeAccess: Send + Sync {
    fn user_id(&self) -> &str;
    fn knowledge_manager(&self) -> &Arc<KnowledgeManager>;
}

// ============================================================================
// ToolDependencies — 工具构造依赖集合（owned Arcs）
// ============================================================================

pub struct ToolDependencies {
    pub agent_loader: Arc<dyn AgentLoader>,
    pub skill_provider: Arc<dyn SkillProvider>,
    pub knowledge_access: Arc<dyn KnowledgeAccess>,
}

impl Clone for ToolDependencies {
    fn clone(&self) -> Self {
        Self {
            agent_loader: self.agent_loader.clone(),
            skill_provider: self.skill_provider.clone(),
            knowledge_access: self.knowledge_access.clone(),
        }
    }
}
