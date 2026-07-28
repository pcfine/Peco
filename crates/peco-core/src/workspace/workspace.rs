// ============================================================================
// WorkSpace — 用户隔离的核心抽象
// ============================================================================

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agent::AgentManager;
use crate::config::{SystemConfig, UserConfig};
use crate::knowledge::KnowledgeManager;
use crate::skills::SkillRegister;

use super::error::WorkspaceError;
use crate::tools::{AgentLoader, KnowledgeAccess, SkillProvider, ToolExecutor, ToolRegister};

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
    pub fn skill_registry(&self) -> &Arc<SkillRegister> {
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

impl AgentLoader for WorkSpace {
    fn load_agent(&self, name: &str) -> Result<Arc<crate::agent::Agent>, crate::agent::AgentError> {
        self.agent_manager.load_agent(name)
    }

    fn list_agent_names(&self) -> Vec<String> {
        self.agent_manager.list_names()
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
