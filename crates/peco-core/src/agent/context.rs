// ============================================================================
// Agent 层上下文构建器
// ============================================================================
//
// 从 Session 的消息引用列表构建 LLM 请求的消息上下文。
// 不同 Agent 可通过配置不同的 ContextStrategy 控制上下文行为。
//
// Session 的 system prompt 在此外层注入，不存入 Session。

use std::sync::Arc;

use model_provider::{InputItem, Role};

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
    /// Token 预算：保留不超过 max_context_tokens 的消息
    TokenBudget {
        max_context_tokens: usize,
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
                max_context_tokens,
                summarize_overflow,
            } => f
                .debug_struct("TokenBudget")
                .field("max_context_tokens", max_context_tokens)
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
    /// 最终发送给 LLM 的历史输入项（Arc 共享所有权，零拷贝上下文构建）。
    ///
    /// 不含 system prompt — 由 `GenerateRequest.instructions` 单独承载。
    pub messages: Vec<Arc<InputItem>>,
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

/// 从消息引用和策略构建 LLM 上下文（历史 `InputItem`，不含 system prompt）。
///
/// `system_prompt` 由 Agent 提供（不存储在 Session 中），仅用于 token 预算估算；
/// 最终由 [`GenerateRequest::instructions`](model_provider::GenerateRequest) 承载，
/// 不注入 `input` 历史。
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
        ContextStrategy::SlidingWindow { max_turns } => build_sliding_window(messages, *max_turns),
        ContextStrategy::TokenBudget {
            max_context_tokens,
            summarize_overflow,
        } => build_token_budget(
            messages,
            system_prompt,
            *max_context_tokens,
            *summarize_overflow,
        ),
        ContextStrategy::FullHistory => build_full_history(messages),
        ContextStrategy::Custom(filter) => filter.apply(messages, system_prompt),
    }
}

// ============================================================================
// 内部辅助
// ============================================================================

