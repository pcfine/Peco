// ============================================================================
// 窄 Trait 接口 — 替代 Arc<WorkSpace> 注入
// ============================================================================

use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::{Agent, AgentError};
use crate::config::{McpServerConfig, TransportType};
use crate::knowledge::KnowledgeManager;
use crate::skills::SkillRegister;
use crate::workflow::WorkflowAccess;
use crate::workflow::persistence::WorkflowPersister;

// ============================================================================
// AgentAccess — 所有 Agent 相关工具需要（加载、创建、列表）
// ============================================================================

pub trait AgentAccess: Send + Sync {
    fn load_agent(&self, name: &str) -> Result<Arc<Agent>, AgentError>;
    fn list_agent_names(&self) -> Vec<String>;
    /// 保存 agent.md 文件。若 agent 已存在则覆盖。
    /// `content` 必须是完整的 agent.md 内容（YAML frontmatter + Markdown body）。
    fn save_agent(&self, name: &str, content: &str) -> Result<(), String>;
    /// 读取 agent.md 原始内容（YAML frontmatter + Markdown body）。
    fn read_agent(&self, name: &str) -> Result<String, String>;
    /// 删除 Agent 目录（不可逆操作）。
    fn delete_agent(&self, name: &str) -> Result<(), String>;
}

// ============================================================================
// SkillProvider — ReadSkill 需要
// ============================================================================

pub trait SkillProvider: Send + Sync {
    fn skill_registry(&self) -> &Arc<SkillRegister>;
    /// 创建或更新 SKILL.md 文件。
    /// `content` 必须是完整的 SKILL.md 内容（YAML frontmatter + Markdown body）。
    fn save_skill(&self, name: &str, content: &str) -> Result<(), String>;
    /// 删除 Skill 目录（不可逆操作）。
    fn delete_skill(&self, name: &str) -> Result<(), String>;
}

// ============================================================================
// KnowledgeAccess — 知识工具需要
// ============================================================================

pub trait KnowledgeAccess: Send + Sync {
    fn user_id(&self) -> &str;
    fn knowledge_manager(&self) -> &Arc<KnowledgeManager>;
}

// ============================================================================
// McpServerInfo — MCP Server 摘要信息
// ============================================================================

/// MCP Server 摘要信息（供 list_mcp_servers 返回）。
#[derive(Debug, Clone, Serialize)]
pub struct McpServerInfo {
    pub name: String,
    pub transport: TransportType,
    pub enabled: bool,
    pub url: Option<String>,
    pub command: Option<String>,
}

// ============================================================================
// McpAccess — MCP 配置管理接口
// ============================================================================

pub trait McpAccess: Send + Sync {
    /// 列出所有已配置的 MCP Server（摘要信息）。
    fn list_mcp_servers(&self) -> Vec<McpServerInfo>;
    /// 添加或更新一个 MCP Server 配置（单 server 粒度合并）。
    fn add_mcp_server(&self, name: &str, config: McpServerConfig) -> Result<(), String>;
    /// 从配置中移除指定的 MCP Server（不可逆）。
    fn remove_mcp_server(&self, name: &str) -> Result<(), String>;
    /// 获取指定 MCP Server 的完整配置。
    /// 返回 None 表示该 Server 未在配置中注册。
    fn get_mcp_server_config(&self, name: &str) -> Option<McpServerConfig>;
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
    /// Workflow 支持（Phase 2 新增）。Optional 以保持向后兼容性。
    pub workflow_access: Option<Arc<dyn WorkflowAccess>>,
    /// MCP 配置管理支持。Optional 以保持向后兼容性。
    pub mcp_access: Option<Arc<dyn McpAccess>>,
    /// Workflow 持久化支持。Optional — None 时使用 NullWorkflowPersister。
    pub workflow_persister: Option<Arc<dyn WorkflowPersister>>,
    /// 工作空间根目录。用于 shell 工具的默认 cwd 与 show_workspace 的 root 输出。
    /// Optional — None 时行为与历史版本逐字节一致（examples / 非 WorkSpace 路径）。
    pub workspace_root: Option<PathBuf>,
}

impl Clone for ToolDependencies {
    fn clone(&self) -> Self {
        Self {
            agent_access: self.agent_access.clone(),
            skill_provider: self.skill_provider.clone(),
            knowledge_access: self.knowledge_access.clone(),
            allowed_kbs: self.allowed_kbs.clone(),
            workflow_access: self.workflow_access.clone(),
            mcp_access: self.mcp_access.clone(),
            workflow_persister: self.workflow_persister.clone(),
            workspace_root: self.workspace_root.clone(),
        }
    }
}
