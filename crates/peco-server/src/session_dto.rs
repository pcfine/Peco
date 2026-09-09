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
    /// 用户消息携带的图片部件 URL（含 data URI），其余角色恒为空。
    pub images: Vec<String>,
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
            images: Vec::new(),
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
                        content: Some(content.text_view().into_owned()),
                        images: Vec::new(),
                        tool_calls: Vec::new(),
                        reasoning_content: None,
                        tool_call_id: None,
                        timestamp_ms: ts,
                    });
                }
                Role::User => {
                    flush(&mut messages, &mut current);
                    // 图片部件随 text 视图一并透出，供前端刷新后恢复渲染。
                    let images = content
                        .image_urls()
                        .into_iter()
                        .map(str::to_string)
                        .collect();
                    messages.push(GroupedMessage {
                        role: "user",
                        content: Some(content.text_view().into_owned()),
                        images,
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
                            builder.content = Some(content.text_view().into_owned());
                        }
                        _ => {
                            flush(&mut messages, &mut current);
                            current = Some(AssistantBuilder {
                                content: Some(content.text_view().into_owned()),
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
                    content: Some(output.text_view().into_owned()),
                    images: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use model_provider::{Content, ContentPart};

    fn user_parts_message() -> InputItem {
        InputItem::Message {
            role: Role::User,
            content: Content::Parts(vec![
                ContentPart::Text {
                    text: "看看这张图".to_string(),
                },
                ContentPart::Image {
                    url: "data:image/png;base64,QUJD".to_string(),
                    detail: None,
                },
            ]),
        }
    }

    #[test]
    fn user_message_with_images_exposes_image_urls() {
        let items = vec![user_parts_message()];
        let grouped = group_input_items(&items, &[1]);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].role, "user");
        assert_eq!(grouped[0].images, vec!["data:image/png;base64,QUJD"]);
        // text 视图保持文本部件拼接。
        assert_eq!(grouped[0].content.as_deref(), Some("看看这张图"));
    }

    #[test]
    fn text_only_user_message_has_empty_images() {
        let items = vec![InputItem::Message {
            role: Role::User,
            content: Content::Text("纯文本".to_string()),
        }];
        let grouped = group_input_items(&items, &[1]);
        assert_eq!(grouped[0].role, "user");
        assert!(grouped[0].images.is_empty());
    }

    #[test]
    fn non_user_roles_never_carry_images() {
        // assistant 图不计入：图片仅对 user 角色有输入语义。
        let items = vec![
            InputItem::Message {
                role: Role::User,
                content: Content::Text("问".to_string()),
            },
            InputItem::Message {
                role: Role::Assistant,
                content: Content::Parts(vec![
                    ContentPart::Text {
                        text: "答".to_string(),
                    },
                    ContentPart::Image {
                        url: "data:image/png;base64,QUJD".to_string(),
                        detail: None,
                    },
                ]),
            },
            InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: user_parts_message_content(),
            },
        ];
        let grouped = group_input_items(&items, &[1, 2, 3]);
        assert!(
            grouped
                .iter()
                .all(|m| m.role == "user" || m.images.is_empty())
        );
    }

    fn user_parts_message_content() -> Content {
        Content::Parts(vec![
            ContentPart::Text {
                text: "工具输出文本".to_string(),
            },
            ContentPart::Image {
                url: "data:image/png;base64,QUJD".to_string(),
                detail: None,
            },
        ])
    }
}
