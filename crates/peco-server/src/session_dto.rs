// ============================================================================
// Session 快照 DTO 分组辅助 — InputItem → 中立分组消息
// ============================================================================
//
// 迁移后 Session 以 `InputItem` 细粒度存储，而前端快照仍消费旧的 `Message`
// 形状（assistant 消息合并 reasoning + tool_calls）。本模块提供中立分组，
// 并额外保留每条消息的首个 item 时间戳，供三处快照 handler 复用。

use model_provider::{InputItem, Role, ToolCall};

/// 合并后的一条消息（中立形状，不依赖已删除的 `Message`）。
#[derive(Debug)]
pub struct GroupedMessage {
    /// 角色名称："system" / "user" / "assistant" / "tool"。
    pub role: &'static str,
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub reasoning_content: Option<String>,
    pub tool_call_id: Option<String>,
    /// 该组首个 item 的时间戳。
    pub timestamp_ms: u64,
}

/// 合并中的「当前 assistant 消息」构建状态（含时间戳）。
#[derive(Default)]
struct AssistantBuilder {
    content: Option<String>,
    tool_calls: Vec<ToolCall>,
    reasoning_content: Option<String>,
    timestamp_ms: u64,
}

impl AssistantBuilder {
    fn into_message(self) -> Option<GroupedMessage> {
        if self.content.is_none() && self.tool_calls.is_empty() && self.reasoning_content.is_none()
        {
            return None;
        }
        Some(GroupedMessage {
            role: "assistant",
            content: self.content,
            tool_calls: self.tool_calls,
            reasoning_content: self.reasoning_content,
            tool_call_id: None,
            timestamp_ms: self.timestamp_ms,
        })
    }
}

/// 将一 turn 的有序 `(InputItem, timestamp_ms)` 列表合并回 [`GroupedMessage`] 列表。
///
/// 合并规则：`FunctionCall` / `Reasoning` 追加到当前 assistant 消息，遇
/// `Message{role: Assistant}` 时若当前组尚无文本则合并、否则刷新开启新组。
/// 每条消息的时间戳取该组首个 item 的时间戳。
pub fn group_input_items(items: &[InputItem], timestamps: &[u64]) -> Vec<GroupedMessage> {
    debug_assert_eq!(items.len(), timestamps.len());

    let mut messages: Vec<GroupedMessage> = Vec::new();
    let mut current: Option<AssistantBuilder> = None;

    // 刷新当前 assistant 消息（如有）。
    fn flush(messages: &mut Vec<GroupedMessage>, current: &mut Option<AssistantBuilder>) {
        if let Some(builder) = current.take()
            && let Some(msg) = builder.into_message()
        {
            messages.push(msg);
        }
    }

    for (item, &ts) in items.iter().zip(timestamps.iter()) {
        match item {
            InputItem::Message { role, content } => match role {
                Role::System | Role::Developer => {
                    flush(&mut messages, &mut current);
                    messages.push(GroupedMessage {
                        role: "system",
                        content: Some(content.clone()),
                        tool_calls: Vec::new(),
                        reasoning_content: None,
                        tool_call_id: None,
                        timestamp_ms: ts,
                    });
                }
                Role::User => {
                    flush(&mut messages, &mut current);
                    messages.push(GroupedMessage {
                        role: "user",
                        content: Some(content.clone()),
                        tool_calls: Vec::new(),
                        reasoning_content: None,
                        tool_call_id: None,
                        timestamp_ms: ts,
                    });
                }
                Role::Assistant => {
                    // 当前 assistant 组尚无文本时合并文本（「Reasoning → FunctionCall →
                    // Message{Assistant}」合成单条）；否则刷新后开启新组。时间戳保持
                    // 该组首个 item 的时间戳。
                    match current.as_mut() {
                        Some(builder) if builder.content.is_none() => {
                            builder.content = Some(content.clone());
                        }
                        _ => {
                            flush(&mut messages, &mut current);
                            current = Some(AssistantBuilder {
                                content: Some(content.clone()),
                                timestamp_ms: ts,
                                ..Default::default()
                            });
                        }
                    }
                }
            },
            InputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                current
                    .get_or_insert_with(|| AssistantBuilder {
                        timestamp_ms: ts,
                        ..Default::default()
                    })
                    .tool_calls
                    .push(ToolCall::new(
                        call_id.clone(),
                        name.clone(),
                        arguments.clone(),
                    ));
            }
            InputItem::Reasoning { content } => {
                let builder = current.get_or_insert_with(|| AssistantBuilder {
                    timestamp_ms: ts,
                    ..Default::default()
                });
                match &mut builder.reasoning_content {
                    Some(existing) => existing.push_str(content),
                    None => builder.reasoning_content = Some(content.clone()),
                }
            }
            InputItem::FunctionCallOutput { call_id, output } => {
                flush(&mut messages, &mut current);
                messages.push(GroupedMessage {
                    role: "tool",
                    content: Some(output.clone()),
                    tool_calls: Vec::new(),
                    reasoning_content: None,
                    tool_call_id: Some(call_id.clone()),
                    timestamp_ms: ts,
                });
            }
            _ => {}
        }
    }
    flush(&mut messages, &mut current);

    messages
}
