//! 压缩日志与边界观测钩子：把核心的折叠 / 削减结算落进 DB。
//!
//! [`CompactionMetricsHook`] 只在真的折叠时写 `peco_compaction_log`；
//! [`ContextShadowHook`] 每个 turn 边界写一行 `peco_context_shadow`，补上
//! 「没有折叠的边界」—— 两张表的口径不同，混成一张即失真。
//!
//! 可观测性数据绝不影响主流程：所有写入放后台，失败仅 warn。

use async_trait::async_trait;
use chrono::Utc;
use model_provider::{Content, ContentPart, InputItem};
use peco_core::agent::hooks::LooperHook;
use peco_core::agent::{BoundaryReduction, CompactionOutcome, select_history_turns};
use peco_core::session::Session;
use sqlx::SqlitePool;
use tracing::{debug, warn};

use crate::db::{compaction_log, context_shadow};

/// 压缩日志钩子。
///
/// 每个聊天 run 一个实例（装配期固定 user_id 与 conversation_id）：钩子无法在
/// 回调里得知是哪个会话，参数只能构造期给定。
pub struct CompactionMetricsHook {
    pool: SqlitePool,
    user_id: String,
    conversation_id: String,
}

impl CompactionMetricsHook {
    pub fn new(
        pool: SqlitePool,
        user_id: impl Into<String>,
        conversation_id: impl Into<String>,
    ) -> Self {
        Self {
            pool,
            user_id: user_id.into(),
            conversation_id: conversation_id.into(),
        }
    }
}

#[async_trait]
impl LooperHook for CompactionMetricsHook {
    async fn on_context_compacted(&self, outcome: &CompactionOutcome) {
        // SQLite 写入放后台 — 钩子约定不阻塞 turn 边界（对齐记忆提取 hook）。
        let pool = self.pool.clone();
        let user_id = self.user_id.clone();
        let conversation_id = self.conversation_id.clone();
        let evicted_turns = outcome.evicted_turns;
        let tokens_before = outcome.estimated_tokens_before;
        let tokens_after = outcome.estimated_tokens_after;
        let summary_chars = outcome.summary.chars().count();
        tokio::spawn(async move {
            let id = uuid::Uuid::new_v4().to_string();
            if let Err(e) = compaction_log::insert(
                &pool,
                &id,
                &user_id,
                &conversation_id,
                evicted_turns,
                tokens_before,
                tokens_after,
                summary_chars,
            )
            .await
            {
                warn!(error = %e, "Failed to record compaction log (non-fatal)");
            }
        });
    }
}

/// 边界观测钩子：每个 turn 边界向 `peco_context_shadow` 写一行。
///
/// `enabled` 由装配方按 gate（shadow / enforce 开关）传入：钩子恒注册、gate 关时
/// 首行返回 —— 行为等价于未装配，但装配点无需分支。
pub struct ContextShadowHook {
    pool: SqlitePool,
    user_id: String,
    conversation_id: String,
    history_token_budget: usize,
    enabled: bool,
    /// 工具输出截断上限（与削减同源的配置值）；`0` = 关闭，超限计数恒 0。
    tool_output_max_chars: usize,
}

impl ContextShadowHook {
    /// * `history_token_budget` — 与 filter 同源的 Verbatim 窗口预算
    ///   （`view_window_tokens` 列 = 该预算下的连续窗口 view 口径）
    /// * `enabled` — shadow gate；`false` 时钩子不写任何行
    /// * `tool_output_max_chars` — 「超上限输出」计数的判据，与削减同一配置值
    pub fn new(
        pool: SqlitePool,
        user_id: impl Into<String>,
        conversation_id: impl Into<String>,
        history_token_budget: usize,
        enabled: bool,
        tool_output_max_chars: usize,
    ) -> Self {
        Self {
            pool,
            user_id: user_id.into(),
            conversation_id: conversation_id.into(),
            history_token_budget,
            enabled,
            tool_output_max_chars,
        }
    }
}

