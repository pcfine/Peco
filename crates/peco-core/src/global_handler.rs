// ============================================================================
// GlobalHandler — 集中管理程序中的单例对象。
// ============================================================================
//!
//! 提供统一的访问入口：
//! - [`GlobalConfig`](crate::config::GlobalConfig) — 全局配置（Provider、MCP 服务器注册表）
//! - [`ToolFactory`](crate::tools::ToolFactory) — 内置工具注册表
//! - [`GlobalSkillList`](crate::skills::GlobalSkillList) — Skill 生命周期管理
//!
//! # MCP 连接管理
//!
//! MCP 连接是**按 Agent 级别**的 — 每个 Agent 通过
//! [`McpManager::new`](crate::mcp::McpManager::new) 声明自己需要的 MCP 服务器，
//! 从全局 [`McpConfig`] 中按名称查找配置后建立连接。
//! 详见 [`McpManager`](crate::mcp::McpManager) 文档。

use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};

use crate::config::GlobalConfig;
use crate::knowledge::KnowledgeManager;

// ── 单例 ──────────────────────────────────────────────────────────────────────

static HANDLER: LazyLock<GlobalHandler> = LazyLock::new(GlobalHandler::init);

// ── GlobalHandler ─────────────────────────────────────────────────────────────

/// 全局 Handler — 集中管理程序中的单例对象。
///
/// 通过 [`GlobalHandler::global()`] 获取全局引用，通过访问器方法获取各管理器。
///
/// # 初始化
///
/// - **同步**（首次访问时自动完成）：加载 Provider 配置、MCP 配置、
///   `ToolFactory`、空 `GlobalSkillList`、`KnowledgeManager`
/// - **延迟**（需显式调用）：[`init_skills`](GlobalHandler::init_skills)（同步），
///   知识库模块的异步加载通过 `knowledge_manager().ensure_loaded()` 延迟完成
///
/// # Example
///
/// ```ignore
/// use peco_core::GlobalHandler;
///
/// let handler = GlobalHandler::global();
/// handler.init_skills()?;
/// // 通过 handler.config().mcp_config() 访问 MCP 服务器配置
/// ```
pub struct GlobalHandler {
    config: GlobalConfig,
    tool_factory: crate::tools::ToolFactory,
    skill_list: RwLock<crate::skills::GlobalSkillList>,
    knowledge_manager: KnowledgeManager,
}

impl GlobalHandler {
    // ── 初始化 ────────────────────────────────────────────────────────────

    /// 同步初始化：加载 GlobalConfig + ToolFactory + 空 GlobalSkillList + KnowledgeManager。
    ///
    /// GlobalConfig 内部加载 `providers.toml` 和 `mcpconfig.json`（均含默认降级）。
    /// KnowledgeManager 的异步初始化需通过 `knowledge_manager().ensure_loaded()` 延迟完成。
    fn init() -> Self {
        let config = GlobalConfig::load();
        let knowledge_manager = KnowledgeManager::with_config(
            config.knowledge_config().base_dir.clone(),
            config.knowledge_config().clone(),
        );

        Self {
            config,
            tool_factory: crate::tools::ToolFactory::init(),
            skill_list: RwLock::new(crate::skills::GlobalSkillList::new(resolve_skills_root())),
            knowledge_manager,
        }
    }

    /// 返回全局单例引用。
    ///
    /// 首次调用时会触发同步初始化（配置加载 + ToolFactory + 空 GlobalSkillList）。
    pub fn global() -> &'static GlobalHandler {
        &HANDLER
    }

    // ── 访问器 ────────────────────────────────────────────────────────────

    /// 返回 [`GlobalConfig`] 的引用。
    ///
    /// 通过 `config().providers()` 访问 provider 配置，
    /// 通过 `config().mcp_config()` 访问 MCP 服务器配置（全局注册表），
    /// 通过 `config().default_provider_name()` 获取默认 provider 名称等。
    pub fn config(&self) -> &GlobalConfig {
        &self.config
    }

    /// 返回 [`ToolFactory`](crate::tools::ToolFactory) 的引用。
    pub fn tool_factory(&self) -> &crate::tools::ToolFactory {
        &self.tool_factory
    }

    /// 返回 [`GlobalSkillList`](crate::skills::GlobalSkillList) 的 [`RwLock`] 引用。
    ///
    /// 调用方需自行获取读写锁。
    /// - 读操作（`all_meta()`, `has_skill()`, `stats()` 等）使用 `read()`
    /// - 写操作（`init()`, `activate()`, `set_skills_root()` 等）使用 `write()`
    pub fn skill_list(&self) -> &RwLock<crate::skills::GlobalSkillList> {
        &self.skill_list
    }

    /// 返回 [`KnowledgeManager`] 的引用。
    ///
    /// 知识库管理器的异步初始化通过 `knowledge_manager().ensure_loaded().await`
    /// 延迟完成。所有知识库工具在调用时会自动触发初始化。
    pub fn knowledge_manager(&self) -> &KnowledgeManager {
        &self.knowledge_manager
    }

    // ── 便捷方法 ──────────────────────────────────────────────────────────

    /// 扫描并加载 Skills 元数据（Tier 1）。
    ///
    /// 调用 [`GlobalSkillList::init`](crate::skills::GlobalSkillList::init)，扫描由
    /// `PECO_SKILLS_ROOT` 环境变量或 `./skills/` 目录下的所有 SKILL.md 文件。
    ///
    /// 返回成功注册的 Skill 数量。
    pub fn init_skills(&self) -> Result<usize, crate::skills::SkillError> {
        self.skill_list.write().expect("RwLock poisoned").init()
    }

    // ── Agent 创建 ─────────────────────────────────────────────────────────

    /// 从 agent.md 文件创建 [`Agent`](crate::agent::Agent)。
    ///
    /// 这是一个便捷方法，等效于：
    ///
    /// ```ignore
    /// Agent::from_file(path)
    /// ```
    ///
    /// 内部使用 [`GlobalHandler`] 的 `ToolFactory`、`GlobalSkillList` 和 Provider
    /// 配置组装 Agent。MCP 连接由 Agent 按需创建。
    ///
    /// # Errors
    ///
    /// 若文件读取、解析或 provider 构建失败，返回 [`AgentError`](crate::agent::AgentError)。
    pub async fn create_agent(
        &self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<crate::agent::Agent, crate::agent::AgentError> {
        crate::agent::Agent::from_file(path).await
    }
}

// ── 路径解析 ──────────────────────────────────────────────────────────────────

/// 解析 skills 根目录路径：
/// 1. `PECO_SKILLS_ROOT` 环境变量
/// 2. `./skills/`（默认）
fn resolve_skills_root() -> PathBuf {
    std::env::var("PECO_SKILLS_ROOT")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./skills"))
}
