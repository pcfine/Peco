// ============================================================================
// 窄 Trait 接口 — 替代 Arc<WorkSpace> 注入
// ============================================================================

use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::{Agent, AgentError};
use crate::config::{McpServerConfig, TransportType};
use crate::knowledge::KnowledgeManager;
use crate::search::SearchBackend;
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
// MemoryAuditAccess — 记忆删除审计（删除工具经窄接口写审计，peco-core 不感知 SQLite）
// ============================================================================

/// 一条待写入审计的删除记录（写入侧最小字段集）。
///
/// `deleted_at` 为 ISO 8601 时刻字符串，由调用方填充。
/// 回滚所需的扩展字段（status / restored_at / restored_doc_id）位于
/// peco-server 的 SQLite DAO 层，不进入本窄接口。
#[derive(Debug, Clone)]
pub struct MemoryAuditEntry {
    pub user_id: String,
    pub kb_name: String,
    pub doc_id: String,
    pub title: String,
    /// 完整原文 — 回滚重放 `add_text` 时使用。
    pub content: String,
    /// 来源标签，如 ppa_profile / ppa_semantic / ppa_episodic。
    pub source: String,
    /// 删除原因：manual_organize / consolidation_dedup / consolidation_ttl /
    /// consolidation_distill。
    pub reason: String,
    /// 执行者：'agent:@memory' | 'worker' | 'user'。
    pub deleted_by: String,
    /// ISO 8601 时刻。
    pub deleted_at: String,
}

/// 记忆删除审计访问接口。
///
/// fail-closed 约定：[`ToolDependencies::memory_audit`] 为 `None` 时
/// 删除工具必须拒绝执行（审计存储不可用即不放行删除）。
/// 实现仅在审计存储确实可用时注入。
#[async_trait::async_trait]
pub trait MemoryAuditAccess: Send + Sync {
    /// 写入一条 pending 审计，返回审计行 id。
    async fn record_pending(&self, entry: MemoryAuditEntry) -> Result<i64, String>;
    /// 删除成功 — pending → done。
    async fn mark_done(&self, id: i64) -> Result<(), String>;
    /// 删除失败 — pending → cancelled。
    async fn mark_cancelled(&self, id: i64) -> Result<(), String>;
    /// 读取单条审计（回滚重放用）。
    async fn get(&self, id: i64) -> Result<Option<MemoryAuditEntry>, String>;
}

/// 什么都不做的审计实现 — 仅供测试显式注入以放行删除。
///
/// `record_pending` 即刻丢弃记录，回滚将无从谈起；
/// 生产路径不要注入本实现 — 保持 `memory_audit = None` 让删除工具
/// 运行时拒绝执行（fail-closed）。仅在 `cfg(test)` 下存在，
/// 依赖方无法把 noop 审计带进生产构建。
#[cfg(test)]
pub struct NoopMemoryAudit;

#[cfg(test)]
#[async_trait::async_trait]
impl MemoryAuditAccess for NoopMemoryAudit {
    async fn record_pending(&self, _entry: MemoryAuditEntry) -> Result<i64, String> {
        Ok(0)
    }

    async fn mark_done(&self, _id: i64) -> Result<(), String> {
        Ok(())
    }

    async fn mark_cancelled(&self, _id: i64) -> Result<(), String> {
        Ok(())
    }

    async fn get(&self, _id: i64) -> Result<Option<MemoryAuditEntry>, String> {
        Ok(None)
    }
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
    /// Workflow 支持，Optional 以保持向后兼容性。
    pub workflow_access: Option<Arc<dyn WorkflowAccess>>,
    /// MCP 配置管理支持，Optional 以保持向后兼容性。
    pub mcp_access: Option<Arc<dyn McpAccess>>,
    /// Workflow 持久化支持。Optional — None 时使用 NullWorkflowPersister。
    pub workflow_persister: Option<Arc<dyn WorkflowPersister>>,
    /// 工作空间根目录。用于 shell 工具的默认 cwd 与 show_workspace 的 root 输出。
    /// Optional — None 时行为与历史版本逐字节一致（examples / 非 WorkSpace 路径）。
    pub workspace_root: Option<PathBuf>,
    /// web 搜索后端（来自 providers.toml 的 `[web_search]` 段）。
    /// Optional — None 时 web_search 工具 warn + skip，保持向后兼容。
    pub web_search: Option<Arc<SearchBackend>>,
    /// 记忆删除审计支持。Optional — None 时删除工具运行时拒绝（fail-closed），
    /// 与其他 Optional 依赖的 warn + skip 语义不同：工具仍注册，只是执行被拒。
    pub memory_audit: Option<Arc<dyn MemoryAuditAccess>>,
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
            web_search: self.web_search.clone(),
            memory_audit: self.memory_audit.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> MemoryAuditEntry {
        MemoryAuditEntry {
            user_id: "test-user".into(),
            kb_name: "@private_memory".into(),
            doc_id: "abcd1234".into(),
            title: "memory_1".into(),
            content: "用户偏好 Rust".into(),
            source: "ppa_semantic".into(),
            reason: "manual_organize".into(),
            deleted_by: "agent:@memory".into(),
            deleted_at: "2026-09-10T00:00:00+00:00".into(),
        }
    }

    /// NoopMemoryAudit 是测试中显式放行删除的注入项：
    /// 接受全部审计调用，但不留存任何可回读的记录。
    #[tokio::test]
    async fn noop_memory_audit_accepts_calls_and_retains_nothing() {
        let audit = NoopMemoryAudit;

        let id = audit.record_pending(sample_entry()).await.unwrap();
        audit.mark_done(id).await.unwrap();
        audit.mark_cancelled(id).await.unwrap();

        assert!(audit.get(id).await.unwrap().is_none());
    }
}
