// ============================================================================
// CompactionMetricsHook — 上下文压缩日志钩子
// ============================================================================
//
// 实现 LooperHook::on_context_compacted，将每次滚动压缩的结果
// 追加写入 peco_compaction_log 表，供 GET /api/peco/session 的
// context_metrics 汇总（压缩次数 / token 时间线 / 摘要长度曲线）。
//
// 可观测性数据绝不影响主流程 — 写入失败仅 warn。

use async_trait::async_trait;
use peco_core::agent::hooks::LooperHook;
use peco_core::agent::{CompactionOutcome, estimate_item_tokens};
use peco_core::session::SessionSnapshot;
use sqlx::SqlitePool;
use tracing::warn;

use crate::db::compaction_log;
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

    // 压缩触发口径：pinned + 全部 committed 轮整轮全量
    let mut total_tokens = pinned_tokens;
    for turn in &snapshot.committed_turns {
        total_tokens += turn
            .iter()
            .map(|am| estimate_item_tokens(&am.message))
            .sum::<usize>();
    }

    // Verbatim 预算口径：与 PecoContextFilter 共用同一选择算法，
    // 保证指标显示的占用与模型实际收到的上下文一致。
    let (_, view_tokens) = select_history_turns(&snapshot.committed_turns, history_token_budget);

    EstimatedContext {
        pinned_tokens,
        total_tokens,
        view_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_provider::{InputItem, Role};
    use peco_core::session::{AnnotatedMessage, MessageId, MessageSource};

    fn annotated(turn: usize, item: InputItem) -> AnnotatedMessage {
        AnnotatedMessage::new(MessageId(0), turn, item, MessageSource::UserInput)
    }
    fn user(text: &str) -> InputItem {
        InputItem::Message {
            role: Role::User,
            content: text.to_string(),
        }
    }
    fn assistant(text: &str) -> InputItem {
        InputItem::Message {
            role: Role::Assistant,
            content: text.to_string(),
        }
    }
    fn tool_output(text: &str) -> InputItem {
        InputItem::FunctionCallOutput {
            call_id: "c1".into(),
            output: text.to_string(),
        }
    }
    fn pinned(text: &str) -> AnnotatedMessage {
        AnnotatedMessage::new(
            MessageId(0),
            0,
            InputItem::Message {
                role: Role::System,
                content: text.to_string(),
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
}
