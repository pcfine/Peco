// ============================================================================
// PecoManager — Peco 永续对话生命周期管理器
// ============================================================================
//
// 职责：
//   1. 确保 personal 模板已安装到用户 WorkSpace（首次访问幂等安装）
//   2. 加载 @assistant Agent
//   3. 组装 PecoConfig（compaction / 环境上下文 / 记忆双路径）
//
// 位于 peco 模块（统一入口）。

use std::sync::Arc;

use peco_agents::BuiltinTemplate;
use peco_core::agent::{CompactionPolicy, ModelSummarizer};

use crate::error::ApiError;
use crate::state::AppState;

use super::config::PecoConfig;
use super::environment::EnvironmentInfo;

/// Peco 永续对话管理器。
///
/// 每个用户一个 Manager，持有已加载的 @assistant Agent 和 PecoConfig。
/// @memory Agent 不在此预加载——由 @assistant 通过 delegate_sub_agent 动态调用。
pub struct PecoManager {
    /// 主助理 Agent（@assistant），从 WorkSpace 目录加载
    agent: Arc<peco_core::agent::Agent>,
    /// Peco 配置（compaction / 环境上下文 / 记忆双路径）
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
    /// 用于覆盖默认的预算/压缩/记忆配置。
    pub async fn new_with_config(
        state: &AppState,
        user_id: &str,
        config: PecoConfig,
    ) -> Result<Self, ApiError> {
        // ── 1. 获取 WorkSpace ────────────────────────────────────────────
        let ws = state
            .workspace_manager
            .get_synced(user_id, &state.db)
            .await?;

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

        // ── 5. 构建上下文压缩策略（复用主 Agent 的 provider + Flash 模型）──
        let summarizer = Arc::new(ModelSummarizer::new(
            Arc::clone(agent.provider()),
            config.summarizer_model.clone(),
        ));
        let mut config = config;
        config.compaction = Some(Arc::new(CompactionPolicy {
            trigger_tokens: config.compaction_trigger_tokens,
            keep_recent_tokens: config.compaction_keep_recent_tokens,
            summarizer,
        }));

        // ── 5.5 记忆双路径（写 hook + 读 dynamic_context）────────────────
        //
        // 存储载体是 @private_memory KB（第 2 步模板安装保证存在）。
        // 提取器复用主 Agent 的 provider + Flash 模型 — 与 compaction 同范式。
        // enabled=false 时跳过装配，零开销。
        if config.memory.enabled {
            let km = Arc::clone(ws.knowledge_manager());
            let analyzer = super::memory::ModelTurnAnalyzer::new(
                Arc::clone(agent.provider()),
                config.memory.model.clone(),
            );
            config
                .hooks
                .push(Arc::new(super::memory::MemoryExtractionHook::new(
                    Arc::clone(&km),
                    Arc::new(analyzer),
                    config.memory.clone(),
                )));
            config.dynamic_context = Some(Arc::new(super::memory::MemoryRecallContext::new(
                km,
                config.memory.clone(),
            )));
        }

        // ── 6. 渲染环境上下文（恒定前缀，构造时求值一次）────────────────
        //
        // PecoManager 在每次流连接时新建（handler 每请求调用），
        // 因此这里求值即保证日期新鲜度——每次续接都以当天日期重建环境块。
        // 求值失败的兜底是 user_id，不阻断对话。
        // username 查询经 WorkspaceManager 进程内缓存，每用户仅首次命中 DB。
        let username = resolve_username(
            state.workspace_manager.username(user_id, &state.db).await,
            user_id,
        );
        let env_info = EnvironmentInfo::new(
            user_id,
            &username,
            ws.root().to_path_buf(),
            &agent.config().agent.name,
        );
        config.environment = Some(env_info.render());

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

/// 解析用于环境块展示的用户名：查询缺失 / 空串 / 全空白 → 回退 `user_id`。
fn resolve_username(raw: Option<String>, user_id: &str) -> String {
    match raw {
        Some(name) if !name.trim().is_empty() => name,
        _ => user_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_username_valid() {
        assert_eq!(resolve_username(Some("alice".into()), "uid-1"), "alice");
    }

    #[test]
    fn test_resolve_username_none() {
        assert_eq!(resolve_username(None, "uid-1"), "uid-1");
    }

    #[test]
    fn test_resolve_username_empty() {
        assert_eq!(resolve_username(Some(String::new()), "uid-1"), "uid-1");
    }

    #[test]
    fn test_resolve_username_whitespace() {
        assert_eq!(resolve_username(Some("   ".into()), "uid-1"), "uid-1");
    }
}
