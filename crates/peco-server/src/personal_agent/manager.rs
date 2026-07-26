// ============================================================================
// PersonalAgentManager — 个人助理生命周期管理器
// ============================================================================
//
// 职责：
//   1. 确保 personal 模板已安装到用户 WorkSpace（首次访问幂等安装）
//   2. 加载 @assistant Agent（@memory 按需通过 AgentLoader 延迟加载）
//
// 与 assistant::PersonalAssistantManager 的核心差异：
//   - 无 PPA 组件：无 DynamicContext、无 MemoryHook
//   - 模板安装：使用 BuiltinTemplate::personal() 而非 include_str!
//   - Agent 加载：WorkspaceManager 而非直接 agent_manager
//   - 记忆管理：@assistant → delegate_sub_agent(@memory) → KB tools
//
// SSE 流式对话逻辑在 handler.rs 中，参考 chat/handler.rs 的模式。

use std::sync::Arc;

use peco_agents::BuiltinTemplate;

use crate::error::ApiError;
use crate::state::AppState;

/// 个人助理管理器。
///
/// 每个用户一个 Manager，持有已加载的 @assistant Agent。
/// @memory Agent 不在此预加载——由 @assistant 通过 delegate_sub_agent 动态调用，
/// AgentLoader trait（WorkSpace）按需从 `agents/@memory/agent.md` 加载。
pub struct PersonalAgentManager {
    /// 主助理 Agent（@assistant），从 WorkSpace 目录加载
    agent: Arc<peco_core::agent::Agent>,
}

impl PersonalAgentManager {
    /// 创建新的 PersonalAgentManager。
    ///
    /// 首次调用会自动安装 `personal` 模板到用户 WorkSpace：
    ///   - agents/@assistant/agent.md
    ///   - agents/@memory/agent.md
    ///   - knowledge/@private_memory/kb_config.json
    ///
    /// 安装是幂等的——已存在的 agent/KB 不会被覆盖。
    /// ★ 不使用 PPA 组件（无 DynamicContext、无 MemoryHook）。
    pub async fn new(state: &AppState, user_id: &str) -> Result<Self, ApiError> {
        // ── 1. 获取 WorkSpace ────────────────────────────────────────────
        let ws = state.workspace_manager.get(user_id)?;

        // ── 2. 幂等安装模板 ──────────────────────────────────────────────
        Self::ensure_template_installed(&ws).await?;

        // ── 3. 使 Agent 缓存失效（模板文件可能刚写入）─────────────────────
        state
            .workspace_manager
            .invalidate_agent(user_id, "@assistant")?;
        state
            .workspace_manager
            .invalidate_agent(user_id, "@memory")?;

        // ── 4. 加载 @assistant Agent（从 WorkSpace 目录，非 DB）─────────
        let agent = state.workspace_manager.get_agent(user_id, "@assistant")?;

        tracing::info!(
            user_id = %user_id,
            "PersonalAgentManager initialized"
        );

        Ok(Self { agent })
    }

    /// 获取 @assistant agent 引用（供 handler 克隆）。
    pub fn agent(&self) -> &Arc<peco_core::agent::Agent> {
        &self.agent
    }

    // ── 私有方法 ──────────────────────────────────────────────────────

    /// 确保 personal 模板已安装在用户 WorkSpace 中。
    ///
    /// 使用 `BuiltinTemplate::personal().materialize()` 将编译时嵌入的模板
    /// 解压到临时目录，然后通过 `WorkSpace::init_from_template()` 幂等安装。
    /// 临时目录在 materialize 返回的 TempDir drop 时自动清理。
    async fn ensure_template_installed(
        ws: &peco_core::workspace::WorkSpace,
    ) -> Result<(), ApiError> {
        let template_dir = BuiltinTemplate::personal().materialize().map_err(|e| {
            ApiError::Internal(format!("failed to materialize personal template: {e}"))
        })?;

        let report = ws
            .init_from_template(template_dir.path())
            .await
            .map_err(|e| ApiError::Internal(format!("template init failed: {e}")))?;

        if !report.agents_installed.is_empty() {
            tracing::info!(
                agents = ?report.agents_installed,
                "Personal agents installed from template"
            );
        }
        if !report.agents_skipped.is_empty() {
            tracing::debug!(
                agents = ?report.agents_skipped,
                "Personal agents already exist, skipped"
            );
        }
        if !report.kbs_created.is_empty() {
            tracing::info!(
                kbs = ?report.kbs_created,
                "Personal knowledge bases created"
            );
        }
        for (name, err) in &report.errors {
            tracing::warn!(%name, %err, "Template init non-fatal error");
        }

        Ok(())
    }
}