#[async_trait]
impl LooperHook for ContextShadowHook {
    async fn on_boundary_reduction(&self, session: &Session, reduction: &BoundaryReduction) {
        if !self.enabled {
            return;
        }

        // ── 字段同步计算（session 借用期内取完，落库放后台） ──
        let view_window_tokens =
            select_history_turns(session.committed_turns(), self.history_token_budget).1;

        // tool / 图片观测：扫全部 committed —— 与 pinned/full/view 同口径
        // （整个会话在此边界的状态，而非「本轮新增」）
        let mut tool_output_items = 0usize;
        let mut tool_output_over_cap = 0usize;
        let mut image_output_items = 0usize;
        let mut image_base64_bytes = 0usize;
        for turn in session.committed_turns() {
            for am in turn {
                if let InputItem::FunctionCallOutput { output, .. } = am.message.as_ref() {
                    tool_output_items += 1;
                    if self.tool_output_max_chars > 0
                        && output.text_view().chars().count() > self.tool_output_max_chars
                    {
                        tool_output_over_cap += 1;
                    }
                    if matches!(output, Content::Parts(_)) && !output.is_plain_text() {
                        image_output_items += 1;
                        for part in output.image_parts() {
                            if let ContentPart::Image { url, .. } = part {
                                // 只数 data URI：https URL 不占快照字节
                                if url.starts_with("data:") {
                                    image_base64_bytes += url.len();
                                }
                            }
                        }
                    }
                }
            }
        }

        // 快照字节近似：钩子拿不到轮边界令牌，序列化 committed + pinned
        // 与快照主体同形 —— 边界后 staging / pending 为空，仅差固定小字段。
        let snapshot_json_bytes =
            serde_json::to_string(&(session.committed_turns(), session.pinned_summary()))
                .map(|s| s.len())
                .unwrap_or(0);

        let strip_evaluated = reduction.strip_evaluation.is_some();
        let stripped_items = reduction
            .strip_evaluation
            .map(|e| e.would_change_items)
            .unwrap_or(0);
        let released_tokens = reduction
            .strip_evaluation
            .map(|e| e.would_release_tokens)
            .unwrap_or(0);

        let row = context_shadow::ContextShadowRow {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: self.user_id.clone(),
            conversation_id: self.conversation_id.clone(),
            turn_index: reduction.turn_index as i64,
            created_at: Utc::now().to_rfc3339(),
            trigger_caliber: if reduction.guard_triggered {
                "full+guard".to_string()
            } else {
                "full".to_string()
            },
            pinned_tokens: reduction.pinned_tokens as i64,
            committed_full_tokens: reduction.committed_full_tokens as i64,
            committed_view_tokens: reduction.committed_view_tokens as i64,
            view_window_tokens: view_window_tokens as i64,
            fold_triggered: reduction.folded() as i64,
            evicted_turns: reduction
                .compaction
                .as_ref()
                .map(|c| c.evicted_turns as i64),
            guard_triggered: reduction.guard_triggered as i64,
            strip_evaluated: strip_evaluated as i64,
            // 生效值：只认实际改写（shadow 路径无 strip，恒 0）
            strip_enforced: reduction.strip.as_ref().is_some_and(|s| s.mutated()) as i64,
            // 评估值：若执行将削减的条数（enforce 时即生效值，同一份扫描）
            stripped_items: stripped_items as i64,
            released_tokens: released_tokens as i64,
            tool_output_items: tool_output_items as i64,
            tool_output_over_cap: tool_output_over_cap as i64,
            image_output_items: image_output_items as i64,
            image_base64_bytes: image_base64_bytes as i64,
            snapshot_json_bytes: snapshot_json_bytes as i64,
        };

        let pool = self.pool.clone();
        tokio::spawn(async move {
            if let Err(e) = context_shadow::insert(&pool, &row).await {
                warn!(error = %e, "Failed to record context shadow row (non-fatal)");
            }
        });
    }
}

/// 按保留期清理边界观测表：启动时跑一次。
///
/// 清理失败只记日志 —— 过期行留到下一个进程周期回收，不该拦住 daemon 启动。
pub async fn purge_expired_shadow(pool: &SqlitePool, retention_days: u64) {
    let cutoff = (Utc::now() - chrono::Duration::days(retention_days as i64)).to_rfc3339();
    match context_shadow::purge_older_than(pool, &cutoff).await {
        Ok(0) => {}
        Ok(purged) => debug!(purged, "Context shadow rows purged"),
        Err(e) => warn!(error = %e, "Context shadow purge failed"),
    }
}

