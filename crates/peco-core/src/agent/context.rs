// ============================================================================
// Agent 层上下文构建器
// ============================================================================
//
// 从 Session 的消息引用列表构建 LLM 请求的消息上下文。
// 不同 Agent 可通过配置不同的 ContextStrategy 控制上下文行为。
//
// Session 的 system prompt 在此外层注入，不存入 Session。

use std::sync::Arc;

use model_provider::Message;

use crate::session::AnnotatedMessage;

// ============================================================================
// ContextStrategy
// ============================================================================

/// 上下文构建策略。
///
/// 通过 `LooperConfig::context_strategy` 配置。
#[derive(Clone)]
pub enum ContextStrategy {
    /// 滑动窗口：保留最近 N 轮完整 turn
    SlidingWindow { max_turns: usize },
    /// Token 预算：保留不超过 max_tokens 的消息
    TokenBudget {
        max_tokens: usize,
        /// 超出预算时是否用摘要替代早期 turn（Phase 4 实现）
        summarize_overflow: bool,
    },
    /// 全量历史（默认）
    FullHistory,
    /// 自定义：外部注入的消息截断逻辑
    Custom(Arc<dyn ContextFilter>),
}

impl std::fmt::Debug for ContextStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SlidingWindow { max_turns } => f
                .debug_struct("SlidingWindow")
                .field("max_turns", max_turns)
                .finish(),
            Self::TokenBudget {
                max_tokens,
                summarize_overflow,
            } => f
                .debug_struct("TokenBudget")
                .field("max_tokens", max_tokens)
                .field("summarize_overflow", summarize_overflow)
                .finish(),
            Self::FullHistory => write!(f, "FullHistory"),
            Self::Custom(_) => write!(f, "Custom(...)"),
        }
    }
}

// ============================================================================
// ContextFilter
// ============================================================================

/// 自定义上下文过滤器。
///
/// 实现者负责从消息引用列表中选择/转换需要发送给 LLM 的消息。
pub trait ContextFilter: Send + Sync {
    /// 从消息引用和可选的 system prompt 构建 `ContextResult`。
    fn apply(&self, messages: &[&AnnotatedMessage], system_prompt: Option<&str>) -> ContextResult;
}

// ============================================================================
// ContextResult
// ============================================================================

/// 上下文构建结果。
#[derive(Debug, Clone)]
pub struct ContextResult {
    /// 最终发送给 LLM 的消息列表（Arc 共享所有权，零拷贝上下文构建）
    pub messages: Vec<Arc<Message>>,
    /// 实际包含的 turn 数量
    pub turns_included: usize,
    /// 估算 token 数（粗略估计：每字符 ~0.3 token）
    pub estimated_tokens: usize,
    /// 是否有消息被截断
    pub truncated: bool,
}

// ============================================================================
// build_context
// ============================================================================

/// 从消息引用和策略构建 LLM 上下文。
///
/// `system_prompt` 由 Agent 提供（不存储在 Session 中）。
/// 若提供，将作为第一条 `Message::System` 插入。
///
/// # 示例
///
/// ```ignore
/// let refs: Vec<&AnnotatedMessage> = session.all_message_refs().collect();
/// let ctx = build_context(&refs, agent.system_prompt().as_deref(), &config.context_strategy);
/// ```
pub fn build_context(
    messages: &[&AnnotatedMessage],
    system_prompt: Option<&str>,
    strategy: &ContextStrategy,
) -> ContextResult {
    match strategy {
        ContextStrategy::SlidingWindow { max_turns } => {
            build_sliding_window(messages, system_prompt, *max_turns)
        }
        ContextStrategy::TokenBudget {
            max_tokens,
            summarize_overflow,
        } => build_token_budget(messages, system_prompt, *max_tokens, *summarize_overflow),
        ContextStrategy::FullHistory => build_full_history(messages, system_prompt),
        ContextStrategy::Custom(filter) => filter.apply(messages, system_prompt),
    }
}

// ============================================================================
// 内部辅助
// ============================================================================

