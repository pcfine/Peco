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

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use peco_agents::BuiltinTemplate;
use peco_core::agent::{CompactionPolicy, ModelSummarizer, TurnSummarizer};
use tracing::{debug, warn};

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

        // ── 5. 构建元任务模型（复用主 Agent 的 provider + Flash 模型）────
        //
        // 一个实例担两职：轮边界压缩的摘要器，与撞上 max_iterations 时的收尾报告器
        // （`summarize_inflight`）。两者同范式，差异只在提示词。合成失败非致命，
        // 收尾那条路径回退到固定中断说明。
        let summarizer: Arc<dyn TurnSummarizer> = Arc::new(ModelSummarizer::new(
            Arc::clone(agent.provider()),
            config.summarizer_model.clone(),
        ));
        let mut config = config;
        config.epilogue = Some(Arc::clone(&summarizer));
        config.compaction = Some(Arc::new(CompactionPolicy::new(
            config.compaction_trigger_tokens,
            config.compaction_keep_recent_tokens,
            summarizer,
        )));

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
                    state.db.clone(),
                    user_id.to_string(),
                )));
            config.dynamic_context = Some(Arc::new(super::memory::MemoryRecallContext::new(
                km,
                config.memory.clone(),
                user_id,
                Some(state.db.clone()),
            )));
        }

        // ── 5.6 压缩日志钩子 ──────────────────────────────────────────────
        //   每次滚动压缩成功后追加 peco_compaction_log 记录。
        config
            .hooks
            .push(Arc::new(super::metrics::CompactionMetricsHook::new(
                state.db.clone(),
                user_id,
                super::session::private_session_id(user_id),
            )));

        // ── 5.7 启动通道：每用户一次对账 + 每进程一次保留期清理 ─────────
        //
        // PecoManager 每次流连接都新建，两个静态闸门保证对账按用户、清理按
        // 进程各只跑一次；均为 fire-and-forget spawn，失败只记日志，不阻塞建连。
        startup_housekeeping(&state.db, ws.knowledge_manager(), &config.memory, user_id);

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

/// 进程内已做过启动对账的用户 —— 每用户每进程一次。
static STARTUP_RECONCILED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// 进程级启动保留期清理闸门 —— 每进程一次。
static STARTUP_PURGE: OnceLock<()> = OnceLock::new();

/// 启动通道：未收口取代意图的对账 + 三表保留期清理。
///
/// 不依赖 `memory.enabled`：开关关闭时历史上开门期写入的 intent 仍需收口。
/// 守卫判重抽成独立函数（[`claim_startup_reconcile`] / [`claim_startup_purge`]），
/// spawn 本身不可断言，测试只测守卫语义。
fn startup_housekeeping(
    db: &sqlx::SqlitePool,
    km: &Arc<peco_core::knowledge::KnowledgeManager>,
    memory: &super::memory::MemoryConfig,
    user_id: &str,
) {
    if claim_startup_reconcile(user_id) {
        let db = db.clone();
        let km = Arc::clone(km);
        let memory = memory.clone();
        let user_id = user_id.to_string();
        tokio::spawn(async move {
            super::memory::reconcile(&db, &km, &memory, &user_id).await;
        });
    }
    if claim_startup_purge() {
        let db = db.clone();
        let memory = memory.clone();
        tokio::spawn(async move { purge_expired(&db, &memory).await });
    }
}

/// 对账守卫：该用户本进程首次领取返回 true。
fn claim_startup_reconcile(user_id: &str) -> bool {
    STARTUP_RECONCILED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        // 中毒时取回内部值继续 —— 集合判重无不变量可破坏，不值得 panic
        .unwrap_or_else(|e| e.into_inner())
        .insert(user_id.to_string())
}

/// 清理守卫：本进程首次触发返回 true。
fn claim_startup_purge() -> bool {
    STARTUP_PURGE.set(()).is_ok()
}

