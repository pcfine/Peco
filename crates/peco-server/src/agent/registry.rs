// ============================================================================
// AgentRegistry — Agent 实例 LRU 缓存与生命周期管理
// ============================================================================

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use lru::LruCache;
use peco_core::agent::{
    build_model_config, build_provider, Agent, AgentIdentity, AgentProfile, LlmConfig,
};
use peco_core::mcp::McpManager;
use peco_core::tools::{ToolExecutor, ToolFactory};
use peco_core::GlobalHandler;
use serde::Deserialize;
use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::db::agents::{self, AgentRow};
use crate::error::ApiError;
use crate::knowledge::manager::WebKnowledgeManager;
use crate::knowledge::tools::{
    WebAddToKnowledgeBase, WebGetKnowledgeBaseDocs, WebListKnowledgeBases, WebSearchKnowledge,
    WebSyncKnowledgeBase,
};
use crate::personal_assistant::tools::{ForgetTool, RecallTool, RememberTool};
use crate::personal_assistant::PersonalMemoryStore;

use super::orchestration::{WebDelegateSubAgentTool, WebRunParallelSubAgentsTool};

/// Agent 的 `config_json` 字段结构。
///
/// 存储在 agents 表的 `config_json` 列中，用于存放 tools、mcp_servers、
/// skills 列表以及 temperature / max_tokens 等推理参数。
#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfigJson {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

impl Default for AgentConfigJson {
    fn default() -> Self {
        Self {
            tools: Vec::new(),
            mcp_servers: Vec::new(),
            skills: Vec::new(),
            temperature: None,
            max_tokens: None,
        }
    }
}

// ── CachedAgent ──────────────────────────────────────────────────────────────

/// 缓存的 Agent 实例，包含构建时间戳用于潜在的超时驱逐。
struct CachedAgent {
    agent: Arc<Agent>,
    #[allow(dead_code)]
    cached_at: Instant,
}

// ── AgentRegistry ────────────────────────────────────────────────────────────

/// Agent 实例注册表。
///
/// 使用 LRU 缓存管理已构建的 Agent 实例，避免重复初始化 MCP 连接和工具。
/// 通过 [`get_or_build`](AgentRegistry::get_or_build) 获取 Agent 实例，
/// 缓存命中时直接返回，未命中时从数据库加载配置并构建。
///
/// # 线程安全
///
/// 内部使用 [`tokio::sync::RwLock`] 保护缓存，支持并发读取。
pub struct AgentRegistry {
    /// agent_id → CachedAgent 的 LRU 映射。
    cache: RwLock<LruCache<String, CachedAgent>>,
    /// Web 层知识库管理器（用于注入用户隔离的知识工具）。
    web_km: Arc<WebKnowledgeManager>,
}

