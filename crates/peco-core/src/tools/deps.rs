// ============================================================================
// 窄 Trait 接口 — 替代 Arc<WorkSpace> 注入
// ============================================================================

use std::sync::Arc;

use crate::agent::{Agent, AgentError};
use crate::knowledge::KnowledgeManager;
use crate::skills::SkillRegister;

// ============================================================================
// AgentAccess — 所有 Agent 相关工具需要（加载、创建、列表）
// ============================================================================

pub trait AgentAccess: Send + Sync {
    fn load_agent(&self, name: &str) -> Result<Arc<Agent>, AgentError>;
    fn list_agent_names(&self) -> Vec<String>;
    /// 保存 agent.md 文件。若 agent 已存在则覆盖。
    /// `content` 必须是完整的 agent.md 内容（YAML frontmatter + Markdown body）。
    fn save_agent(&self, name: &str, content: &str) -> Result<(), String>;
}

// ============================================================================
// SkillProvider — ReadSkill 需要
// ============================================================================

pub trait SkillProvider: Send + Sync {
    fn skill_registry(&self) -> &Arc<SkillRegister>;
}

// ============================================================================
// KnowledgeAccess — 知识工具需要
// ============================================================================

pub trait KnowledgeAccess: Send + Sync {
    fn user_id(&self) -> &str;
    fn knowledge_manager(&self) -> &Arc<KnowledgeManager>;
}

// ============================================================================
// ToolDependencies — 工具构造依赖集合（owned Arcs）
// ============================================================================

pub struct ToolDependencies {
    pub agent_access: Arc<dyn AgentAccess>,
    pub skill_provider: Arc<dyn SkillProvider>,
    pub knowledge_access: Arc<dyn KnowledgeAccess>,
    /// 来自 agent.md `knowledge_bases` 的 KB 白名单。空 = 无权访问任何 KB。
    pub allowed_kbs: Vec<String>,
}

impl Clone for ToolDependencies {
    fn clone(&self) -> Self {
        Self {
            agent_access: self.agent_access.clone(),
            skill_provider: self.skill_provider.clone(),
            knowledge_access: self.knowledge_access.clone(),
            allowed_kbs: self.allowed_kbs.clone(),
        }
    }
}
