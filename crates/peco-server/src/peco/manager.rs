// ============================================================================
// PecoManager — Peco 永续对话生命周期管理器
// ============================================================================
//
// 职责：
//   1. 确保 personal 模板已安装到用户 WorkSpace（首次访问幂等安装）
//   2. 加载 @assistant Agent
//   3. 持有 PecoConfig（含 PPA 钩子预留注入点）
//
// 与 personal_agent::PersonalAgentManager 的核心差异：
//   - 使用 PecoConfig（可扩展，预留 Hook/DynamicContext）
//   - 位于 peco 模块（统一入口）

use std::sync::Arc;

use peco_agents::BuiltinTemplate;

use crate::error::ApiError;
use crate::state::AppState;

use super::config::PecoConfig;

/// Peco 永续对话管理器。
///
/// 每个用户一个 Manager，持有已加载的 @assistant Agent 和 PecoConfig。
/// @memory Agent 不在此预加载——由 @assistant 通过 delegate_sub_agent 动态调用。
pub struct PecoManager {
    /// 主助理 Agent（@assistant），从 WorkSpace 目录加载
    agent: Arc<peco_core::agent::Agent>,
    /// Peco 配置（含 PPA 钩子预留注入点）
    config: PecoConfig,
}

impl PecoManager {
    /// 创建新的 PecoManager。
    ///
    /// 首次调用会自动安装 `personal` 模板到用户 WorkSpace：
    ///   - agents/@assistant/agent.md
    ///   - agents/@memory/agent.md
    ///   - knowledge/@private_memory/kb_config.json
    ///
    /// 安装是幂等的——已存在的 agent/KB 不会被覆盖。
    pub async fn new(state: &AppState, user_id: &str) -> Result<Self, ApiError> {
        Self::new_with_config(state, user_id, PecoConfig::default()).await
    }

    /// 创建带自定义配置的 PecoManager。
    ///
    /// 用于后续接入 PPA 时注入 DynamicContext 和 LooperHook。
    pub async fn new_with_config(
        state: &AppState,
        user_id: &str,
        config: PecoConfig,
    ) -> Result<Self, ApiError> {
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
            "PecoManager initialized"
        );

        Ok(Self { agent, config })
    }

    /// 获取 @assistant agent 引用（供 handler 克隆）。
    pub fn agent(&self) -> &Arc<peco_core::agent::Agent> {
        &self.agent
    }

    /// 获取 PecoConfig 引用。
    pub fn config(&self) -> &PecoConfig {
        &self.config
    }

    // ── 私有方法 ──────────────────────────────────────────────────────

    /// 确保 personal 模板已安装在用户 WorkSpace 中。
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
