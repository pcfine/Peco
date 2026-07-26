// ============================================================================
// PersonalAgentMessageFilter — 个人助理专用消息过滤器
// ============================================================================
//
// 与 PPA 的 PersonalAssistantMessageFilter 独立维护，不复用其代码。
// PPA filter 会独立演进（与 Hook/DynamicContext 耦合），保持两模块解耦。
//
// 过滤策略（比 PPA filter 更激进）：
//   - 当前轮：保留全部消息（含 tool_call / tool_result）
//   - 历史轮：仅保留 User + 纯文本 Assistant（丢弃 tool 过程消息）
//   - 滑动窗口：历史轮最近 N 条（默认 10）
//   - 时间窗口：预留字段，暂不启用

use model_provider::Message;
use peco_core::agent::MessageFilter;
use peco_core::session::AnnotatedMessage;

/// 个人助理专用消息过滤器。
///
/// 在 `build_context` 之前对 `AnnotatedMessage` 引用列表进行过滤。
/// System prompt 和动态上下文由 `build_context` 单独注入，不经过此过滤器。
pub struct PersonalAgentMessageFilter {
    /// 历史轮保留的最大消息条数（默认 10，约 5 轮对话）。
    max_history_messages: usize,
    /// 时间窗口（秒），0 表示不限制。预留字段，暂不启用。
    #[allow(dead_code)]
    time_window_secs: u64,
}

