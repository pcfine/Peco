// ============================================================================
// CompactionMetricsHook / ContextShadowHook — 压缩日志与边界观测钩子
// ============================================================================
//
// CompactionMetricsHook 实现 LooperHook::on_context_compacted，将每次滚动
// 压缩的结果追加写入 peco_compaction_log 表，供 GET /api/peco/session 的
// context_metrics 汇总（压缩次数 / token 时间线 / 摘要长度曲线）。
//
// ContextShadowHook 实现 LooperHook::on_boundary_reduction，每个 turn 边界
// 写一行 peco_context_shadow —— 折叠日志只记折叠，观测表记全部边界，
// 两者互补（无折叠的边界只有观测表能承载）。
//
// 可观测性数据绝不影响主流程 — 写入失败仅 warn。

use async_trait::async_trait;
use model_provider::{Content, ContentPart, InputItem};
use peco_core::agent::hooks::LooperHook;
use peco_core::agent::{
    BoundaryReduction, CompactionOutcome, TRANSCRIPT_ITEM_MAX_CHARS, estimate_item_tokens,
};
use peco_core::session::{Session, SessionSnapshot};
use sqlx::SqlitePool;
use tracing::warn;

use crate::db::{compaction_log, context_shadow};
use crate::peco::filter::select_history_turns;

/// 压缩日志钩子。
///
/// 每个 Peco 会话 looper 一个实例（PecoManager 装配），
/// user_id 与 conversation_id 在构造期固定。
pub struct CompactionMetricsHook {
    pool: SqlitePool,
    user_id: String,
    conversation_id: String,
}