impl AgentRegistry {
    /// 创建新的 AgentRegistry。
    ///
    /// `capacity` 指定 LRU 缓存的最大容量（最少为 1）。
    /// `web_km` 用于在构建 Agent 时注入用户隔离的知识工具。
    pub fn new(capacity: usize, web_km: Arc<WebKnowledgeManager>) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).unwrap();
        Self {
            cache: RwLock::new(LruCache::new(cap)),
            web_km,
        }
    }

    /// 获取或构建 Agent 实例。
    ///
    /// 流程：检查缓存 → 命中直接返回 → 未命中从 DB 查询配置 → 构建 Agent →
    /// 写入缓存 → 返回。
    ///
    /// # Errors
    ///
    /// - Agent 在数据库中不存在 → [`ApiError::NotFound`]
    /// - Agent 不属于当前用户 → [`ApiError::Forbidden`]
    /// - Provider 未配置或 API key 缺失 → [`ApiError::Internal`]
    pub async fn get_or_build(
        &self,
        self_arc: Arc<AgentRegistry>,
        pool: &SqlitePool,
        user_id: &str,
        agent_id: &str,
        data_dir: &PathBuf,
    ) -> Result<Arc<Agent>, ApiError> {
        // ── 1. 检查缓存 ──────────────────────────────────────────────────
        {
            let mut cache = self.cache.write().await;
            if let Some(cached) = cache.get(agent_id) {
                tracing::debug!(agent_id = %agent_id, "Agent cache hit");
                return Ok(cached.agent.clone());
            }
        }

        // ── 2. 从 DB 查询 Agent 配置 ──────────────────────────────────────
        let row = agents::find_by_id(pool, agent_id)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("agent '{agent_id}' not found")))?;

        // 所有权校验
        if row.user_id != user_id {
            return Err(ApiError::Forbidden(
                "you do not have access to this agent".into(),
            ));
        }

        // ── 3. 构建 Agent 实例 ────────────────────────────────────────────
        let agent = self.build_agent(&row, self_arc, pool, user_id, data_dir).await?;

        let cached = CachedAgent {
            agent: agent.clone(),
            cached_at: Instant::now(),
        };

        // ── 4. 写入缓存 ───────────────────────────────────────────────────
        {
            let mut cache = self.cache.write().await;
            cache.put(agent_id.to_string(), cached);
        }

        tracing::info!(
            agent_id = %agent_id,
            agent_name = %row.name,
            "Agent built and cached"
        );

        Ok(agent)
    }

    /// 按 Agent 名称查找并获取 Agent 实例（委托给 [`get_or_build`]）。
    ///
    /// 流程：从 DB 按名称查询 agent_id → 复用 get_or_build 的缓存和构建逻辑。
    ///
    /// # Errors
    ///
    /// - Agent 名称不存在 → [`ApiError::NotFound`]
    /// - Agent 不属于当前用户 → [`ApiError::Forbidden`]
    pub async fn get_by_name(
        &self,
        self_arc: Arc<AgentRegistry>,
        pool: &SqlitePool,
        user_id: &str,
        name: &str,
        data_dir: &PathBuf,
    ) -> Result<Arc<Agent>, ApiError> {
        let row = agents::find_by_name_and_user(pool, name, user_id)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("agent '{name}' not found")))?;

        self.get_or_build(self_arc, pool, user_id, &row.id, data_dir).await
    }

    /// 使指定 Agent 的缓存失效（配置更新后调用）。
    ///
    /// 下次调用 [`get_or_build`](AgentRegistry::get_or_build) 时将重新构建。
    pub async fn invalidate(&self, agent_id: &str) {
        let mut cache = self.cache.write().await;
        cache.pop(agent_id);
        tracing::debug!(agent_id = %agent_id, "Agent cache invalidated");
    }

    /// 获取当前缓存中的 Agent 数量。
    #[allow(dead_code)]
    pub async fn cache_size(&self) -> usize {
        self.cache.read().await.len()
    }

    // ── 内部构建逻辑 ──────────────────────────────────────────────────────

    /// 从数据库行构建 Agent 实例。
    async fn build_agent(
        &self,
        row: &AgentRow,
        self_arc: Arc<AgentRegistry>,
        pool: &SqlitePool,
        user_id: &str,
        data_dir: &PathBuf,
    ) -> Result<Arc<Agent>, ApiError> {
        // 解析 config_json
        let config: AgentConfigJson = if row.config_json.is_empty() || row.config_json == "{}" {
            AgentConfigJson::default()
        } else {
            serde_json::from_str(&row.config_json).unwrap_or_else(|e| {
                tracing::warn!(
                    agent_id = %row.id,
                    error = %e,
                    config_json = %row.config_json,
                    "Failed to parse agent config_json, using defaults"
                );
                AgentConfigJson::default()
            })
        };

        // ── 构建 AgentProfile ────────────────────────────────────────────
        let profile = AgentProfile {
            agent: AgentIdentity {
                name: row.name.clone(),
                description: row.description.clone(),
            },
            llm: Some(LlmConfig {
                provider: Some(row.provider.clone()),
                model: Some(row.model.clone()),
                temperature: config.temperature,
                max_tokens: config.max_tokens,
                stream: None,
                reasoning_effort: None,
            }),
            tools: config.tools.clone(),
            mcp: config.mcp_servers.clone(),
            skills: config.skills.clone(),
            max_turns: 20,
        };

        // ── 构建 ModelConfig ──────────────────────────────────────────────
        let model_config = build_model_config(&profile);

        tracing::debug!(
            agent_id = %row.id,
            provider = ?model_config.provider_name,
            model = ?model_config.model_name,
            "Model config resolved for agent"
        );

        // ── 构建 ModelProvider ────────────────────────────────────────────
        let model = build_provider(&model_config).map_err(|e| {
            ApiError::Internal(format!("failed to build provider for agent '{}': {e}", row.id))
        })?;

        // ── 构建 ToolExecutor ─────────────────────────────────────────────
        let tool_factory = ToolFactory::global();
        let executor = tool_factory.make_tools_executor(&config.tools);

        // ★ 注入 Web 版子 Agent 工具 + 知识工具（替换全局工具，实现用户隔离）
        replace_tool_if_configured(&config, &executor, "delegate_sub_agent", WebDelegateSubAgentTool::new(
            pool.clone(), self_arc.clone(), data_dir.clone(), user_id.to_string(),
        ));
        replace_tool_if_configured(&config, &executor, "run_parallel_sub_agents", WebRunParallelSubAgentsTool::new(
            pool.clone(), self_arc.clone(), data_dir.clone(), user_id.to_string(),
        ));

        let web_km = self.web_km.clone();
        let uid = user_id.to_string();
        replace_tool_if_configured(&config, &executor, "search_knowledge", WebSearchKnowledge::new(web_km.clone(), uid.clone()));
        replace_tool_if_configured(&config, &executor, "list_knowledge_bases", WebListKnowledgeBases::new(web_km.clone(), uid.clone()));
        replace_tool_if_configured(&config, &executor, "add_to_knowledge_base", WebAddToKnowledgeBase::new(web_km.clone(), uid.clone()));
        replace_tool_if_configured(&config, &executor, "sync_knowledge_base", WebSyncKnowledgeBase::new(web_km.clone(), uid.clone()));
        replace_tool_if_configured(&config, &executor, "get_knowledge_base_docs", WebGetKnowledgeBaseDocs::new(web_km.clone(), uid.clone()));

        // ★ PPA 工具：remember / recall / forget（用户隔离）
        let user_km = self.web_km.get_manager(&uid).await.map_err(|e| {
            ApiError::Internal(format!("failed to get user knowledge manager: {e}"))
        })?;
        let ppa_store = Arc::new(PersonalMemoryStore::new(
            user_km,
            format!("personal_memory_{}", uid),
            Default::default(),
        ));
        replace_tool_if_configured(&config, &executor, "remember", RememberTool::new(ppa_store.clone()));
        replace_tool_if_configured(&config, &executor, "recall", RecallTool::new(ppa_store.clone()));
        replace_tool_if_configured(&config, &executor, "forget", ForgetTool::new(ppa_store.clone()));

        let tool_executor: Arc<dyn ToolExecutor> = Arc::new(executor);

        tracing::debug!(
            agent_id = %row.id,
            tool_count = tool_executor.definitions().len(),
            tools = ?config.tools,
            "Tools registered for agent"
        );

        // ── 构建 McpManager ──────────────────────────────────────────────
        let mcp_config = GlobalHandler::global().config().mcp_config();
        let mcp_servers: Vec<(String, peco_core::config::McpServerConfig)> = config
            .mcp_servers
            .iter()
            .filter_map(|name| {
                mcp_config
                    .get_server(name)
                    .filter(|c| c.enabled)
                    .map(|c| (name.clone(), c.clone()))
            })
            .collect();

        // 对声明了但未找到的 MCP server 发出警告
        for name in &config.mcp_servers {
            if mcp_config.get_server(name).is_none() {
                tracing::warn!(
                    agent_id = %row.id,
                    server = %name,
                    "MCP server declared in agent config but not found in mcpconfig.json"
                );
            }
        }

        let mcp_manager = Arc::new(McpManager::new(&mcp_servers, tool_executor.clone()).await);

        tracing::debug!(
            agent_id = %row.id,
            mcp_count = mcp_manager.server_count(),
            "MCP connections established for agent"
        );

        // ── 构建 agent.md 路径 ─────────────────────────────────────────────
        let md_path = data_dir
            .join("agents")
            .join(&row.user_id)
            .join(&row.id)
            .join("agent.md");

        // ── 组装 Agent ────────────────────────────────────────────────────
        let agent = Agent::from_parts(
            md_path,
            profile,
            row.system_prompt.clone(),
            model,
            model_config,
            tool_executor,
            mcp_manager,
        );

        Ok(Arc::new(agent))
    }
}

/// 如果 tools 配置中包含 `tool_name`，则用 `replacement` 替换现有工具实现。
///
/// 用于将全局工具（文件路径查找 Agent）替换为 Web 感知版本（DB 查找）。
fn replace_tool_if_configured<T: peco_core::tools::ToolDyn + 'static>(
    config: &AgentConfigJson,
    executor: &impl peco_core::tools::ToolExecutor,
    tool_name: &str,
    replacement: T,
) {
    if config.tools.iter().any(|t| t == tool_name) {
        executor.remove_tool(tool_name).ok();
        executor.add_tool(Box::new(replacement)).ok();
        tracing::info!(tool = tool_name, "Replaced with web-aware version");
    }
}
