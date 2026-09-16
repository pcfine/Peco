// ============================================================================
// WorkSpace — 用户隔离的核心抽象
// ============================================================================

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::agent::AgentManager;
use crate::config::{McpConfig, SystemConfig, UserConfig};
use crate::knowledge::KnowledgeManager;
use crate::mcp::McpConfigStore;
use crate::skills::SkillRegister;
use crate::workflow::{WorkflowAccess, WorkflowManager};

use super::error::WorkspaceError;
use crate::tools::{
    AgentAccess, KnowledgeAccess, McpAccess, McpServerInfo, MemoryAuditAccess, SkillProvider,
    ToolExecutor, ToolRegister,
};

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
    /// 因模板版本更高而备份覆盖的 Agent（名称列表）
    pub agents_updated: Vec<String>,
    /// 创建了哪些知识库
    pub kbs_created: Vec<String>,
    /// 跳过了哪些知识库（已存在）
    pub kbs_skipped: Vec<String>,
    /// 初始化过程中的错误（非致命）：(名称, 错误描述)
    pub errors: Vec<(String, String)>,
}

/// 解析 agent.md frontmatter 中的模板版本号。
///
/// 无字段、解析失败或非模板文件一律视为 0 — 保证存量用户
/// 能通过版本比对拿到模板升级内容。
fn agent_template_version(content: &str) -> u32 {
    crate::agent::agent_config::parse_agent_md(content)
        .ok()
        .and_then(|(profile, _)| profile.template_version)
        .unwrap_or(0)
}

// ============================================================================
// WorkSpace
// ============================================================================

pub struct WorkSpace {
    user_id: String,
    root: PathBuf,
    /// 系统级配置快照 — 用户 providers.toml 与之深递归合并。
    /// 进程内不变，但需保留以支持 [`WorkSpace::reload_providers`] 重新合并。
    system_config: SystemConfig,
    /// 当前生效的用户配置（providers + mcp），可被热重载整体替换。
    config: RwLock<UserConfig>,
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
            system_config: system_config.clone(),
            config: RwLock::new(config),
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
    /// 返回当前生效的用户配置快照。
    ///
    /// providers 可被 [`reload_providers`](Self::reload_providers) 整体替换，
    /// 故返回克隆而非引用。
    pub fn config(&self) -> UserConfig {
        self.config.read().unwrap().clone()
    }
    pub fn skill_registry(&self) -> &Arc<SkillRegister> {
        &self.skill_registry
    }

    // ── 依赖注入 ────────────────────────────────────────────────────

    /// 将 WorkSpace 自身注入 AgentManager，使其构建的 ToolDependencies
    /// 包含 workflow_access 和 mcp_access。
    ///
    /// 必须在 WorkSpace 被包装为 `Arc` 后调用。
    pub fn inject_deps(self: &Arc<Self>) {
        self.agent_manager
            .set_workflow_access(self.clone() as Arc<dyn WorkflowAccess>);
        self.agent_manager
            .set_mcp_access(self.clone() as Arc<dyn McpAccess>);
    }

    /// 注入记忆删除审计依赖（由 peco-server 层在 workspace 就绪后调用）。
    ///
    /// WorkSpace 自身没有审计存储，无法像 workflow/mcp 那样在 `inject_deps`
    /// 中自注入；不注入（默认 None）= 删除工具运行时拒绝执行（fail-closed）。
    pub fn set_memory_audit(&self, ma: Arc<dyn MemoryAuditAccess>) {
        self.agent_manager.set_memory_audit(ma);
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
        self.agent_manager.reload_mcp_config(system_mcp)
    }

    /// 原子替换整个 MCP 配置（校验 → 写盘 → 更新内存，在一次锁内完成）。
    ///
    /// 相比先写文件再 `reload_mcp_config()` 的两步模式，此方法保证磁盘与内存一致，
    /// 且写入失败时内存状态保持不变。
    pub fn replace_mcp_config(&self, new_config: McpConfig) -> Result<(), String> {
        self.agent_manager
            .mcp_config_store()
            .atomic_update(&self.root, |config| {
                *config = new_config;
                Ok(())
            })
    }

    /// 重新加载单个 Agent 的缓存（Tier-2 失效 + Tier-1 刷新）。
    ///
    /// 适用于 agent.md 文件被外部修改（如 IDE 编辑）后的场景。
    /// 下次 [`AgentManager::load_cached`] 调用将重新解析 agent.md。
    pub fn reload_agent(&self, name: &str) {
        self.agent_manager.refresh_one(name);
    }

