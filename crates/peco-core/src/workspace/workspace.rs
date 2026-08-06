// ============================================================================
// WorkSpace — 用户隔离的核心抽象
// ============================================================================

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agent::AgentManager;
use crate::config::{McpConfig, SystemConfig, UserConfig};
use crate::knowledge::KnowledgeManager;
use crate::mcp::McpConfigStore;
use crate::skills::SkillRegister;
use crate::workflow::{WorkflowAccess, WorkflowManager};

use super::error::WorkspaceError;
use crate::tools::{AgentAccess, KnowledgeAccess, SkillProvider, ToolExecutor, ToolRegister};

// ============================================================================
// TemplateInitReport
// ============================================================================

/// 模板初始化报告 — 幂等操作的结果摘要。
///
/// 单个 agent/KB 安装失败不会阻塞其他项，
/// 错误收集到 [`errors`](TemplateInitReport::errors) 字段中。
#[derive(Debug, Default)]
pub struct TemplateInitReport {
    /// 安装了哪些 Agent（名称列表）
    pub agents_installed: Vec<String>,
    /// 跳过了哪些 Agent（已存在）
    pub agents_skipped: Vec<String>,
    /// 创建了哪些知识库
    pub kbs_created: Vec<String>,
    /// 跳过了哪些知识库（已存在）
    pub kbs_skipped: Vec<String>,
    /// 初始化过程中的错误（非致命）：(名称, 错误描述)
    pub errors: Vec<(String, String)>,
}

// ============================================================================
// WorkSpace
// ============================================================================

pub struct WorkSpace {
    user_id: String,
    root: PathBuf,
    config: UserConfig,
    skill_registry: Arc<SkillRegister>,
    knowledge_manager: Arc<KnowledgeManager>,
    agent_manager: Arc<AgentManager>,
    workflow_manager: Arc<WorkflowManager>,
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

