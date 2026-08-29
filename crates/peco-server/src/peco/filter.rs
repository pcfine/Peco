// ============================================================================
// PecoContextFilter — Peco 永续对话的统一上下文组装器
// ============================================================================
//
// 单一截断点设计（参照 OpenHands 的 Condenser 抽象）：
// 全部「选哪些消息进入上下文」的决策集中于此过滤器，LooperConfig 的
// context_strategy 保持 FullHistory 直通，避免 filter + strategy 双层窗口
// 职责重叠。
//
// 组装规则：
//   1. 钉扎层：pinned 摘要（compaction 产物，Role::System）恒保留在最前；
//   2. Verbatim 层：历史轮按「校准 token 预算」从最新往回整轮保留，
//      轮内仅保留 User / Assistant 文本（丢弃 tool 过程与 reasoning）；
//   3. 当前轮：完整保留（含 tool_call / tool_result / reasoning）。

use model_provider::{InputItem, Role};
use peco_core::agent::MessageFilter;
use peco_core::agent::estimate_item_tokens;
use peco_core::session::{AnnotatedMessage, MessageSource};

/// Peco 永续对话专用上下文过滤器。
///
/// 相比早期的「10 条消息滑动窗口」，以 token 预算整轮选择历史，
/// 并与 compaction 的 pinned 摘要协同（摘要永不驱逐）。
pub struct PecoContextFilter {
    /// 历史轮 verbatim 保留区 token 预算（不含当前轮与 pinned 摘要）。
    history_token_budget: usize,
}

impl PecoContextFilter {
    /// 创建过滤器。
    ///
    /// * `history_token_budget` — 历史轮保留区 token 预算
    pub fn new(history_token_budget: usize) -> Self {
        Self {
            history_token_budget,
        }
    }
}

impl MessageFilter for PecoContextFilter {
    fn filter(&self, messages: &[&AnnotatedMessage]) -> Vec<AnnotatedMessage> {
        if messages.is_empty() {
            return vec![];
        }

        // ── 1. 切分：pinned 摘要 / 历史轮 / 当前轮 ─────────────────
        // 当前轮 = 最后一条消息的 turn_index
        let current_turn = messages.last().unwrap().turn_index;

        let mut pinned: Vec<AnnotatedMessage> = Vec::new();
        // (turn_index, Vec<克隆消息>)
        let mut history_turns: Vec<(usize, Vec<AnnotatedMessage>)> = Vec::new();
        let mut current: Vec<AnnotatedMessage> = Vec::new();

        for m in messages.iter().map(|m| (*m).clone()) {
            // pinned 摘要：按 MessageSource 判定（compaction 的 SystemInjection 标记），
            // 不按 Role 判定 —— 避免未来其他 System/Developer 消息被错误提升到
            // 顶部并绕过预算驱逐。SystemInjection 目前仅有 compaction 一个生产者。
            if matches!(m.source, MessageSource::SystemInjection { .. }) {
                pinned.push(m);
            } else if m.turn_index >= current_turn {
                current.push(m);
            } else {
                match history_turns.last_mut() {
                    Some((turn, msgs)) if *turn == m.turn_index => msgs.push(m),
                    _ => history_turns.push((m.turn_index, vec![m])),
                }
            }
        }

        // ── 2. 历史轮整轮选择：从最新往回，校准 token 预算 ──────────
        // 严格预算：超预算的历史轮一律裁掉 — 旧信息由 pinned 摘要承载，
        // 不做「至少保留一轮」的降级例外。
        let mut selected: Vec<Vec<AnnotatedMessage>> = Vec::new();
        let mut budget = self.history_token_budget;
        for (_, turn_msgs) in history_turns.iter().rev() {
            // 轮内 view：仅 User / Assistant 文本（丢弃 tool 过程与 reasoning）
            let view_tokens: usize = turn_msgs
                .iter()
                .filter(|am| is_history_viewable(&am.message))
                .map(|am| estimate_item_tokens(&am.message))
                .sum();
            if view_tokens > budget {
                break;
            }
            budget = budget.saturating_sub(view_tokens);
            selected.push(
                turn_msgs
                    .iter()
                    .filter(|am| is_history_viewable(&am.message))
                    .cloned()
                    .collect(),
            );
        }

        // ── 3. 组装：pinned 摘要 + 历史（旧→新）+ 当前轮 ────────────
        let mut result = Vec::new();
        result.extend(pinned);
        for turn_msgs in selected.into_iter().rev() {
            result.extend(turn_msgs);
        }
        result.extend(current);
        result
    }
}