    /// 重新加载 provider 配置（重新读盘 + 与系统配置深递归合并）。
    ///
    /// `providers.toml` 不属于任何单例管理器，故先在 workspace 层完成合并，
    /// 再把结果推给 [`AgentManager`]（它按 provider 快照构建 Agent）。
    /// 返回被失效的已缓存 Agent 数量 — 这些 Agent 下次加载时会用新 provider 重建。
    ///
    /// 与 [`reload_mcp_config`](Self::reload_mcp_config) 的差别：MCP 只影响
    /// 后续新加载的 Agent，provider 是**已建实例内的凭据**，必须主动失效缓存，
    /// 否则改完 api_key 仍走旧凭据。
    pub fn reload_providers(&self) -> Result<usize, crate::config::ConfigError> {
        let new_config = UserConfig::load(&self.system_config, &self.root)?;
        let invalidated = self.agent_manager.reload_providers(new_config.clone());
        *self.config.write().unwrap() = new_config;
        Ok(invalidated)
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
        deps.mcp_access = Some(self.clone() as Arc<dyn McpAccess>);
        // web_search 后端已由 AgentManager 构造期缓存，随 build_deps() 注入；
        // workflow_persister 由 peco-server 层注入；此处保持 None
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
    /// 幂等操作：已存在的 agent 和 KB 不会被覆盖。仅当模板 `agent.md`
    /// frontmatter 的 `template_version` 高于现存文件时（现存文件无版本号
    /// 视为 0），先将现存文件备份为 `agent.md.bak.{旧版本}` 再覆盖写入，
    /// 用于存量用户的模板升级；不做内容合并，用户手写改动只能从备份找回。
    ///
    /// 流程：
    /// 1. 扫描 `template_dir/agents/*/agent.md`
    ///    → 对于 workspace 中尚不存在的 agent，复制 `agent.md` 到 `agents/{name}/agent.md`
    ///    → 已存在的 agent 按模板版本比对决定跳过或升级
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
                    // 版本比对迁移：模板版本更高 → 备份后覆盖；
                    // 版本相同或更高（用户自定义改版）→ 维持跳过
                    let existing_version = std::fs::read_to_string(&dst_md)
                        .map(|c| agent_template_version(&c))
                        .unwrap_or(0);
                    let template_version = std::fs::read_to_string(&src_md)
                        .map(|c| agent_template_version(&c))
                        .unwrap_or(0);
                    if template_version <= existing_version {
                        report.agents_skipped.push(name);
                        continue;
                    }

                    let backup = dst_md.with_extension(format!("md.bak.{existing_version}"));
                    if let Err(e) = std::fs::copy(&dst_md, &backup) {
                        report
                            .errors
                            .push((name.clone(), format!("备份现存 agent.md 失败: {e}")));
                        continue;
                    }

                    match std::fs::read_to_string(&src_md) {
                        Ok(content) => match self.agent_manager().save(&name, &content) {
                            Ok(()) => report.agents_updated.push(name),
                            Err(e) => report
                                .errors
                                .push((name, format!("覆盖写入 agent.md 失败: {e}"))),
                        },
                        Err(e) => report
                            .errors
                            .push((name, format!("读取模板 agent.md 失败: {e}"))),
                    }
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

    fn read_agent(&self, name: &str) -> Result<String, String> {
        let path = self.agent_manager.md_path(name);
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read agent '{name}': {e}"))
    }

    fn delete_agent(&self, name: &str) -> Result<(), String> {
        if name == "@assistant" {
            return Err(
                "Cannot delete @assistant — it is the meta-agent managing this workspace."
                    .to_string(),
            );
        }
        self.agent_manager.delete(name).map_err(|e| e.to_string())
    }
}

impl SkillProvider for WorkSpace {
    fn skill_registry(&self) -> &Arc<SkillRegister> {
        &self.skill_registry
    }

    fn save_skill(&self, name: &str, content: &str) -> Result<(), String> {
        self.skill_registry
            .save_skill(name, content)
            .map_err(|e| e.to_string())
    }