/// 构建全量历史（默认策略）。
fn build_full_history(messages: &[&AnnotatedMessage]) -> ContextResult {
    let mut result: Vec<Arc<InputItem>> = Vec::with_capacity(messages.len());

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
fn build_sliding_window(messages: &[&AnnotatedMessage], max_turns: usize) -> ContextResult {
    if max_turns == 0 {
        // 0 窗口：无任何消息
        return ContextResult {
            messages: Vec::new(),
            turns_included: 0,
            estimated_tokens: 0,
            truncated: !messages.is_empty(),
        };
    }

    // 找到最大的 turn_index
    let max_turn = messages.iter().map(|am| am.turn_index).max().unwrap_or(0);
    // 计算起始 turn（不能下溢）
    let start_turn = max_turn.saturating_sub(max_turns.saturating_sub(1));

    let truncated = messages.iter().any(|am| am.turn_index < start_turn);

    let mut result: Vec<Arc<InputItem>> = Vec::new();

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
/// 从最新 turn 向前累积消息，直到超出 `max_context_tokens` 预算。
/// 始终保留 system prompt（若提供）和 at least 1 个完整 turn。
/// 当 `summarize_overflow` 为 true 时，被截断的早期 turn 用占位摘要替代。
fn build_token_budget(
    messages: &[&AnnotatedMessage],
    system_prompt: Option<&str>,
    max_context_tokens: usize,
    summarize_overflow: bool,
) -> ContextResult {
    if messages.is_empty() {
        return ContextResult {
            estimated_tokens: 0,
            messages: Vec::new(),
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
    let mut budget_remaining = max_context_tokens.saturating_sub(system_tokens);

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
            .map(|am| (input_item_chars(am.message.as_ref()) as f64 * 0.3) as usize)
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
    let mut result: Vec<Arc<InputItem>> = Vec::new();

    // 若被截断且 summarize_overflow 启用，插入摘要占位（System 消息项）。
    if truncated && summarize_overflow {
        let omitted_turns = min_included_turn;
        let summary = format!(
            "[Earlier conversation omitted: {} turn(s) truncated due to token budget]",
            omitted_turns
        );
        result.push(Arc::new(InputItem::Message {
            role: Role::System,
            content: summary,
        }));
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

/// 单条 [`InputItem`] 的主要文本内容（校准 token 估算依据）。
///
/// 与 `input_item_chars` 覆盖相同的字段（Message/Reasoning 的 content、
/// FunctionCall 的 arguments、FunctionCallOutput 的 output）。
fn input_item_text(item: &InputItem) -> &str {
    match item {
        InputItem::Message { content, .. } => content,
        InputItem::Reasoning { content } => content,
        InputItem::FunctionCall { arguments, .. } => arguments,
        InputItem::FunctionCallOutput { output, .. } => output,
        _ => "",
    }
}

/// 单条 [`InputItem`] 的字符数（粗略 token 估算依据）。
fn input_item_chars(item: &InputItem) -> usize {
    match item {
        InputItem::Message { content, .. } => content.len(),
        InputItem::Reasoning { content } => content.len(),
        InputItem::FunctionCall { arguments, .. } => arguments.len(),
        InputItem::FunctionCallOutput { output, .. } => output.len(),
        _ => 0,
    }
}

/// 粗略 token 估算（Arc 版本）— 委托给校准估算器，保证全项目单一口径。
///
/// 历史上此函数按字节 × 0.3 估算（CJK 每字 3 字节 → ≈0.9 token/字，系统性高估）；
/// 现与 compaction / 历史预算共用同一校准实现，避免双口径漂移。
fn estimate_tokens_arc(messages: &[Arc<InputItem>]) -> usize {
    messages
        .iter()
        .map(|m| estimate_item_tokens(m.as_ref()))
        .sum()
}

// ============================================================================
// 校准 token 估算（compaction / 历史预算共用）
// ============================================================================

/// 校准 token 估算：CJK 字符 ≈ 0.6 token/字符，其它 ≈ 0.3 token/字符。
///
/// 旧的 `字节 × 0.3` 估算对中文系统性偏差（UTF-8 每汉字 3 字节 × 0.3 ≈ 0.9
/// token/字，高估 1.5×；纯 ASCII 场景则低估）。此估算器按字符类别分别计权，
/// 供上下文压缩、历史窗口预算与 ContextUsage 等所有 token 估算路径使用 —
/// 全项目单一实现，防漂移。
pub fn estimate_item_tokens(item: &InputItem) -> usize {
    estimate_str_tokens(input_item_text(item))
}

/// 按「字符数」估算 token（同 [`estimate_item_tokens`] 的权重规则）。
pub fn estimate_str_tokens(s: &str) -> usize {
    let mut cjk = 0usize;
    let mut other = 0usize;
    for c in s.chars() {
        if is_cjk(c) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    estimate_chars_tokens(other) + estimate_cjk_tokens(cjk)
}

fn estimate_chars_tokens(chars: usize) -> usize {
    (chars as f64 * 0.3).ceil() as usize
}

fn estimate_cjk_tokens(cjk_chars: usize) -> usize {
    (cjk_chars as f64 * 0.6).ceil() as usize
}

/// 是否为 CJK 字符（汉字 / 日文假名 / 谚文 / 全角标点）。
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x9FFF   // CJK 部首、汉字、假名、谚文兼容
        | 0xAC00..=0xD7AF // 谚文音节
        | 0xF900..=0xFAFF // CJK 兼容表意文字
        | 0xFF00..=0xFFEF // 全角形式
        | 0x20000..=0x2FA1F // CJK 扩展 B-F
    )
}

/// 粗略 token 估算：每字符约 0.3 token（`&InputItem` 版本，用于测试）。
#[allow(dead_code)]
fn estimate_tokens(messages: &[InputItem]) -> usize {
    let char_count: usize = messages.iter().map(input_item_chars).sum();
    (char_count as f64 * 0.3) as usize
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{AnnotatedMessage, MessageId, MessageSource};
    use model_provider::{InputItem, Role};

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

    fn make_annotated(id: u64, turn: usize, msg: InputItem) -> AnnotatedMessage {
        AnnotatedMessage::new(MessageId(id), turn, msg, MessageSource::UserInput)
    }

    #[test]
    fn test_full_history_no_prompt() {
        let msgs = [
            make_annotated(0, 0, user("hello")),
            make_annotated(1, 0, assistant("hi")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = build_context(&refs, None, &ContextStrategy::FullHistory);
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.turns_included, 1);
        assert!(!result.truncated);
    }

    #[test]
    fn test_full_history_with_prompt() {
        let msgs = [make_annotated(0, 0, user("hello"))];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = build_context(
            &refs,
            Some("You are helpful."),
            &ContextStrategy::FullHistory,
        );
        // system prompt is NOT injected into the history items anymore
        assert_eq!(result.messages.len(), 1);
        assert!(matches!(
            result.messages[0].as_ref(),
            InputItem::Message {
                role: Role::User,
                ..
            }
        ));
    }

    #[test]
    fn test_sliding_window_truncates() {
        let msgs = [
            make_annotated(0, 0, user("q0")),
            make_annotated(1, 1, user("q1")),
            make_annotated(2, 2, user("q2")),
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
        let msgs = [
            make_annotated(0, 0, user("q0")),
            make_annotated(1, 1, user("q1")),
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
        let msgs = [make_annotated(0, 0, user("q0"))];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = build_context(
            &refs,
            Some("prompt"),
            &ContextStrategy::SlidingWindow { max_turns: 0 },
        );
        assert_eq!(result.messages.len(), 0);
        assert!(result.truncated);
    }

    #[test]
    fn test_token_budget_keeps_at_least_one_turn() {
        // Long messages to ensure non-zero token estimates
        let msgs = [
            make_annotated(
                0,
                0,
                user("this is the first user query with enough characters to count"),
            ),
            make_annotated(
                1,
                1,
                user("this is the second user query also with content"),
            ),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        // Very small budget — should still keep at least 1 turn
        let result = build_context(
            &refs,
            None,
            &ContextStrategy::TokenBudget {
                max_context_tokens: 1,
                summarize_overflow: false,
            },
        );
        assert!(result.turns_included >= 1);
        assert!(result.truncated);
    }

    #[test]
    fn test_token_budget_truncates_correctly() {
        let msgs = [
            make_annotated(
                0,
                0,
                user(
                    "this is a very long first query that should be truncated away due to token budget constraints",
                ),
            ),
            make_annotated(1, 1, user("short")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        // Budget of ~5 tokens — only "short" (~2 tokens) fits
        let result = build_context(
            &refs,
            None,
            &ContextStrategy::TokenBudget {
                max_context_tokens: 5,
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
        let msgs = [
            make_annotated(
                0,
                0,
                user("first turn with enough text to require budget consideration"),
            ),
            make_annotated(1, 1, user("second turn that should also be counted")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = build_context(
            &refs,
            None,
            &ContextStrategy::TokenBudget {
                max_context_tokens: 5,
                summarize_overflow: true,
            },
        );
        // Should include a summary system message + the last turn
        assert!(result.turns_included >= 1);
        assert!(!result.messages.is_empty());
    }

    #[test]
    fn test_estimate_tokens() {
        let msgs = vec![user("hello world")]; // 11 chars * 0.3 ≈ 3
        let tokens = estimate_tokens(&msgs);
        assert_eq!(tokens, 3);
    }
}