/// 三表保留期清理（§6.6 启动通道）：intent 按 done / failed+cancelled 分档，
/// shadow 单档，audit 按 reason 分档（superseded 独立档，pending 永不清）。
/// 任一失败仅记日志 —— 清理非致命，下个进程周期重试。
async fn purge_expired(db: &sqlx::SqlitePool, memory: &super::memory::MemoryConfig) {
    let cutoff =
        |days: u64| (chrono::Utc::now() - chrono::Duration::days(days as i64)).to_rfc3339();

    match crate::db::memory_supersede::purge_intent_older_than(
        db,
        &cutoff(memory.intent_done_retention_days),
        &cutoff(memory.intent_failed_retention_days),
    )
    .await
    {
        Ok(n) if n > 0 => debug!(purged = n, "Supersede intents purged"),
        Err(e) => warn!("supersede intent purge failed; {e}"),
        _ => {}
    }

    match crate::db::memory_supersede::purge_shadow_older_than(
        db,
        &cutoff(memory.shadow_retention_days),
    )
    .await
    {
        Ok(n) if n > 0 => debug!(purged = n, "Supersede shadow rows purged"),
        Err(e) => warn!("supersede shadow purge failed; {e}"),
        _ => {}
    }

    match crate::db::memory_audit::purge_older_than(
        db,
        // 审计保留期挂在巩固配置上（清理只消费天数，不依赖巩固开关）
        &cutoff(memory.consolidation.audit_retention_days),
        &cutoff(memory.superseded_retention_days),
    )
    .await
    {
        Ok(n) if n > 0 => debug!(purged = n, "Memory audit rows purged"),
        Err(e) => warn!("memory audit purge failed; {e}"),
        _ => {}
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

    // ── 启动通道（对账守卫 + 保留期清理）────────────────────────────────

    #[test]
    fn startup_reconcile_guard_is_once_per_user() {
        // 每用户每进程一次：同用户重复领取被拦截，不同用户互不影响
        assert!(claim_startup_reconcile("guard-user-a"));
        assert!(!claim_startup_reconcile("guard-user-a"));
        assert!(claim_startup_reconcile("guard-user-b"));
        assert!(!claim_startup_reconcile("guard-user-b"));
    }

    fn days_ago(days: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339()
    }

    /// 插入一条 intent 并按需改写终态 —— 直写 UPDATE 绕过 CAS 领取流程，
    /// 保证 updated_at 就是我们指定的时刻。
    async fn put_intent(pool: &sqlx::SqlitePool, doc: &str, status: &str, ts: &str) {
        let id = crate::db::memory_supersede::write_intent(
            pool,
            &crate::db::memory_supersede::IntentRow {
                user_id: "purge-user".to_string(),
                kb_name: "@private_memory".to_string(),
                topic_key: Some("answer_style".to_string()),
                old_doc_id: format!("old-{doc}"),
                old_title: "old_title".to_string(),
                old_source: "ppa_profile".to_string(),
                new_doc_id: doc.to_string(),
                new_title: "new_title".to_string(),
                new_content: format!("content {doc}"),
                new_source: "ppa_profile".to_string(),
                created_at: ts.to_string(),
                updated_at: ts.to_string(),
            },
        )
        .await
        .unwrap();
        if status != "pending" {
            sqlx::query(
                "UPDATE memory_supersede_intent SET status = ?, updated_at = ? WHERE id = ?",
            )
            .bind(status)
            .bind(ts)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    /// 插入一条审计行，`done=true` 时迁移到终态（deleted_at 保持入参时刻）。
    async fn put_audit(
        pool: &sqlx::SqlitePool,
        doc: &str,
        reason: &str,
        deleted_at: &str,
        done: bool,
    ) {
        let id = crate::db::memory_audit::insert_pending(
            pool,
            &peco_core::tools::MemoryAuditEntry {
                user_id: "purge-user".to_string(),
                kb_name: "@private_memory".to_string(),
                doc_id: doc.to_string(),
                title: "t".to_string(),
                content: "c".to_string(),
                source: "ppa_profile".to_string(),
                reason: reason.to_string(),
                deleted_by: "hook:supersede".to_string(),
                deleted_at: deleted_at.to_string(),
                topic_key: None,
                successor_doc_id: None,
            },
        )
        .await
        .unwrap();
        if done {
            crate::db::memory_audit::mark_done(pool, id).await.unwrap();
        }
    }

    #[tokio::test]
    async fn startup_purge_expired_rows_by_tier() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();

        // intent：done 超 7d 清、failed 超 90d 清；done 新鲜与陈旧 pending 都保留
        put_intent(&pool, "i-done-old", "done", &days_ago(8)).await;
        put_intent(&pool, "i-done-new", "done", &days_ago(1)).await;
        put_intent(&pool, "i-failed-old", "failed", &days_ago(91)).await;
        put_intent(&pool, "i-pending-old", "pending", &days_ago(400)).await;

        // shadow：超 30d 清、新鲜保留
        for ts in [days_ago(31), days_ago(1)] {
            crate::db::memory_supersede::insert_shadow(
                &pool,
                &crate::db::memory_supersede::ShadowRow {
                    user_id: "purge-user".to_string(),
                    created_at: ts,
                    candidates_json: "[]".to_string(),
                    facts_json: "[]".to_string(),
                    decisions_json: "[]".to_string(),
                    acted: false,
                    extracted_topic_cnt: 0,
                    episodic_cnt: 0,
                },
            )
            .await
            .unwrap();
        }

        // audit：superseded 走 30d 档、其余走 90d 档、pending 永不清
        put_audit(&pool, "a-sup-old", "superseded", &days_ago(45), true).await;
        put_audit(&pool, "a-other-old", "manual_organize", &days_ago(45), true).await;
        put_audit(&pool, "a-sup-pending", "superseded", &days_ago(45), false).await;
        put_audit(
            &pool,
            "a-other-ancient",
            "manual_organize",
            &days_ago(91),
            true,
        )
        .await;

        purge_expired(&pool, &crate::peco::memory::MemoryConfig::default()).await;

        let intents: Vec<String> = sqlx::query_scalar(
            "SELECT new_doc_id FROM memory_supersede_intent ORDER BY new_doc_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(intents, ["i-done-new", "i-pending-old"]);

        let shadows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_supersede_shadow")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(shadows, 1);

        let audits: Vec<String> =
            sqlx::query_scalar("SELECT doc_id FROM memory_audit ORDER BY doc_id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(audits, ["a-other-old", "a-sup-pending"]);
    }
}
