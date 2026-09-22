//! Responses 语义流式适配器的共享收尾逻辑。
//!
//! 三个 Responses 适配器（deepseek / qwen / openai）的事件解析各写各的，但
//! **流结束时的收尾必须同构** —— 同一份上游行为在不同 provider 下产出不同的
//! 会话历史，是排查不动的 bug。此处集中承载收尾里唯一带判断的那一步：
//! 缓冲区中没有等到 `output_item.done` 的工具调用该补齐还是该丢弃。

use std::collections::HashMap;

use tracing::debug;

use crate::response::{ContentBlock, FinishReason};

/// 正在累积的 Responses 工具调用。
///
/// 仅在 `output_item.added` 到 `output_item.done` 之间存活 —— 收到
/// `output_item.done` 即从缓冲中移除，故流结束时仍留在缓冲里的都是**未闭合**的。
pub(crate) struct PendingResponseToolCall {
    pub(crate) call_id: String,
    pub(crate) name: String,
    pub(crate) arguments: String,
}

/// 流结束时收尾未闭合的工具调用，产出按 `output_index` 升序排列的完整块。
///
/// **截断（[`FinishReason::MaxTokens`]）时一律丢弃。** 此时 arguments 是被截断
/// 的半截 JSON，产成 [`ContentBlock::ToolCall`] 会进会话历史、并被原样回传给
/// 下一轮请求（arguments 是字符串透传，不做 JSON 校验）。丢弃后该块只留一个
/// 悬挂的 `BlockStart`，由 [`crate::BlockAssembler`] 的 `Finish{MaxTokens}`
/// 收敛为 `Incomplete`。与 chat 路径对 `finish_reason == "length"` 的处理同义
/// （见 `streaming::pipeline` 的收尾段）。
///
/// 非截断收尾（上游漏发 `output_item.done`、EOF 无终止事件）仍按安全网补齐：
/// 这条路径上参数通常是完整的，补齐比丢弃更接近上游本意。
pub(crate) fn flush_unclosed_tool_calls(
    request_id: &str,
    tool_calls: HashMap<usize, PendingResponseToolCall>,
    finish_reason: Option<FinishReason>,
) -> Vec<(usize, ContentBlock)> {
    if matches!(finish_reason, Some(FinishReason::MaxTokens)) {
        if !tool_calls.is_empty() {
            let mut dropped: Vec<usize> = tool_calls.keys().copied().collect();
            dropped.sort();
            debug!(
                target: "model_provider::responses",
                request_id = %request_id,
                indices = ?dropped,
                count = dropped.len(),
                "MaxTokens truncation; dropping unclosed tool calls"
            );
        }
        return Vec::new();
    }

    let mut indices: Vec<usize> = tool_calls.keys().copied().collect();
    indices.sort();

    let mut blocks = Vec::with_capacity(indices.len());
    for idx in indices {
        let Some(tc) = tool_calls.get(&idx) else {
            continue;
        };
        // 缺 call_id / name 的条目无法构成合法的 function_call，静默丢弃
        // （正常流程不会走到这里，`output_item.added` 已过滤）。
        if tc.call_id.is_empty() || tc.name.is_empty() {
            continue;
        }
        let mut arguments = tc.arguments.clone();
        if arguments.is_empty() || arguments.trim() == "null" {
            arguments = "{}".to_string();
        }
        blocks.push((
            idx,
            ContentBlock::ToolCall {
                call_id: tc.call_id.clone(),
                name: tc.name.clone(),
                arguments,
            },
        ));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(call_id: &str, name: &str, arguments: &str) -> PendingResponseToolCall {
        PendingResponseToolCall {
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        }
    }

    #[test]
    fn truncation_drops_unclosed_tool_calls() {
        let mut tool_calls = HashMap::new();
        tool_calls.insert(0, pending("c1", "get_weather", "{\"city\":\"S"));

        let blocks = flush_unclosed_tool_calls("req-1", tool_calls, Some(FinishReason::MaxTokens));

        assert!(blocks.is_empty(), "截断的半截 JSON 不得产成 ToolCall 块");
    }

    #[test]
    fn non_truncated_end_flushes_unclosed_tool_calls() {
        let mut tool_calls = HashMap::new();
        tool_calls.insert(1, pending("c1", "get_weather", "{\"city\":\"SF\"}"));

        let blocks = flush_unclosed_tool_calls("req-1", tool_calls, Some(FinishReason::Stop));

        assert_eq!(
            blocks,
            vec![(
                1,
                ContentBlock::ToolCall {
                    call_id: "c1".to_string(),
                    name: "get_weather".to_string(),
                    arguments: "{\"city\":\"SF\"}".to_string(),
                }
            )]
        );
    }

    /// 无终止事件（EOF 即断开）时 `finish_reason` 为 `None`，走补齐分支。
    #[test]
    fn missing_finish_reason_still_flushes() {
        let mut tool_calls = HashMap::new();
        tool_calls.insert(0, pending("c1", "t1", ""));

        let blocks = flush_unclosed_tool_calls("req-1", tool_calls, None);

        assert_eq!(
            blocks,
            vec![(
                0,
                ContentBlock::ToolCall {
                    call_id: "c1".to_string(),
                    name: "t1".to_string(),
                    // 空参数归一化为 `{}`，与 chat 路径一致。
                    arguments: "{}".to_string(),
                }
            )]
        );
    }

    #[test]
    fn blocks_are_ordered_by_output_index() {
        let mut tool_calls = HashMap::new();
        tool_calls.insert(7, pending("c7", "t7", "{}"));
        tool_calls.insert(2, pending("c2", "t2", "{}"));
        tool_calls.insert(5, pending("c5", "t5", "{}"));

        let blocks = flush_unclosed_tool_calls("req-1", tool_calls, Some(FinishReason::Stop));

        let indices: Vec<usize> = blocks.iter().map(|(idx, _)| *idx).collect();
        assert_eq!(indices, vec![2, 5, 7], "HashMap 迭代序不得泄漏到产物顺序");
    }

    #[test]
    fn entries_missing_identity_are_skipped() {
        let mut tool_calls = HashMap::new();
        tool_calls.insert(0, pending("", "t1", "{}"));
        tool_calls.insert(1, pending("c2", "", "{}"));
        tool_calls.insert(2, pending("c3", "t3", "{}"));

        let blocks = flush_unclosed_tool_calls("req-1", tool_calls, Some(FinishReason::Stop));

        let indices: Vec<usize> = blocks.iter().map(|(idx, _)| *idx).collect();
        assert_eq!(indices, vec![2]);
    }
}