impl CompactionMetricsHook {
    /// 创建新的压缩日志钩子。
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
        // SQLite 写入放后台 — hook 约定不阻塞 turn 边界（对齐 MemoryExtractionHook）。
        // 失败仅记日志，绝不影响主流程。
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

/// 边界观测钩子：每个 turn 边界向 peco_context_shadow 写一行。
///
/// 与压缩日志钩子分离：折叠日志只记折叠，观测表记全部边界 ——
/// 混进折叠日志会让「无折叠的边界」无处安放，shadow 观测即失真。
///
/// `enabled` 由装配方按 gate（shadow 观测开关）传入：钩子恒注册、
/// gate 关时首行返回 —— 行为等价于不装配，但装配点无需分支。
pub struct ContextShadowHook {
    pool: SqlitePool,
    user_id: String,
    conversation_id: String,
    history_token_budget: usize,
    enabled: bool,
}

impl ContextShadowHook {
    /// 创建边界观测钩子。
    ///
    /// * `history_token_budget` — 与 filter 同源的 Verbatim 窗口预算
    ///   （`view_window_tokens` 列 = 该预算下的连续窗口 view 口径）
    /// * `enabled` — shadow gate；`false` 时钩子不写任何行
    pub fn new(
        pool: SqlitePool,
        user_id: impl Into<String>,
        conversation_id: impl Into<String>,
        history_token_budget: usize,
        enabled: bool,
    ) -> Self {
        Self {
            pool,
            user_id: user_id.into(),
            conversation_id: conversation_id.into(),
            history_token_budget,
            enabled,
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
                    if output.text_view().chars().count() > TRANSCRIPT_ITEM_MAX_CHARS {
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
            created_at: chrono::Utc::now().to_rfc3339(),
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

// ============================================================================
// 会话上下文估算（GET /api/peco/session 的 context_metrics 来源）
// ============================================================================

/// 快照上下文的估算 token（两口径，与 [`crate::peco::config::PecoConfig`]
/// 注释中的口径一一对应）。
#[derive(Debug, Clone, Copy)]
pub struct EstimatedContext {
    /// pinned 摘要的估算 token。
    pub pinned_tokens: usize,
    /// 压缩触发口径：pinned + 全部 committed 轮（含 tool 输出与 reasoning）。
    pub total_tokens: usize,
    /// Verbatim 预算口径：历史轮中 viewable（User/Assistant 文本）条目，
    /// 从最新轮往回整轮计入，超出 `history_token_budget` 即停止
    /// （连续窗口，与 [`crate::peco::filter::select_history_turns`] 同一算法）。
    pub view_tokens: usize,
    /// 全部 committed 轮的 viewable 条目之和（V，不设窗口预算）。
    /// 与 [`Self::view_tokens`] 的差值即窗口裁掉的部分。
    pub viewable_backlog_tokens: usize,
    /// 非 viewable 内容（tool 输出、工具调用参数、reasoning）之和（F − V）。
    /// 恒等式：`viewable_backlog_tokens + non_viewable_tokens == total_tokens - pinned_tokens`。
    pub non_viewable_tokens: usize,
}

/// 估算快照上下文 token（与 compaction / filter 相同的校准估算器，防漂移）。
///
/// * `history_token_budget` — Verbatim 层预算（`PecoConfig::history_token_budget`）。
pub fn estimate_session_context(
    snapshot: &SessionSnapshot,
    history_token_budget: usize,
) -> EstimatedContext {
    let pinned_tokens = snapshot
        .pinned_summary
        .as_ref()
        .map(|am| estimate_item_tokens(&am.message))
        .unwrap_or(0);

    // 一次遍历同时结算全量与 viewable 两口径 —— 分两遍扫既慢又有
    // 两遍口径漂移的风险（恒等式只有同遍计算才天然成立）
    let mut total_tokens = pinned_tokens;
    let mut viewable_backlog_tokens = 0usize;
    for turn in &snapshot.committed_turns {
        for am in turn {
            total_tokens += estimate_item_tokens(&am.message);
            viewable_backlog_tokens += peco_core::agent::estimate_viewable_item_tokens(&am.message);
        }
    }

    // Verbatim 预算口径：与 PecoContextFilter 共用同一选择算法，
    // 保证指标显示的占用与模型实际收到的上下文一致。
    let (_, view_tokens) = select_history_turns(&snapshot.committed_turns, history_token_budget);

    // F − V：viewable 条目必然也是全量条目的一部分，减法不会下溢
    EstimatedContext {
        pinned_tokens,
        total_tokens,
        view_tokens,
        non_viewable_tokens: (total_tokens - pinned_tokens) - viewable_backlog_tokens,
        viewable_backlog_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_provider::{Content, ContentPart, InputItem, Role};
    use peco_core::session::{AnnotatedMessage, MessageId, MessageSource};

    fn annotated(turn: usize, item: InputItem) -> AnnotatedMessage {
        AnnotatedMessage::new(MessageId(0), turn, item, MessageSource::UserInput)
    }
    fn user(text: &str) -> InputItem {
        InputItem::Message {
            role: Role::User,
            content: text.into(),
        }
    }
    fn assistant(text: &str) -> InputItem {
        InputItem::Message {
            role: Role::Assistant,
            content: text.into(),
        }
    }
    fn tool_output(text: &str) -> InputItem {
        InputItem::FunctionCallOutput {
            call_id: "c1".into(),
            output: text.into(),
        }
    }
    fn pinned(text: &str) -> AnnotatedMessage {
        AnnotatedMessage::new(
            MessageId(0),
            0,
            InputItem::Message {
                role: Role::System,
                content: text.into(),
            },
            MessageSource::SystemInjection {
                reason: "compaction".to_string(),
            },
        )
    }

    fn snapshot(
        turns: Vec<Vec<AnnotatedMessage>>,
        pinned: Option<AnnotatedMessage>,
    ) -> SessionSnapshot {
        let turn_index = turns.len();
        SessionSnapshot {
            committed_turns: turns,
            turn_index,
            total_usage: model_provider::Usage::default(),
            next_message_id: 0,
            pending_inputs: Vec::new(),
            pinned_summary: pinned,
        }
    }

    #[test]
    fn test_estimate_two_calibers() {
        // 10 CJK 字 ≈ 6 token；tool 输出 20 ASCII ≈ 6 token（仅计入全量口径）
        let snap = snapshot(
            vec![
                vec![annotated(0, user("一二三四五六七八九十"))],
                vec![
                    annotated(1, assistant("一二三四五六七八九十")),
                    annotated(1, tool_output(&"a".repeat(20))),
                ],
            ],
            Some(pinned("一二三四五六七八九十")),
        );

        let est = estimate_session_context(&snap, 12_000);
        assert_eq!(est.pinned_tokens, 6);
        // 全量口径 = pinned 6 + (6) + (6 + 6)
        assert_eq!(est.total_tokens, 24);
        // Verbatim 口径 = 全部轮的 viewable（6 + 6），tool 输出不计入
        assert_eq!(est.view_tokens, 12);
    }

    #[test]
    fn test_view_stops_at_budget_strictly() {
        let snap = snapshot(
            vec![
                vec![annotated(0, user("一二三四五六七八九十"))], // 6
                vec![annotated(1, assistant("一二三四五六七八九十"))], // 6
            ],
            None,
        );
        // 预算 8 → 仅最近一轮（6）计入；turn 0（6 > 剩余 2）整轮不计
        let est = estimate_session_context(&snap, 8);
        assert_eq!(est.view_tokens, 6);
        // 全量口径不受预算影响
        assert_eq!(est.total_tokens, 12);
    }

    #[test]
    fn test_view_selection_matches_filter_window_semantics() {
        // 不等大小轮次 [3, 10, 6]、预算 12：filter 从最新往回整轮选择，
        // 轮 1（10 > 剩余 6）触发停止 — 指标必须与 filter 同算法（连续窗口），
        // 只计最新轮的 6，而非正序跳过累加的 3 + 6 = 9。
        let snap = snapshot(
            vec![
                vec![annotated(0, user("一二三四五"))],           // 5 CJK ≈ 3
                vec![annotated(1, user(&"一".repeat(16)))],       // ≈ 10
                vec![annotated(2, user("一二三四五六七八九十"))], // ≈ 6
            ],
            None,
        );
        let est = estimate_session_context(&snap, 12);
        assert_eq!(est.view_tokens, 6);
        assert_eq!(est.total_tokens, 19);
    }

    #[tokio::test]
    async fn test_hook_writes_compaction_log() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();

        let hook = CompactionMetricsHook::new(pool.clone(), "u1", "u1-private-session");
        let outcome = CompactionOutcome {
            evicted_turns: 4,
            summary: "## 已做决定\n- 采用方案 A".to_string(),
            estimated_tokens_before: 24_500,
            estimated_tokens_after: 8_800,
            degraded: false,
        };
        hook.on_context_compacted(&outcome).await;

        // 写入在后台任务中执行 — 轮询等待落库
        let rows = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let rows = crate::db::compaction_log::list_by_conversation(
                    &pool,
                    "u1",
                    "u1-private-session",
                )
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
        assert_eq!(rows[0].conversation_id, "u1-private-session");
        assert_eq!(rows[0].evicted_turns, 4);
        assert_eq!(rows[0].tokens_before, 24_500);
        assert_eq!(rows[0].tokens_after, 8_800);
        // 摘要长度按字符数统计（非字节数）
        assert_eq!(
            rows[0].summary_chars,
            "## 已做决定\n- 采用方案 A".chars().count() as i64
        );
    }

    /// 可变红夹具：pinned 摘要 + 一轮 [User 文本, Reasoning, tool 输出] —
    /// 两个拆分字段逐字手算，任何一个被漏算或重复计数都会红。
    #[test]
    fn test_estimate_viewable_split_fields_are_hand_computable() {
        let snap = snapshot(
            vec![vec![
                annotated(0, user("一二三四五六七八九十")), // 10 CJK → 6（viewable）
                annotated(
                    0,
                    InputItem::Reasoning {
                        content: "思考思考思考".to_string(),
                    },
                ), // 6 CJK → 4（非 viewable）
                annotated(0, tool_output(&"a".repeat(20))), // 20 ASCII → 6（非 viewable）
            ]],
            Some(pinned("一二三四五六七八九十")), // 6
        );

        let est = estimate_session_context(&snap, 12_000);
        assert_eq!(est.pinned_tokens, 6);
        assert_eq!(est.total_tokens, 6 + 6 + 4 + 6);
        // V = 仅 User 文本；F − V = reasoning + tool 输出
        assert_eq!(est.viewable_backlog_tokens, 6);
        assert_eq!(est.non_viewable_tokens, 4 + 6);
        // 恒等式：V + (F − V) == F
        assert_eq!(
            est.viewable_backlog_tokens + est.non_viewable_tokens,
            est.total_tokens - est.pinned_tokens
        );
        // 窗口口径 ≤ 全量 view 口径（本夹具预算充足，两者相等）
        assert_eq!(est.view_tokens, est.viewable_backlog_tokens);
    }

    /// 夹具会话：user + reasoning + 超转录上限的 tool 输出 + 含图 Parts 输出。
    fn shadow_fixture_session() -> Session {
        let mut s = Session::new("shadow-s".to_string(), "shadow".to_string());
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
            strip_evaluation: Some(peco_core::agent::StripEvaluation {
                would_change_items: 3,
                would_release_tokens: 42,
                skipped_turns: 1,
            }),
            compaction: None,
        }
    }

    /// enforce 生效时 `strip_enforced` 跟随生效值（实际改写才置 1）。
    fn fixture_reduction_enforced(changed_items: usize) -> BoundaryReduction {
        BoundaryReduction {
            strip: Some(peco_core::agent::StripOutcome {
                changed_items,
                released_tokens: 42,
                skipped_turns: 0,
            }),
            ..fixture_reduction()
        }
    }

    /// shadow 单行各列正确（I9：不碰 peco_compaction_log）。
    #[tokio::test]
    async fn test_shadow_hook_writes_row_and_leaves_compaction_log_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();

        let hook = ContextShadowHook::new(pool.clone(), "u1", "u1-private-session", 12_000, true);
        let session = shadow_fixture_session();
        hook.on_boundary_reduction(&session, &fixture_reduction())
            .await;

        let rows = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let rows = crate::db::context_shadow::list_by_user(&pool, "u1")
                    .await
                    .unwrap();
                if !rows.is_empty() {
                    break rows;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("shadow row should appear in background");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.conversation_id, "u1-private-session");
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
        assert_eq!(row.guard_triggered, 0);
        assert_eq!(row.strip_evaluated, 1);
        assert_eq!(row.strip_enforced, 0, "shadow 只评估不改写");
        assert_eq!(row.stripped_items, 3);
        assert_eq!(row.released_tokens, 42);
        // tool 观测：2 条输出，其中 2500 字符的一条超 2000 上限
        assert_eq!(row.tool_output_items, 2);
        assert_eq!(row.tool_output_over_cap, 1);
        // 图片观测：1 条 Parts 含图，data URI 15 字节
        assert_eq!(row.image_output_items, 1);
        assert_eq!(
            row.image_base64_bytes,
            "data:image/png;base64,QUJD".len() as i64
        );
        assert!(row.snapshot_json_bytes > 0);

        // I9：写观测行不产生折叠日志
        let compaction_rows =
            crate::db::compaction_log::list_by_conversation(&pool, "u1", "u1-private-session")
                .await
                .unwrap();
        assert!(
            compaction_rows.is_empty(),
            "观测行不得写进 peco_compaction_log"
        );
    }

    /// enforce 生效值：实际改写 → `strip_enforced=1`；零改写 → 0。
    #[tokio::test]
    async fn test_shadow_hook_strip_enforced_follows_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();

        let hook = ContextShadowHook::new(pool.clone(), "u2", "u2-private-session", 12_000, true);
        let session = shadow_fixture_session();
        hook.on_boundary_reduction(&session, &fixture_reduction_enforced(3))
            .await;
        hook.on_boundary_reduction(&session, &fixture_reduction_enforced(0))
            .await;

        let rows = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let rows = crate::db::context_shadow::list_by_user(&pool, "u2")
                    .await
                    .unwrap();
                if rows.len() >= 2 {
                    break rows;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("two shadow rows should appear");
        // 后台写入顺序不保证 —— 按 strip_enforced 值分类断言
        let enforced: Vec<i64> = rows.iter().map(|r| r.strip_enforced).collect();
        assert!(enforced.contains(&1), "实际改写必须置 strip_enforced=1");
        assert!(enforced.contains(&0), "零改写保持 strip_enforced=0");
        assert!(
            rows.iter().all(|r| r.strip_evaluated == 1),
            "enforce 路径同样产出评估值"
        );
    }

    /// gate 关闭时不落任何观测行。
    #[tokio::test]
    async fn test_shadow_hook_gate_off_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();

        let hook = ContextShadowHook::new(pool.clone(), "u1", "u1-private-session", 12_000, false);
        let session = shadow_fixture_session();
        hook.on_boundary_reduction(&session, &fixture_reduction())
            .await;
        // 写入路径若被走到，spawn 的 insert 远快于一次 sleep — 留窗观察
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let rows = crate::db::context_shadow::list_by_user(&pool, "u1")
            .await
            .unwrap();
        assert!(rows.is_empty(), "gate 关闭不得落观测行");
    }
}