    fn delete_skill(&self, name: &str) -> Result<(), String> {
        self.skill_registry
            .delete_skill(name)
            .map_err(|e| e.to_string())
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
    fn list_workflow_meta(&self) -> Vec<crate::workflow::WorkflowMeta> {
        self.workflow_manager.list_meta()
    }
    fn reload_workflow(
        &self,
        name: &str,
    ) -> Result<crate::workflow::WorkflowDefinition, crate::workflow::WorkflowError> {
        self.workflow_manager.reload(name)
    }
    fn save_workflow(&self, name: &str, content: &str) -> Result<(), String> {
        match self.workflow_manager.create(name, content) {
            Ok(_) => Ok(()),
            Err(crate::workflow::WorkflowError::AlreadyExists(_)) => self
                .workflow_manager
                .update(name, content)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
    fn delete_workflow(&self, name: &str) -> Result<(), String> {
        self.workflow_manager
            .delete(name)
            .map_err(|e| e.to_string())
    }
}

impl McpAccess for WorkSpace {
    fn list_mcp_servers(&self) -> Vec<McpServerInfo> {
        let config = self.agent_manager.mcp_config_store().get();
        config
            .mcp_servers
            .iter()
            .map(|(name, srv)| McpServerInfo {
                name: name.clone(),
                transport: srv.transport.clone(),
                enabled: srv.enabled,
                url: srv.url.clone(),
                command: srv.command.clone(),
            })
            .collect()
    }

    fn add_mcp_server(
        &self,
        name: &str,
        server_config: crate::config::McpServerConfig,
    ) -> Result<(), String> {
        let name = name.to_string();
        self.agent_manager
            .mcp_config_store()
            .atomic_update(&self.root, |config| {
                config.mcp_servers.insert(name.clone(), server_config);
                Ok(())
            })
            .map_err(|e| format!("Failed to save MCP server '{name}': {e}"))
    }

    fn remove_mcp_server(&self, name: &str) -> Result<(), String> {
        let name = name.to_string();
        self.agent_manager
            .mcp_config_store()
            .atomic_update(&self.root, |config| {
                if config.mcp_servers.remove(&name).is_none() {
                    return Err(format!("MCP server '{name}' not found"));
                }
                Ok(())
            })
    }

    fn get_mcp_server_config(&self, name: &str) -> Option<crate::config::McpServerConfig> {
        let config = self.agent_manager.mcp_config_store().get();
        config.mcp_servers.get(name).cloned()
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{McpConfig, ProvidersConfig, SystemConfig};
    use std::collections::HashMap;

    /// 构造带可选版本号的 agent.md 文本。
    fn md(version: Option<u32>, body: &str) -> String {
        let version_line = version
            .map(|v| format!("template_version: {v}\n"))
            .unwrap_or_default();
        format!(
            "---\nagent:\n  name: \"@demo\"\n  description: \"demo agent\"\n{version_line}llm:\n  provider: \"deepseek\"\n---\n{body}"
        )
    }

    fn make_workspace(root: &std::path::Path) -> WorkSpace {
        let system_config = SystemConfig {
            providers: ProvidersConfig {
                default_provider: "deepseek".into(),
                providers: HashMap::new(),
                web_search: None,
            },
            mcp: McpConfig {
                mcp_servers: HashMap::new(),
                extra: HashMap::new(),
            },
            skills_root: root.join("skills"),
            knowledge_dir: root.join("knowledge"),
        };
        WorkSpace::open(root.join("workspace"), "test-user".into(), &system_config).unwrap()
    }

    /// 写出只含一个 agent 的模板目录，返回模板路径。
    fn make_template(root: &std::path::Path, content: &str) -> PathBuf {
        let template_dir = root.join("template");
        let agent_dir = template_dir.join("agents").join("@demo");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("agent.md"), content).unwrap();
        template_dir
    }

    fn installed_agent_dir(root: &std::path::Path) -> PathBuf {
        root.join("workspace").join("agents").join("@demo")
    }

    /// 存量无版本号（视为 0）→ 模板版本更高 → 备份 .bak.0 且覆盖为新内容；
    /// 二次 init 版本持平 → skipped（幂等）。
    #[tokio::test]
    async fn legacy_agent_upgraded_with_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = make_workspace(tmp.path());

        let agents_dir = installed_agent_dir(tmp.path());
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("agent.md"), md(None, "# 旧协议")).unwrap();

        let template = make_template(tmp.path(), &md(Some(2), "# 新协议 v2"));
        let report = ws.init_from_template(&template).await.unwrap();

        assert_eq!(report.agents_updated, vec!["@demo".to_string()]);
        assert!(report.agents_skipped.is_empty());
        let upgraded = std::fs::read_to_string(agents_dir.join("agent.md")).unwrap();
        assert!(upgraded.contains("# 新协议 v2"));
        let backup = std::fs::read_to_string(agents_dir.join("agent.md.bak.0")).unwrap();
        assert!(backup.contains("# 旧协议"));

        // 幂等：升级后再跑一次，版本持平 → skipped
        let report = ws.init_from_template(&template).await.unwrap();
        assert_eq!(report.agents_skipped, vec!["@demo".to_string()]);
        assert!(report.agents_updated.is_empty());
    }

    /// 现存版本与模板持平 → skipped，用户改动保留，不产生备份。
    #[tokio::test]
    async fn same_version_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = make_workspace(tmp.path());
        let agents_dir = installed_agent_dir(tmp.path());
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("agent.md"), md(Some(2), "# 用户手写内容")).unwrap();

