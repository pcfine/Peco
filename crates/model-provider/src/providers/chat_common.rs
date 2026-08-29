//! chat completions 传输层消息的共享构建逻辑。
//!
//! [`WireMessage`] 及其配套类型描述 OpenAI 兼容 chat 协议的请求消息形状，
//! 由各 chat 适配器（如 deepseek）复用；`input_items_to_wire_messages`
//! 负责把中立的 [`InputItem`] 列表合并为该形状。

use std::borrow::Cow;
use std::sync::Arc;

use serde::Serialize;

use crate::response::{InputItem, Role};

/// chat completions 传输层消息（仅用于请求体序列化，crate 内部使用）。
///
/// 承载 chat 协议需要的形状：`system` / `user` / `assistant` / `tool`。
///
/// 所有文本字段借用自 [`GenerateRequest`](crate::response::GenerateRequest)
/// （`'a` 即请求的借用期），序列化前不复制字符串；
/// 唯一例外是 `reasoning_content` — 多个 `Reasoning` 项需要拼接，故用 [`Cow`]：
/// 单项时借用，需拼接时才转为 owned。
#[derive(Debug, Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub(crate) enum WireMessage<'a> {
    System {
        content: &'a str,
    },
    User {
        content: &'a str,
    },
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<WireToolCall<'a>>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<Cow<'a, str>>,
    },
    #[serde(rename = "tool")]
    Tool {
        tool_call_id: &'a str,
        content: &'a str,
    },
}

/// [`ToolCall`](crate::ToolCall) 的借用版本，与其序列化形状逐字段一致（`id` / `type` / `function`）。
#[derive(Debug, Serialize)]
pub(crate) struct WireToolCall<'a> {
    pub(crate) id: &'a str,
    #[serde(rename = "type")]
    pub(crate) call_type: &'static str,
    pub(crate) function: WireToolCallFunction<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WireToolCallFunction<'a> {
    pub(crate) name: &'a str,
    pub(crate) arguments: &'a str,
}

/// 合并累加器中「当前 assistant 消息」的构建状态。
#[derive(Default)]
struct WireAssistantBuilder<'a> {
    content: Option<&'a str>,
    tool_calls: Vec<WireToolCall<'a>>,
    reasoning_content: Option<Cow<'a, str>>,
}

impl<'a> WireAssistantBuilder<'a> {
    fn into_message(self) -> Option<WireMessage<'a>> {
        // 空字符串 content 视同缺失，避免序列化出 `"content": ""`（部分网关拒绝空 content）。
        let content = self.content.filter(|c| !c.is_empty());
        if content.is_none() && self.tool_calls.is_empty() && self.reasoning_content.is_none() {
            return None;
        }
        Some(WireMessage::Assistant {
            content,
            tool_calls: if self.tool_calls.is_empty() {
                None
            } else {
                Some(self.tool_calls)
            },
            reasoning_content: self.reasoning_content,
        })
    }
}

/// 将有序 [`InputItem`] 列表合并为 chat completions 传输层消息列表。
///
/// 用「合并累加器」维护当前 assistant 消息指针：`FunctionCall` / `Reasoning` 追加到
/// 该指针，遇 `Message{role: Assistant}` 时若当前组尚无文本则合并、否则刷新开启新组。
pub(crate) fn input_items_to_wire_messages<'a>(
    items: &'a [Arc<InputItem>],
) -> Vec<WireMessage<'a>> {
    let mut messages: Vec<WireMessage<'a>> = Vec::new();
    let mut current: Option<WireAssistantBuilder<'a>> = None;

    // 刷新当前 assistant 消息（如有）。
    fn flush<'a>(
        messages: &mut Vec<WireMessage<'a>>,
        current: &mut Option<WireAssistantBuilder<'a>>,
    ) {
        if let Some(builder) = current.take()
            && let Some(msg) = builder.into_message()
        {
            messages.push(msg);
        }
    }

    for item in items {
        match &**item {
            InputItem::Message { role, content } => match role {
                // chat 无法承载 Developer，宽松模式下降级为 system。
                Role::System | Role::Developer => {
                    flush(&mut messages, &mut current);
                    messages.push(WireMessage::System { content });
                }
                Role::User => {
                    flush(&mut messages, &mut current);
                    messages.push(WireMessage::User { content });
                }
                Role::Assistant => match current.as_mut() {
                    Some(builder) if builder.content.is_none() => {
                        builder.content = Some(content);
                    }
                    _ => {
                        flush(&mut messages, &mut current);
                        current = Some(WireAssistantBuilder {
                            content: Some(content),
                            ..Default::default()
                        });
                    }
                },
            },
            InputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                current
                    .get_or_insert_with(WireAssistantBuilder::default)
                    .tool_calls
                    .push(WireToolCall {
                        id: call_id,
                        call_type: "function",
                        function: WireToolCallFunction { name, arguments },
                    });
            }
            InputItem::Reasoning { content } => {
                let builder = current.get_or_insert_with(WireAssistantBuilder::default);
                match &mut builder.reasoning_content {
                    // 拼接才付出一次 owned 代价；单条 Reasoning 仍是借用。
                    Some(existing) => existing.to_mut().push_str(content),
                    None => builder.reasoning_content = Some(Cow::Borrowed(content)),
                }
            }
            InputItem::FunctionCallOutput { call_id, output } => {
                flush(&mut messages, &mut current);
                messages.push(WireMessage::Tool {
                    tool_call_id: call_id,
                    content: output,
                });
            }
        }
    }
    flush(&mut messages, &mut current);

    messages
}

/// 确保消息列表以 `user` 结尾。
///
/// 部分网关要求请求的最后一条消息必须是 `user`；若当前末条不是
/// `user`（如 `tool` / `assistant`），追加一条空 content 的 `User`，否则不动。
pub(crate) fn ensure_trailing_user(messages: &mut Vec<WireMessage<'_>>) {
    if matches!(messages.last(), Some(WireMessage::User { .. })) {
        return;
    }
    messages.push(WireMessage::User { content: "" });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_message() -> WireMessage<'static> {
        WireMessage::Tool {
            tool_call_id: "c1",
            content: "72F",
        }
    }

    #[test]
    fn test_ensure_trailing_user_appends_after_tool() {
        let mut messages = vec![WireMessage::User { content: "hi" }, tool_message()];
        ensure_trailing_user(&mut messages);
        assert_eq!(messages.len(), 3);
        assert!(matches!(
            messages.last(),
            Some(WireMessage::User { content: "" })
        ));
    }

    #[test]
    fn test_ensure_trailing_user_appends_after_assistant() {
        let mut messages = vec![WireMessage::Assistant {
            content: Some("done"),
            tool_calls: None,
            reasoning_content: None,
        }];
        ensure_trailing_user(&mut messages);
        assert_eq!(messages.len(), 2);
        assert!(matches!(
            messages.last(),
            Some(WireMessage::User { content: "" })
        ));
    }

    #[test]
    fn test_ensure_trailing_user_keeps_existing_user() {
        let mut messages = vec![
            WireMessage::System { content: "sys" },
            WireMessage::User { content: "hi" },
        ];
        ensure_trailing_user(&mut messages);
        assert_eq!(messages.len(), 2);
    }
}