/// 构建全量历史（默认策略）。
fn build_full_history(
    messages: &[&AnnotatedMessage],
    system_prompt: Option<&str>,
) -> ContextResult {
    let mut result: Vec<Arc<Message>> =
        Vec::with_capacity(messages.len() + system_prompt.iter().count());

    if let Some(prompt) = system_prompt {
        result.push(Arc::new(Message::system(prompt)));
    }

    let turn_set: std::collections::BTreeSet<usize> =
        messages.iter().map(|am| am.turn_index).collect();

    for am in messages {
        result.push(Arc::clone(&am.message));
    }

    let estimated_tokens = estimate_tokens_arc(&result);

    ContextResult {
        messages: result,
        turns_included: turn_set.len(),
        estimated_tokens,
        truncated: false,
    }
}

/// 构建滑动窗口上下文。
///
/// 保留最近 `max_turns` 轮完整 turn。
fn build_sliding_window(
    messages: &[&AnnotatedMessage],
    system_prompt: Option<&str>,
    max_turns: usize,
) -> ContextResult {
    if max_turns == 0 {
        // 0 窗口：仅 system prompt
        let mut result: Vec<Arc<Message>> = Vec::new();
        if let Some(prompt) = system_prompt {
            result.push(Arc::new(Message::system(prompt)));
        }
        return ContextResult {
            messages: result,
            turns_included: 0,
            estimated_tokens: system_prompt.map(|s| s.len() / 3).unwrap_or(0),
            truncated: !messages.is_empty(),
        };
    }

    // 找到最大的 turn_index
    let max_turn = messages.iter().map(|am| am.turn_index).max().unwrap_or(0);
    // 计算起始 turn（不能下溢）
    let start_turn = max_turn.saturating_sub(max_turns.saturating_sub(1));

    let truncated = messages.iter().any(|am| am.turn_index < start_turn);

    let mut result: Vec<Arc<Message>> = Vec::new();
    if let Some(prompt) = system_prompt {
        result.push(Arc::new(Message::system(prompt)));
    }

    let mut turns_included = 0usize;
    let mut last_turn = None;
    for am in messages {
        if am.turn_index >= start_turn {
            result.push(Arc::clone(&am.message));
            if last_turn != Some(am.turn_index) {
                turns_included += 1;
                last_turn = Some(am.turn_index);
            }
        }
    }

    let estimated_tokens = estimate_tokens_arc(&result);

    ContextResult {
        messages: result,
        turns_included,
        estimated_tokens,
        truncated,
    }
}