        for subdir in &["skills", "knowledge", "agents", "workflows"] {
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
        let skill_registry = match SkillRegister::new(user_skills_dir.clone()) {
            Ok(registry) => Arc::new(registry),
            Err(e) => {
                tracing::warn!(error = %e, "Failed to scan user skills, using empty registry");
                Arc::new(SkillRegister::empty())
            }
        };

        let kb_dir = root.join("knowledge");
        let knowledge_manager = Arc::new(KnowledgeManager::new(kb_dir));

        let agents_dir = root.join("agents");
        let mcp_config_store = McpConfigStore::new(config.mcp.clone());
        let agent_manager = Arc::new(AgentManager::new(
            agents_dir,
            user_id.clone(),
            config.clone(),
            mcp_config_store,
            skill_registry.clone(),
            knowledge_manager.clone(),
        ));
        if let Err(e) = agent_manager.init() {
            tracing::warn!(error = %e, "Failed to scan agent metadata");
        }

        let workflows_dir = root.join("workflows");
        let workflow_manager = Arc::new(WorkflowManager::new(workflows_dir));
        if let Err(e) = workflow_manager.init() {
            tracing::warn!(error = %e, "Failed to scan workflow metadata");
        }

        Ok(Self {
            user_id,
            root,
            config,
            skill_registry,
            knowledge_manager,
            agent_manager,
            workflow_manager,
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
    pub fn skill_registry(&self) -> &Arc<SkillRegister> {
        &self.skill_registry
    }
    pub fn knowledge_manager(&self) -> &Arc<KnowledgeManager> {
        &self.knowledge_manager
    }
    pub fn agent_manager(&self) -> &Arc<AgentManager> {
        &self.agent_manager
    }
    pub fn workflow_manager(&self) -> &Arc<WorkflowManager> {
        &self.workflow_manager
    }

    // ── 热重载 ───────────────────────────────────────────────────

    /// 重新加载 MCP 配置（从 workspace 的 `mcpconfig.json`）。
    ///
    /// 仅影响后续新加载的 Agent；已缓存的 Agent 保持原有 MCP 连接。
    /// 注意：此方法不更新 [`config()`](Self::config) 返回值中的 MCP 配置。
    pub fn reload_mcp_config(&self, system_mcp: &McpConfig) -> usize {
        self.agent_manager.reload_mcp_config(&self.root, system_mcp)
    }

    /// 重新加载单个 Agent 的缓存（Tier-2 失效 + Tier-1 刷新）。
    ///
    /// 适用于 agent.md 文件被外部修改（如 IDE 编辑）后的场景。
    /// 下次 [`AgentManager::load_cached`] 调用将重新解析 agent.md。
    pub fn reload_agent(&self, name: &str) {
        self.agent_manager.refresh_one(name);
    }

    /// 重新扫描所有 Agent（刷新 Tier-1 元数据）。
    pub fn reload_agents(&self) -> Result<usize, crate::agent::AgentError> {
        self.agent_manager.rescan()
    }

    /// 重新加载单个 Skill（Tier-2 失效 + Tier-1 刷新）。
    ///
    /// 适用于 SKILL.md 文件被修改后的场景。
    /// 如果 Skill 目录已被删除，则从注册表中完全移除。
    pub fn reload_skill(&self, name: &str) {
        self.skill_registry.refresh_one(name);
    }

    /// 从 SkillRegister 缓存中移除一个 Skill（不触碰文件系统）。
    ///
    /// 适用于 Skill 目录已被外部删除的场景。
    pub fn remove_skill(&self, name: &str) {
        self.skill_registry.remove_one(name);
    }

    /// 重新扫描所有 Skill（刷新 Tier-1 + 清理过期的 Tier-2 条目）。
    pub fn reload_skills(&self) -> usize {
        self.skill_registry.rescan()
    }

    /// 重新加载单个 Workflow（Tier-2 失效 + 重新解析）。
    pub fn reload_workflow(
        &self,
        name: &str,
    ) -> Result<crate::workflow::WorkflowDefinition, crate::workflow::WorkflowError> {
        self.workflow_manager.reload(name)
    }

    /// 重新扫描所有 Workflow（刷新 Tier-1 元数据）。
    pub fn reload_workflows(&self) -> Result<usize, crate::workflow::WorkflowError> {
        self.workflow_manager.rescan()
    }

    /// 重新加载知识库管理器（拆毁并重建底层 KnowledgeBaseManager）。
    ///
    /// 适用于 kb_config.json 变更后需要重新发现知识库的场景。
    pub async fn reload_knowledge(&self) -> Result<(), crate::knowledge::KnowledgeModuleError> {
        self.knowledge_manager.reload().await
    }

    /// 增量同步单个知识库（扫描 docs/ 目录，对比哈希，更新数据库）。
    pub async fn sync_knowledge(
        &self,
        kb_name: &str,
    ) -> Result<crate::knowledge::SyncReport, crate::knowledge::KnowledgeModuleError> {
        self.knowledge_manager.sync_kb(kb_name).await
    }

    // ── Tool 组装 ────────────────────────────────────────────────────

    pub fn build_tool_executor(self: &Arc<Self>, tool_names: &[String]) -> Arc<dyn ToolExecutor> {
        let mut deps = self.agent_manager.build_deps();
        deps.workflow_access = Some(self.clone() as Arc<dyn WorkflowAccess>);
        ToolRegister::build(tool_names, &deps)
    }

    // ── 路径辅助 ─────────────────────────────────────────────────────

    pub fn agents_dir(&self) -> PathBuf {
        self.root.join("agents")
    }
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    // ── 模板初始化 ──────────────────────────────────────────────────

    /// 从模板目录初始化 workspace。
    ///
    /// 幂等操作：已存在的 agent 和 KB 不会被覆盖。
    ///
    /// 流程：
    /// 1. 扫描 `template_dir/agents/*/agent.md`
    ///    → 对于 workspace 中尚不存在的 agent，复制 `agent.md` 到 `agents/{name}/agent.md`
    /// 2. 扫描 `template_dir/knowledge/*/kb_config.json`
    ///    → 对于 workspace 中尚不存在的 KB，读取配置 → `KnowledgeManager::create_kb()`
    /// 3. 不处理 skills/、providers.toml、config.toml（非模板关注范围）
    ///
    /// 错误处理策略：单个 agent/KB 安装失败不影响其他项，错误收集到 `report.errors`，
    /// 方法总是返回 Ok(report)。仅当模板目录本身无法读取时才返回 Err。
    ///
    /// I/O 说明：模板目录通常只含少量小文件（若干 agent.md + kb_config.json），
    /// 初始化发生在启动阶段，不在热路径上，因此模板文件的读取使用同步 I/O。
    pub async fn init_from_template(
        &self,
        template_dir: &Path,
    ) -> Result<TemplateInitReport, WorkspaceError> {
        let mut report = TemplateInitReport::default();

        // ── 1. 安装 Agent ──────────────────────────────────────────
        let template_agents_dir = template_dir.join("agents");
        if template_agents_dir.exists()
            && let Ok(entries) = std::fs::read_dir(&template_agents_dir)
        {
            for entry in entries.flatten() {
                if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                let src_md = entry.path().join("agent.md");
                let dst_md = self.agent_manager().md_path(&name);

                if dst_md.exists() {
                    report.agents_skipped.push(name);
                    continue;
                }

                if src_md.exists() {
                    match std::fs::read_to_string(&src_md) {
                        Ok(content) => match self.agent_manager().save(&name, &content) {
                            Ok(()) => report.agents_installed.push(name),
                            Err(e) => report
                                .errors
                                .push((name, format!("保存 agent.md 失败: {e}"))),
                        },
                        Err(e) => report
                            .errors
                            .push((name, format!("读取模板 agent.md 失败: {e}"))),
                    }
                }
            }
        }

        // ── 2. 创建知识库 ──────────────────────────────────────────
        let template_kb_dir = template_dir.join("knowledge");
        if template_kb_dir.exists() {
            // 确保 KnowledgeManager 已初始化
            if let Err(e) = self.knowledge_manager().ensure_loaded().await {
                report.errors.push((
                    "knowledge_manager".into(),
                    format!("ensure_loaded 失败: {e}"),
                ));
                // 无法继续创建 KB，但 agent 部分已完成
                return Ok(report);
            }

            let existing = match self.knowledge_manager().list_kbs().await {
                Ok(list) => list,
                Err(e) => {
                    report
                        .errors
                        .push(("knowledge_manager".into(), format!("list_kbs 失败: {e}")));
                    return Ok(report);
                }
            };
            let existing_names: Vec<&str> = existing.iter().map(|i| i.name.as_str()).collect();

            if let Ok(entries) = std::fs::read_dir(&template_kb_dir) {
                for entry in entries.flatten() {
                    if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    let kb_config_path = entry.path().join("kb_config.json");
                    if !kb_config_path.exists() {
                        continue;
                    }

                    let kb_name = entry.file_name().to_string_lossy().to_string();
                    if existing_names.contains(&kb_name.as_str()) {
                        report.kbs_skipped.push(kb_name);
                        continue;
                    }

                    // 读取并解析配置
                    let config = match std::fs::read_to_string(&kb_config_path) {
                        Ok(json) => match serde_json::from_str::<knowledge_base::KbConfig>(&json) {
                            Ok(cfg) => cfg,
                            Err(e) => {
                                report
                                    .errors
                                    .push((kb_name, format!("解析 kb_config.json 失败: {e}")));
                                continue;
                            }
                        },
                        Err(e) => {
                            report
                                .errors
                                .push((kb_name, format!("读取模板 kb_config.json 失败: {e}")));
                            continue;
                        }
                    };

                    // 校验：config.name 必须与目录名一致
                    if config.name != kb_name {
                        report.errors.push((
                            kb_name.clone(),
                            format!(
                                "kb_config.json 中 name=\"{}\" 与目录名 \"{}\" 不一致",
                                config.name, kb_name
                            ),
                        ));
                        continue;
                    }

                    match self.knowledge_manager().create_kb(config).await {
                        Ok(_) => report.kbs_created.push(kb_name),
                        Err(e) => report
                            .errors
                            .push((kb_name, format!("create_kb 失败: {e}"))),
                    }
                }
            }
        }

        Ok(report)
    }
}

// ============================================================================
// Narrow trait implementations — WorkSpace 作为编排者
// ============================================================================

impl AgentAccess for WorkSpace {
    fn load_agent(&self, name: &str) -> Result<Arc<crate::agent::Agent>, crate::agent::AgentError> {
        self.agent_manager.load_agent(name)
    }

    fn list_agent_names(&self) -> Vec<String> {
        self.agent_manager.list_names()
    }

    fn save_agent(&self, name: &str, content: &str) -> Result<(), String> {
        self.agent_manager
            .save(name, content)
            .map_err(|e| e.to_string())
    }
}

impl SkillProvider for WorkSpace {
    fn skill_registry(&self) -> &Arc<SkillRegister> {
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

impl WorkflowAccess for WorkSpace {
    fn load_workflow(
        &self,
        name: &str,
    ) -> Result<crate::workflow::WorkflowDefinition, crate::workflow::WorkflowError> {
        self.workflow_manager.load(name)
    }
    fn list_workflow_names(&self) -> Vec<String> {
        self.workflow_manager.list_names()
    }
    fn reload_workflow(
        &self,
        name: &str,
    ) -> Result<crate::workflow::WorkflowDefinition, crate::workflow::WorkflowError> {
        self.workflow_manager.reload(name)
    }
}