/// 历史轮中可进入上下文的条目：User / Assistant 纯文本。
///
/// pinned 摘要（SystemInjection）已在切分阶段归入 pinned，不在此列。
fn is_history_viewable(item: &InputItem) -> bool {
    matches!(
        item,
        InputItem::Message {
            role: Role::User | Role::Assistant,
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use peco_core::agent::estimate_str_tokens;
    use peco_core::session::{MessageId, MessageSource};

    fn user(text: impl Into<String>) -> InputItem {
        InputItem::Message {
            role: Role::User,
            content: text.into(),
        }
    }
    fn assistant(text: impl Into<String>) -> InputItem {
        InputItem::Message {
            role: Role::Assistant,
            content: text.into(),
        }
    }
    fn system(text: impl Into<String>) -> InputItem {
        InputItem::Message {
            role: Role::System,
            content: text.into(),
        }
    }
    fn function_call(call_id: &str, name: &str) -> InputItem {
        InputItem::FunctionCall {
            call_id: call_id.into(),
            name: name.into(),
            arguments: "ls".into(),
        }
    }
    fn function_call_output(call_id: &str) -> InputItem {
        InputItem::FunctionCallOutput {
            call_id: call_id.into(),
            output: "out".into(),
        }
    }

    fn make_annotated(turn: usize, msg: InputItem) -> AnnotatedMessage {
        AnnotatedMessage::new(MessageId(0), turn, msg, MessageSource::UserInput)
    }

    fn make_pinned(msg: InputItem) -> AnnotatedMessage {
        AnnotatedMessage::new(
            MessageId(0),
            0,
            msg,
            MessageSource::SystemInjection {
                reason: "compaction".to_string(),
            },
        )
    }

    #[test]
    fn test_empty_messages() {
        let filter = PecoContextFilter::new(10_000);
        let refs: Vec<&AnnotatedMessage> = vec![];
        assert!(filter.filter(&refs).is_empty());
    }

    #[test]
    fn test_pinned_summary_always_kept_first() {
        let filter = PecoContextFilter::new(1); // 预算为 0 也不驱逐摘要
        let msgs = vec![
            make_pinned(system(
                "<earlier_context_summary>摘要</earlier_context_summary>",
            )),
            make_annotated(0, user("旧问题")),
            make_annotated(0, assistant("旧回答")),
            make_annotated(1, user("新问题")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // 历史 0 轮因预算被裁掉，但 pinned 摘要 + 当前轮仍在
        assert_eq!(result.len(), 2);
        assert!(matches!(
            result[0].message.as_ref(),
            InputItem::Message {
                role: Role::System,
                ..
            }
        ));
        assert!(matches!(
            result[1].message.as_ref(),
            InputItem::Message {
                role: Role::User,
                ..
            }
        ));
    }

    #[test]
    fn test_non_injection_system_message_not_pinned() {
        // Role::System 但来源不是 SystemInjection 的消息不得被提升为 pinned —
        // pinned 判定依据 MessageSource，而非 Role。
        let filter = PecoContextFilter::new(10_000);
        let msgs = vec![
            make_annotated(0, system("普通 system 消息")),
            make_annotated(1, user("问题")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // 历史 turn 0 的 system 消息不可见（仅 User/Assistant）→ 丢弃；仅剩当前轮
        assert_eq!(result.len(), 1);
        assert!(matches!(
            result[0].message.as_ref(),
            InputItem::Message {
                role: Role::User,
                ..
            }
        ));
    }

    #[test]
    fn test_current_turn_keeps_full_tool_context() {
        let filter = PecoContextFilter::new(10_000);
        let msgs = vec![
            make_annotated(0, user("旧问题")),
            make_annotated(0, assistant("旧回答")),
            make_annotated(1, user("新问题")),
            make_annotated(1, assistant("run")),
            make_annotated(1, function_call("c1", "shell")),
            make_annotated(1, function_call_output("c1")),
            make_annotated(1, assistant("done")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // 历史: User + Asst(text) = 2, 当前: 5
        assert_eq!(result.len(), 7);
    }

    #[test]
    fn test_history_budget_evicts_oldest_turns_whole() {
        let filter = PecoContextFilter::new(25);
        // 每轮 2 条中文消息 ≈ 20 token（校准估算：15 CJK × 0.6 + 3 ASCII × 0.3 ≈ 10/条）；
        // 预算 25 → 历史只保留最近 1 轮，更早轮整轮裁掉
        let msgs: Vec<AnnotatedMessage> = (0..3)
            .flat_map(|i| {
                vec![
                    make_annotated(i, user(format!("这是第 {i} 轮的提问内容，长度足够。"))),
                    make_annotated(i, assistant(format!("这是第 {i} 轮的回答内容，同样足够。"))),
                ]
            })
            .collect();
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // turn 1（历史，20 ≤ 25）保留；turn 0（20 > 剩余 5）整轮裁掉；
        // turn 2 为当前轮完整保留 → 共 4 条
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].turn_index, 1);
        assert_eq!(result[2].turn_index, 2);
    }

    #[test]
    fn test_history_drops_tool_items_but_keeps_text() {
        let filter = PecoContextFilter::new(10_000);
        let msgs = vec![
            make_annotated(0, user("查天气")),
            make_annotated(0, assistant("让我看看")),
            make_annotated(0, function_call("c1", "fetch")),
            make_annotated(0, function_call_output("c1")),
            make_annotated(0, assistant("今天晴天")),
            make_annotated(1, user("谢谢")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // 历史 turn 0 仅 User + 2 条 Assistant 文本，当前轮 1 条
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn test_calibrated_estimator_weights_cjk() {
        // 10 个汉字 ≈ 6 token（0.6/字），10 个 ASCII ≈ 3 token（0.3/字）
        assert_eq!(estimate_str_tokens("一二三四五六七八九十"), 6);
        assert_eq!(estimate_str_tokens("abcdefghij"), 3);
        // 混合：ceil(6.0) + ceil(0.9) = 6 + 1 = 7
        assert_eq!(estimate_str_tokens("一二三四五六七八九十abc"), 7);
    }
}