/// 构建 Token 预算上下文。
///
/// 从最新 turn 向前累积消息，直到超出 `max_tokens` 预算。
/// 始终保留 system prompt（若提供）和 at least 1 个完整 turn。
/// 当 `summarize_overflow` 为 true 时，被截断的早期 turn 用占位摘要替代。
fn build_token_budget(
    messages: &[&AnnotatedMessage],
    system_prompt: Option<&str>,
    max_tokens: usize,
    summarize_overflow: bool,
) -> ContextResult {
    if messages.is_empty() {
        let mut result: Vec<Arc<Message>> = Vec::new();
        if let Some(prompt) = system_prompt {
            result.push(Arc::new(Message::system(prompt)));
        }
        return ContextResult {
            estimated_tokens: estimate_tokens_arc(&result),
            messages: result,
            turns_included: 0,
            truncated: false,
        };
    }

    // 收集所有消息到 Vec（按时间顺序）
    let all_messages: Vec<&AnnotatedMessage> = messages.to_vec();

    let max_turn = all_messages
        .iter()
        .map(|am| am.turn_index)
        .max()
        .unwrap_or(0);

    // System prompt 的 token 开销
    let system_tokens: usize = system_prompt
        .map(|s| (s.len() as f64 * 0.3) as usize)
        .unwrap_or(0);
    let mut budget_remaining = max_tokens.saturating_sub(system_tokens);

    // 从后往前扫描，按 turn 分组累积
    // 找到每轮 turn 的起止索引
    let turn_ranges: Vec<(usize, usize, usize)> = {
        // (turn_index, start_idx, end_idx_exclusive)
        let mut ranges = Vec::new();
        let mut current_turn = None;
        let mut start_idx = 0;
        for (i, am) in all_messages.iter().enumerate() {
            if current_turn != Some(am.turn_index) {
                if let Some(turn) = current_turn {
                    ranges.push((turn, start_idx, i));
                }
                current_turn = Some(am.turn_index);
                start_idx = i;
            }
        }
        if let Some(turn) = current_turn {
            ranges.push((turn, start_idx, all_messages.len()));
        }
        ranges
    };

    // 从后往前累积 turn，直到超出预算
    let mut included_turns: Vec<usize> = Vec::new();

    for (turn, start, end) in turn_ranges.iter().rev() {
        let turn_tokens: usize = all_messages[*start..*end]
            .iter()
            .map(|am| match am.message.as_ref() {
                Message::User { content } => (content.len() as f64 * 0.3) as usize,
                Message::Assistant {
                    content,
                    reasoning_content,
                    ..
                } => {
                    let c = content.as_ref().map(|s| s.len()).unwrap_or(0);
                    let r = reasoning_content.as_ref().map(|s| s.len()).unwrap_or(0);
                    ((c + r) as f64 * 0.3) as usize
                }
                Message::Tool { content, .. } => (content.len() as f64 * 0.3) as usize,
                Message::System { content } => (content.len() as f64 * 0.3) as usize,
            })
            .sum();

        if included_turns.is_empty() {
            // 始终包含至少 1 个 turn
            included_turns.push(*turn);
            budget_remaining = budget_remaining.saturating_sub(turn_tokens);
        } else if turn_tokens <= budget_remaining {
            included_turns.push(*turn);
            budget_remaining = budget_remaining.saturating_sub(turn_tokens);
        } else {
            break;
        }
    }

    // 包含的 turn 中最早的那个
    let min_included_turn = included_turns.iter().min().copied().unwrap_or(max_turn);
    let truncated = min_included_turn > 0
        && min_included_turn > all_messages.first().map(|am| am.turn_index).unwrap_or(0);

    // 构建结果
    let mut result: Vec<Arc<Message>> = Vec::new();
    if let Some(prompt) = system_prompt {
        result.push(Arc::new(Message::system(prompt)));
    }

    // 若被截断且 summarize_overflow 启用，插入摘要占位
    if truncated && summarize_overflow {
        let omitted_turns = min_included_turn;
        let summary = format!(
            "[Earlier conversation omitted: {} turn(s) truncated due to token budget]",
            omitted_turns
        );
        result.push(Arc::new(Message::system(summary)));
    }

    for am in &all_messages {
        if am.turn_index >= min_included_turn {
            result.push(Arc::clone(&am.message));
        }
    }

    let estimated_tokens = estimate_tokens_arc(&result);
    let turns_included = included_turns.len();

    ContextResult {
        messages: result,
        turns_included,
        estimated_tokens,
        truncated,
    }
}

/// 粗略 token 估算：每字符约 0.3 token（Arc 版本）。
fn estimate_tokens_arc(messages: &[Arc<Message>]) -> usize {
    let char_count: usize = messages
        .iter()
        .map(|m| match m.as_ref() {
            Message::User { content } => content.len(),
            Message::Assistant {
                content,
                reasoning_content,
                ..
            } => {
                content.as_ref().map(|c| c.len()).unwrap_or(0)
                    + reasoning_content.as_ref().map(|r| r.len()).unwrap_or(0)
            }
            Message::Tool { content, .. } => content.len(),
            Message::System { content } => content.len(),
        })
        .sum();
    (char_count as f64 * 0.3) as usize
}