/// 无折叠边界也必须有观测行：这是观测钩子与折叠日志的区别所在。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use peco_core::agent::{StripEvaluation, StripOutcome};
    use peco_core::session::{MessageSource, Session};

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = db::connect(&url).await.unwrap();
        db::run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    /// 一轮含 reasoning + 超上限工具输出 + 含图输出的会话。
    fn fixture_session() -> Session {
        let mut s = Session::new("shadow".to_string(), "shadow".to_string());
        s.start_turn("hi".into()).unwrap();
        s.stage_item(
            MessageSource::ModelGeneration,
            InputItem::Reasoning {
                content: "思考".to_string(),
            },
        )
        .unwrap();
        s.stage_item(
            MessageSource::ModelGeneration,
            InputItem::FunctionCallOutput {
                call_id: "c1".into(),
                output: "x".repeat(2500).into(),
            },
        )
        .unwrap();
        s.stage_item(
            MessageSource::ModelGeneration,
            InputItem::FunctionCallOutput {
                call_id: "c2".into(),
                output: Content::Parts(vec![
                    ContentPart::Text {
                        text: "ok".to_string(),
                    },
                    ContentPart::Image {
                        url: "data:image/png;base64,QUJD".to_string(),
                        detail: None,
                    },
                ]),
            },
        )
        .unwrap();
        let _ = s.commit_turn().unwrap();
        s
    }

    fn fixture_reduction() -> BoundaryReduction {
        BoundaryReduction {
            turn_index: 1,
            pinned_tokens: 100,
            committed_full_tokens: 500,
            committed_view_tokens: 120,
            guard_triggered: false,
            strip: None,
            strip_evaluation: Some(StripEvaluation {
                would_change_items: 3,
                would_release_tokens: 42,
                skipped_turns: 1,
            }),
            compaction: None,
        }
    }

    async fn wait_for_shadow_rows(
        pool: &SqlitePool,
        user_id: &str,
    ) -> Vec<context_shadow::ContextShadowRow> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let rows = context_shadow::list_by_user(pool, user_id).await.unwrap();
                if !rows.is_empty() {
                    break rows;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("shadow row should appear in background")
    }

    #[tokio::test]
    async fn compaction_hook_writes_one_row_per_fold() {
        let (pool, _dir) = test_pool().await;

        let hook = CompactionMetricsHook::new(pool.clone(), "u1", "conv-1");
        let outcome = CompactionOutcome {
            evicted_turns: 4,
            summary: "## 已做决定\n- 采用方案 A".to_string(),
            estimated_tokens_before: 24_500,
            estimated_tokens_after: 8_800,
            degraded: false,
        };
        hook.on_context_compacted(&outcome).await;

        let rows = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let rows = compaction_log::list_by_conversation(&pool, "u1", "conv-1")
                    .await
                    .unwrap();
                if !rows.is_empty() {
                    break rows;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("compaction log row should appear in background");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].conversation_id, "conv-1");
        assert_eq!(rows[0].evicted_turns, 4);
        assert_eq!(rows[0].tokens_before, 24_500);
        assert_eq!(rows[0].tokens_after, 8_800);
        // 摘要长度按字符数统计（非字节数）
        assert_eq!(
            rows[0].summary_chars,
            "## 已做决定\n- 采用方案 A".chars().count() as i64
        );
    }

    #[tokio::test]
    async fn shadow_hook_writes_row_and_leaves_compaction_log_alone() {
        let (pool, _dir) = test_pool().await;

        let hook = ContextShadowHook::new(
            pool.clone(),
            "u1",
            "conv-1",
            12_000,
            true,
            peco_core::agent::TRANSCRIPT_ITEM_MAX_CHARS,
        );
        let session = fixture_session();
        hook.on_boundary_reduction(&session, &fixture_reduction())
            .await;

        let rows = wait_for_shadow_rows(&pool, "u1").await;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.conversation_id, "conv-1");
        assert_eq!(row.turn_index, 1);
        assert_eq!(row.trigger_caliber, "full");
        assert_eq!(row.pinned_tokens, 100);
        assert_eq!(row.committed_full_tokens, 500);
        assert_eq!(row.committed_view_tokens, 120);
        // 窗口口径由钩子按 filter 算法自算：夹具只有 user 可见（"hi" → 1 token），
        // 预算充足 ⇒ 连续窗口口径 == 全量 view
        assert_eq!(row.view_window_tokens, 1);
        assert_eq!(row.fold_triggered, 0);
        assert_eq!(row.evicted_turns, None, "无折叠时驱逐轮数为 NULL");
        assert_eq!(row.strip_evaluated, 1);
        assert_eq!(row.strip_enforced, 0, "shadow 只评估不改写");
        assert_eq!(row.stripped_items, 3);
        assert_eq!(row.released_tokens, 42);
        // tool 观测：2 条输出，其中 2500 字符的一条超默认上限
        assert_eq!(row.tool_output_items, 2);
        assert_eq!(row.tool_output_over_cap, 1);
        // 图片观测：1 条 Parts 含图，data URI 15 字节
        assert_eq!(row.image_output_items, 1);
        assert_eq!(
            row.image_base64_bytes,
            "data:image/png;base64,QUJD".len() as i64
        );
        assert!(row.snapshot_json_bytes > 0);

        // 观测行不得写进折叠日志
        let folds = compaction_log::list_by_conversation(&pool, "u1", "conv-1")
            .await
            .unwrap();
        assert!(folds.is_empty(), "观测行不得写进 peco_compaction_log");
    }

    #[tokio::test]
    async fn shadow_hook_gate_off_writes_nothing() {
        let (pool, _dir) = test_pool().await;

        let hook = ContextShadowHook::new(
            pool.clone(),
            "u1",
            "conv-1",
            12_000,
            false,
            peco_core::agent::TRANSCRIPT_ITEM_MAX_CHARS,
        );
        hook.on_boundary_reduction(&fixture_session(), &fixture_reduction())
            .await;
        // 写入若被走到，spawn 的 insert 远快于一次 sleep — 留窗观察
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let rows = context_shadow::list_by_user(&pool, "u1").await.unwrap();
        assert!(rows.is_empty(), "gate 关闭不得落观测行");
    }

    /// enforce 生效时 `strip_enforced` 跟随生效值（实际改写才置 1）。
    #[tokio::test]
    async fn shadow_hook_strip_enforced_follows_outcome() {
        let (pool, _dir) = test_pool().await;

        let hook = ContextShadowHook::new(
            pool.clone(),
            "u1",
            "conv-1",
            12_000,
            true,
            peco_core::agent::TRANSCRIPT_ITEM_MAX_CHARS,
        );
        let session = fixture_session();
        let enforced = BoundaryReduction {
            strip: Some(StripOutcome {
                changed_items: 3,
                released_tokens: 42,
                skipped_turns: 0,
            }),
            ..fixture_reduction()
        };
        hook.on_boundary_reduction(&session, &enforced).await;
        hook.on_boundary_reduction(&session, &fixture_reduction())
            .await;

        let rows = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let rows = context_shadow::list_by_user(&pool, "u1").await.unwrap();
                if rows.len() >= 2 {
                    break rows;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("two shadow rows should appear");
        // 后台写入顺序不保证 —— 按 strip_enforced 值分类断言
        let enforced_flags: Vec<i64> = rows.iter().map(|r| r.strip_enforced).collect();
        assert!(
            enforced_flags.contains(&1),
            "实际改写必须置 strip_enforced=1"
        );
        assert!(enforced_flags.contains(&0), "零改写保持 strip_enforced=0");
    }

    /// purge 只删过期的：保留期写在配置里，删多了等于丢观测数据。
    #[tokio::test]
    async fn purge_expired_shadow_keeps_rows_inside_retention() {
        let (pool, _dir) = test_pool().await;

        let row = |id: &str, created_at: &str| context_shadow::ContextShadowRow {
            id: id.to_string(),
            user_id: "u1".to_string(),
            conversation_id: "conv-1".to_string(),
            turn_index: 1,
            created_at: created_at.to_string(),
            trigger_caliber: "full".to_string(),
            pinned_tokens: 1,
            committed_full_tokens: 2,
            committed_view_tokens: 3,
            view_window_tokens: 4,
            fold_triggered: 0,
            evicted_turns: None,
            guard_triggered: 0,
            strip_evaluated: 0,
            strip_enforced: 0,
            stripped_items: 0,
            released_tokens: 0,
            tool_output_items: 0,
            tool_output_over_cap: 0,
            image_output_items: 0,
            image_base64_bytes: 0,
            snapshot_json_bytes: 0,
        };
        let old = (Utc::now() - chrono::Duration::days(40)).to_rfc3339();
        let fresh = (Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        context_shadow::insert(&pool, &row("old", &old))
            .await
            .unwrap();
        context_shadow::insert(&pool, &row("fresh", &fresh))
            .await
            .unwrap();

        purge_expired_shadow(&pool, 30).await;

        let rows = context_shadow::list_by_user(&pool, "u1").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "fresh");
    }
}