        let template = make_template(tmp.path(), &md(Some(2), "# 新协议 v2"));
        let report = ws.init_from_template(&template).await.unwrap();

        assert_eq!(report.agents_skipped, vec!["@demo".to_string()]);
        assert!(report.agents_updated.is_empty());
        let content = std::fs::read_to_string(agents_dir.join("agent.md")).unwrap();
        assert!(content.contains("# 用户手写内容"));
        assert!(!agents_dir.join("agent.md.bak.2").exists());
    }

    /// 现存版本比模板更高（用户侧已改版）→ skipped。
    #[tokio::test]
    async fn lower_template_version_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = make_workspace(tmp.path());
        let agents_dir = installed_agent_dir(tmp.path());
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("agent.md"), md(Some(3), "# 用户改版")).unwrap();

        let template = make_template(tmp.path(), &md(Some(2), "# 新协议 v2"));
        let report = ws.init_from_template(&template).await.unwrap();
        assert_eq!(report.agents_skipped, vec!["@demo".to_string()]);
        assert!(report.agents_updated.is_empty());
    }

    /// 新用户首次安装 → installed，不产生备份。
    #[tokio::test]
    async fn fresh_install_reported_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = make_workspace(tmp.path());
        let template = make_template(tmp.path(), &md(Some(2), "# 新协议 v2"));
        let report = ws.init_from_template(&template).await.unwrap();

        assert_eq!(report.agents_installed, vec!["@demo".to_string()]);
        let agents_dir = installed_agent_dir(tmp.path());
        assert!(agents_dir.join("agent.md").exists());
        assert!(!agents_dir.join("agent.md.bak.0").exists());
    }

    /// 写出 providers.toml，其中 deepseek 的 api_key 为字面量 `key`。
    fn write_providers(root: &std::path::Path, key: &str) {
        let dir = root.join("workspace");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("providers.toml");
        let content = format!(
            "default_provider = \"deepseek\"\n\n[providers.deepseek]\ntype = \"deepseek\"\napi_key = \"{key}\"\nbase_url = \"https://api.deepseek.com\"\n\n[providers.deepseek.default]\nmodel = \"deepseek-v4-flash\"\n"
        );
        std::fs::write(&path, content).unwrap();
    }

    /// 修改 api_key 后 `reload_providers()` 必须让已缓存 Agent 重建 —— 这正是
    /// WebUI 保存后"密钥改了却不生效"的修复点：Agent 内嵌 provider 实例，
    /// 只换配置快照而不失效缓存等于没改。
    #[tokio::test]
    async fn reload_providers_invalidates_cached_agent() {
        let tmp = tempfile::tempdir().unwrap();
        write_providers(tmp.path(), "sk-old");
        let ws = make_workspace(tmp.path());

        let agents_dir = installed_agent_dir(tmp.path());
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("agent.md"), md(None, "# 对话体")).unwrap();

        let ws = Arc::new(ws);
        let before = ws.agent_manager().load_cached("@demo").unwrap();
        assert_eq!(
            ws.config()
                .provider_entry(Some("deepseek"))
                .and_then(|e| e.api_key.clone())
                .as_deref(),
            Some("sk-old")
        );

        write_providers(tmp.path(), "sk-new");
        let invalidated = ws.reload_providers().unwrap();

        // 缓存被清空 + 生效配置已更新
        assert_eq!(invalidated, 1, "已缓存 Agent 应被全部失效");
        assert_eq!(
            ws.config()
                .provider_entry(Some("deepseek"))
                .and_then(|e| e.api_key.clone())
                .as_deref(),
            Some("sk-new")
        );

        // 重新加载拿到的是新构建的 provider 实例（旧实例不会被就地修改）
        let after = ws.agent_manager().load_cached("@demo").unwrap();
        assert!(
            !Arc::ptr_eq(before.provider(), after.provider()),
            "provider 实例应随配置重载而重建"
        );
    }

    /// 没有已缓存 Agent 时重载同样安全（返回 0），且不破坏后续加载。
    #[tokio::test]
    async fn reload_providers_with_empty_cache_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        write_providers(tmp.path(), "sk-old");
        let ws = make_workspace(tmp.path());

        assert_eq!(ws.reload_providers().unwrap(), 0);
        assert_eq!(
            ws.config().default_provider_name(),
            "deepseek",
            "重载后仍能与系统配置正确合并"
        );
    }
}