impl PersonalAgentMessageFilter {
    /// 创建过滤器。
    ///
    /// * `max_history_messages` — 历史轮保留消息数上限
    pub fn new(max_history_messages: usize) -> Self {
        Self {
            max_history_messages,
            time_window_secs: 0, // 暂不启用
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

impl MessageFilter for PersonalAgentMessageFilter {
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
                } => {
                    // 仅保留有文本内容且无 tool_calls 的 Assistant（纯回复）
                    content.is_some() && tool_calls.is_none()
                }
                // Tool / System / 其他 → 全部丢弃
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use model_provider::{ToolCall, ToolCallFunction};
    use peco_core::session::{MessageId, MessageSource};

    fn make_annotated(turn: usize, msg: Message) -> AnnotatedMessage {
        AnnotatedMessage::new(MessageId(0), turn, msg, MessageSource::UserInput)
    }

    fn make_user(content: &str) -> Message {
        Message::User {
            content: content.to_string(),
        }
    }

    fn make_assistant_text(content: &str) -> Message {
        Message::Assistant {
            content: Some(content.to_string()),
            tool_calls: None,
            reasoning_content: None,
        }
    }

    fn make_assistant_tool_call(id: &str, name: &str, args: &str) -> Message {
        Message::Assistant {
            content: Some("Let me run a command.".to_string()),
            tool_calls: Some(vec![ToolCall {
                id: id.to_string(),
                call_type: "function".to_string(),
                function: ToolCallFunction {
                    name: name.to_string(),
                    arguments: args.to_string(),
                },
            }]),
            reasoning_content: None,
        }
    }

    fn make_tool(tool_call_id: &str, content: &str) -> Message {
        Message::Tool {
            tool_call_id: tool_call_id.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn test_empty_messages() {
        let filter = PersonalAgentMessageFilter::new(10);
        let refs: Vec<&AnnotatedMessage> = vec![];
        let result = filter.filter(&refs);
        assert!(result.is_empty());
    }

    #[test]
    fn test_single_turn_keeps_all() {
        let filter = PersonalAgentMessageFilter::new(10);
        let msgs = vec![
            make_annotated(0, make_user("问题")),
            make_annotated(0, make_assistant_tool_call("t1", "shell", "ls")),
            make_annotated(0, make_tool("t1", "output")),
            make_annotated(0, make_assistant_text("回答")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // 单轮 → 全部保留
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn test_history_tool_calls_dropped() {
        let filter = PersonalAgentMessageFilter::new(10);
        let msgs = vec![
            // Turn 0: history — tool-call Assistant + Tool → dropped
            make_annotated(0, make_user("历史问题")),
            make_annotated(0, make_assistant_tool_call("t1", "shell", "ls")),
            make_annotated(0, make_tool("t1", "output")),
            make_annotated(0, make_assistant_text("历史回答")),
            // Turn 1: current
            make_annotated(1, make_user("当前问题")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);

        // 历史：User("历史问题") + 纯文本 Assistant("历史回答")
        // tool-call Assistant 和 Tool 被丢弃
        // 当前：User("当前问题")
        assert_eq!(result.len(), 3);
        assert!(matches!(result[0].message.as_ref(), Message::User { .. }));
        assert!(matches!(
            result[1].message.as_ref(),
            Message::Assistant { .. }
        ));
        assert!(matches!(result[2].message.as_ref(), Message::User { .. }));
    }

    #[test]
    fn test_sliding_window() {
        // 只保留最近 4 条历史消息
        let filter = PersonalAgentMessageFilter::new(4);
        let mut msgs: Vec<AnnotatedMessage> = Vec::new();

        // 5 轮历史对话 (turn 0..4)
        for i in 0..5 {
            msgs.push(make_annotated(i, make_user(&format!("问题{i}"))));
            msgs.push(make_annotated(i, make_assistant_text(&format!("回答{i}"))));
        }
        // 当前轮 (turn 5)
        msgs.push(make_annotated(5, make_user("当前问题")));

        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);

        // 历史 10 条 → 滑动窗口保留最近 4 条（问题3,回答3,问题4,回答4）
        // + 当前轮 User(1) = 5
        assert_eq!(result.len(), 5);
        assert!(matches!(result[0].message.as_ref(), Message::User { .. })); // 问题3
        assert!(matches!(
            result[1].message.as_ref(),
            Message::Assistant { .. }
        )); // 回答3
        assert!(matches!(result[2].message.as_ref(), Message::User { .. })); // 问题4
        assert!(matches!(
            result[3].message.as_ref(),
            Message::Assistant { .. }
        )); // 回答4
        assert!(matches!(result[4].message.as_ref(), Message::User { .. }));
        // 当前
    }

    #[test]
    fn test_no_user_messages() {
        let filter = PersonalAgentMessageFilter::new(10);
        let msgs = vec![make_annotated(0, make_assistant_text("No user message"))];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        // 只有 Assistant 消息，全部视为当前轮 → 原样保留
        let result = filter.filter(&refs);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_current_turn_keeps_full_tool_context() {
        // 当前轮的工具调用和结果应完整保留
        let filter = PersonalAgentMessageFilter::new(10);
        let msgs = vec![
            // Turn 0: history
            make_annotated(0, make_user("旧问题")),
            make_annotated(0, make_assistant_text("旧回答")),
            // Turn 1: current — 含完整 ReAct 循环
            make_annotated(1, make_user("新问题")),
            make_annotated(1, make_assistant_tool_call("c1", "shell", "ls")),
            make_annotated(1, make_tool("c1", "output")),
            make_annotated(1, make_assistant_text("新回答")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);

        // 历史: User("旧问题") + Assistant("旧回答") = 2
        // 当前: User + Asst(tool) + Tool + Asst(text) = 4
        assert_eq!(result.len(), 6);

        // 验证当前轮完整性
        let current_msgs: Vec<_> = result.iter().filter(|m| m.turn_index == 1).collect();
        assert_eq!(current_msgs.len(), 4);
        assert!(matches!(
            current_msgs[1].message.as_ref(),
            Message::Assistant { .. }
        )); // tool_call
        assert!(matches!(
            current_msgs[2].message.as_ref(),
            Message::Tool { .. }
        ));
        assert!(matches!(
            current_msgs[3].message.as_ref(),
            Message::Assistant { .. }
        )); // 文本回复
    }

    #[test]
    fn test_assistant_with_no_content_and_tool_calls_dropped_in_history() {
        // Assistant { content: None, tool_calls: Some } 在历史中丢弃
        let filter = PersonalAgentMessageFilter::new(10);
        let msgs = vec![
            make_annotated(0, make_user("历史问题")),
            make_annotated(
                0,
                Message::Assistant {
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "t1".to_string(),
                        call_type: "function".to_string(),
                        function: ToolCallFunction {
                            name: "shell".to_string(),
                            arguments: "ls".to_string(),
                        },
                    }]),
                    reasoning_content: None,
                },
            ),
            make_annotated(0, make_tool("t1", "output")),
            make_annotated(0, make_assistant_text("历史回答")),
            // current
            make_annotated(1, make_user("当前问题")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);

        // 历史：仅 User("历史问题") + 纯文本 Asst("历史回答") = 2
        // 丢弃：Asst(content=None, tool_calls=Some) + Tool
        // 当前：User = 1
        assert_eq!(result.len(), 3);
    }
}