/// 粗略 token 估算：每字符约 0.3 token（&Message 版本，用于测试）。
#[allow(dead_code)]
fn estimate_tokens(messages: &[Message]) -> usize {
    let char_count: usize = messages
        .iter()
        .map(|m| match m {
            Message::User { content } => content.len(),
            Message::Assistant {
                content,
                reasoning_content,
                ..
            } => {
                content.as_ref().map(|c| c.len()).unwrap_or(0)
                    + reasoning_content.as_ref().map(|r| r.len()).unwrap_or(0)
            }
            Message::Tool { content, .. } => content.len(),
            Message::System { content } => content.len(),
        })
        .sum();
    (char_count as f64 * 0.3) as usize
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{AnnotatedMessage, MessageId, MessageSource};
    use model_provider::Message;

    fn make_annotated(id: u64, turn: usize, msg: Message) -> AnnotatedMessage {
        AnnotatedMessage::new(MessageId(id), turn, msg, MessageSource::UserInput)
    }

    #[test]
    fn test_full_history_no_prompt() {
        let msgs = vec![
            make_annotated(0, 0, Message::user("hello")),
            make_annotated(1, 0, Message::assistant("hi")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = build_context(&refs, None, &ContextStrategy::FullHistory);
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.turns_included, 1);
        assert!(!result.truncated);
    }

    #[test]
    fn test_full_history_with_prompt() {
        let msgs = vec![make_annotated(0, 0, Message::user("hello"))];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = build_context(
            &refs,
            Some("You are helpful."),
            &ContextStrategy::FullHistory,
        );
        assert_eq!(result.messages.len(), 2);
        assert!(matches!(
            result.messages[0].as_ref(),
            Message::System { .. }
        ));
    }

    #[test]
    fn test_sliding_window_truncates() {
        let msgs = vec![
            make_annotated(0, 0, Message::user("q0")),
            make_annotated(1, 1, Message::user("q1")),
            make_annotated(2, 2, Message::user("q2")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = build_context(
            &refs,
            None,
            &ContextStrategy::SlidingWindow { max_turns: 2 },
        );
        // Only turns 1 and 2 should be included (last 2 turns)
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.turns_included, 2);
        assert!(result.truncated);
    }

    #[test]
    fn test_sliding_window_all_fit() {
        let msgs = vec![
            make_annotated(0, 0, Message::user("q0")),
            make_annotated(1, 1, Message::user("q1")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = build_context(
            &refs,
            None,
            &ContextStrategy::SlidingWindow { max_turns: 5 },
        );
        assert_eq!(result.messages.len(), 2);
        assert!(!result.truncated);
    }

    #[test]
    fn test_sliding_window_zero() {
        let msgs = vec![make_annotated(0, 0, Message::user("q0"))];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = build_context(
            &refs,
            Some("prompt"),
            &ContextStrategy::SlidingWindow { max_turns: 0 },
        );
        assert_eq!(result.messages.len(), 1);
        assert!(result.truncated);
    }

    #[test]
    fn test_token_budget_keeps_at_least_one_turn() {
        // Long messages to ensure non-zero token estimates
        let msgs = vec![
            make_annotated(
                0,
                0,
                Message::user("this is the first user query with enough characters to count"),
            ),
            make_annotated(
                1,
                1,
                Message::user("this is the second user query also with content"),
            ),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        // Very small budget — should still keep at least 1 turn
        let result = build_context(
            &refs,
            None,
            &ContextStrategy::TokenBudget {
                max_tokens: 1,
                summarize_overflow: false,
            },
        );
        assert!(result.turns_included >= 1);
        assert!(result.truncated);
    }

    #[test]
    fn test_token_budget_truncates_correctly() {
        let msgs = vec![
            make_annotated(
                0,
                0,
                Message::user(
                    "this is a very long first query that should be truncated away due to token budget constraints",
                ),
            ),
            make_annotated(1, 1, Message::user("short")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        // Budget of ~5 tokens — only "short" (~2 tokens) fits
        let result = build_context(
            &refs,
            None,
            &ContextStrategy::TokenBudget {
                max_tokens: 5,
                summarize_overflow: false,
            },
        );
        assert_eq!(result.turns_included, 1);
        assert!(result.truncated);
        // Should only have the "short" message
        assert_eq!(result.messages.len(), 1);
    }

    #[test]
    fn test_token_budget_with_summarize() {
        let msgs = vec![
            make_annotated(
                0,
                0,
                Message::user("first turn with enough text to require budget consideration"),
            ),
            make_annotated(
                1,
                1,
                Message::user("second turn that should also be counted"),
            ),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = build_context(
            &refs,
            None,
            &ContextStrategy::TokenBudget {
                max_tokens: 5,
                summarize_overflow: true,
            },
        );
        // Should include a summary system message + the last turn
        assert!(result.turns_included >= 1);
        assert!(!result.messages.is_empty());
    }

    #[test]
    fn test_estimate_tokens() {
        let msgs = vec![Message::user("hello world")]; // 11 chars * 0.3 ≈ 3
        let tokens = estimate_tokens(&msgs);
        assert_eq!(tokens, 3);
    }
}
