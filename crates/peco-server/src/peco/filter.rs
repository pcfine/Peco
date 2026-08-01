// ============================================================================
// PecoMessageFilter — Peco 永续对话专用消息过滤器
// ============================================================================
//
// 过滤策略：
//   - 当前轮：保留全部消息（含 tool_call / tool_result）
//   - 历史轮：仅保留 User + 纯文本 Assistant（丢弃 tool 过程消息）
//   - 滑动窗口：历史轮最近 N 条（默认 10）

use model_provider::Message;
use peco_core::agent::MessageFilter;
use peco_core::session::AnnotatedMessage;

/// Peco 永续对话专用消息过滤器。
///
/// 在 `build_context` 之前对 `AnnotatedMessage` 引用列表进行过滤。
/// System prompt 和动态上下文由 `build_context` 单独注入，不经过此过滤器。
pub struct PecoMessageFilter {
    /// 历史轮保留的最大消息条数（默认 10，约 5 轮对话）。
    max_history_messages: usize,
    /// 时间窗口（秒），0 表示不限制。预留字段，暂不启用。
    #[allow(dead_code)]
    time_window_secs: u64,
}

impl PecoMessageFilter {
    /// 创建过滤器。
    ///
    /// * `max_history_messages` — 历史轮保留消息数上限
    pub fn new(max_history_messages: usize) -> Self {
        Self {
            max_history_messages,
            time_window_secs: 0,
        }
    }

    /// 创建带时间窗口的过滤器（接口预留，暂不启用过滤逻辑）。
    #[allow(dead_code)]
    pub fn with_time_window(max_history_messages: usize, time_window_secs: u64) -> Self {
        Self {
            max_history_messages,
            time_window_secs,
        }
    }
}

impl MessageFilter for PecoMessageFilter {
    fn filter(&self, messages: &[&AnnotatedMessage]) -> Vec<AnnotatedMessage> {
        if messages.is_empty() {
            return vec![];
        }

        // ── 1. 定位当前轮：最后一条消息的 turn_index ──────────────
        let current_turn = messages.last().unwrap().turn_index;

        // ── 2. 切分：当前轮 vs 历史轮 ────────────────────────────
        let (history, current): (Vec<_>, Vec<_>) = messages
            .iter()
            .map(|m| (*m).clone())
            .partition(|m| m.turn_index < current_turn);

        // ── 3. 过滤历史轮 ────────────────────────────────────────
        // 保留 User 消息和纯文本 Assistant (content 有值, tool_calls 无值)
        let filtered_history: Vec<_> = history
            .into_iter()
            .filter(|m| match m.message.as_ref() {
                Message::User { .. } => true,
                Message::Assistant {
                    content,
                    tool_calls,
                    ..
                } => content.is_some() && tool_calls.is_none(),
                _ => false,
            })
            .collect();

        // ── 4. 滑动窗口：历史轮只保留最近 N 条 ───────────────────
        let recent_history = if filtered_history.len() > self.max_history_messages {
            let start = filtered_history.len() - self.max_history_messages;
            filtered_history[start..].to_vec()
        } else {
            filtered_history
        };

        // ── 5. 组装：截断后的历史 + 完整当前轮 ──────────────────
        let mut result = Vec::with_capacity(recent_history.len() + current.len());
        result.extend(recent_history);
        result.extend(current);

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_provider::{ToolCall, ToolCallFunction};
    use peco_core::session::{MessageId, MessageSource};

    fn make_annotated(turn: usize, msg: Message) -> AnnotatedMessage {
        AnnotatedMessage::new(MessageId(0), turn, msg, MessageSource::UserInput)
    }

    #[test]
    fn test_empty_messages() {
        let filter = PecoMessageFilter::new(10);
        let refs: Vec<&AnnotatedMessage> = vec![];
        let result = filter.filter(&refs);
        assert!(result.is_empty());
    }

    #[test]
    fn test_current_turn_keeps_full_tool_context() {
        let filter = PecoMessageFilter::new(10);
        let msgs = vec![
            make_annotated(
                0,
                Message::User {
                    content: "旧问题".into(),
                },
            ),
            make_annotated(
                0,
                Message::Assistant {
                    content: Some("旧回答".into()),
                    tool_calls: None,
                    reasoning_content: None,
                },
            ),
            make_annotated(
                1,
                Message::User {
                    content: "新问题".into(),
                },
            ),
            make_annotated(
                1,
                Message::Assistant {
                    content: Some("run".into()),
                    tool_calls: Some(vec![ToolCall {
                        id: "c1".into(),
                        call_type: "function".into(),
                        function: ToolCallFunction {
                            name: "shell".into(),
                            arguments: "ls".into(),
                        },
                    }]),
                    reasoning_content: None,
                },
            ),
            make_annotated(
                1,
                Message::Tool {
                    tool_call_id: "c1".into(),
                    content: "out".into(),
                },
            ),
            make_annotated(
                1,
                Message::Assistant {
                    content: Some("done".into()),
                    tool_calls: None,
                    reasoning_content: None,
                },
            ),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // 历史: User + Asst(text) = 2, 当前: 4
        assert_eq!(result.len(), 6);
    }
}
