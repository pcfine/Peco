//! chat completions 传输层消息的共享构建逻辑。
//!
//! [`WireMessage`] 及其配套类型描述 OpenAI 兼容 chat 协议的请求消息形状，
//! 由各 chat 适配器（如 deepseek）复用；`input_items_to_wire_messages`
//! 负责把中立的 [`InputItem`] 列表合并为该形状。
//! `strip_image_parts` / `strip_content_images` 为不支持图片输入的适配器
//! 提供条目级/单内容级的图片剥离。

use std::borrow::Cow;
use std::sync::Arc;

use serde::Serialize;
use tracing::warn;

use crate::ProviderError;
use crate::response::{Content, ContentPart, ImageDetail, InputItem, Role};

/// chat completions 传输层消息（仅用于请求体序列化，crate 内部使用）。
///
/// 承载 chat 协议需要的形状：`system` / `user` / `assistant` / `tool`。
///
/// 文本字段用 [`Cow`] 借用自 [`GenerateRequest`](crate::response::GenerateRequest)
/// （`'a` 即请求的借用期）：消息内容为纯文本时零拷贝借用；
/// 为 `Parts` 混排时经 `text_view()` 拼接才转 owned。序列化形状与裸 `&str` 一致。
#[derive(Debug, Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub(crate) enum WireMessage<'a> {
    System {
        content: Cow<'a, str>,
    },
    User {
        content: WireUserContent<'a>,
    },
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<Cow<'a, str>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<WireToolCall<'a>>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<Cow<'a, str>>,
    },
    #[serde(rename = "tool")]
    Tool {
        tool_call_id: &'a str,
        content: Cow<'a, str>,
    },
}

/// user 消息内容：纯文本（裸字符串）或部件数组（`{"type":"text"}` /
/// `{"type":"image_url",...}`）。序列化形状与 OpenAI chat 协议逐字段一致。
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum WireUserContent<'a> {
    Text(Cow<'a, str>),
    Parts(Vec<WireContentPart<'a>>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum WireContentPart<'a> {
    Text { text: &'a str },
    ImageUrl { image_url: WireImageUrl<'a> },
}

#[derive(Debug, Serialize)]
pub(crate) struct WireImageUrl<'a> {
    pub(crate) url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<ImageDetail>,
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
    content: Option<Cow<'a, str>>,
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
///
/// 用户消息的部件数组忠实映射（text/image 全保留）；`system` / `assistant`
/// 不承载图片，其内容经 `text_view()` 收窄，丢弃的图片按请求聚合计数后 `warn!` 一次。
pub(crate) fn input_items_to_wire_messages<'a>(
    items: &'a [Arc<InputItem>],
) -> Vec<WireMessage<'a>> {
    let mut messages: Vec<WireMessage<'a>> = Vec::new();
    let mut current: Option<WireAssistantBuilder<'a>> = None;
    let mut dropped_role_images: usize = 0;
    let mut dropped_empty_users: usize = 0;

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
                    dropped_role_images += content.image_count();
                    messages.push(WireMessage::System {
                        content: content.text_view(),
                    });
                }
                Role::User => {
                    flush(&mut messages, &mut current);
                    match content {
                        Content::Text(s) => messages.push(WireMessage::User {
                            content: WireUserContent::Text(Cow::Borrowed(s)),
                        }),
                        Content::Parts(parts) => {
                            let wire_parts: Vec<WireContentPart<'_>> = parts
                                .iter()
                                .map(|part| match part {
                                    ContentPart::Text { text } => WireContentPart::Text { text },
                                    ContentPart::Image { url, detail } => {
                                        WireContentPart::ImageUrl {
                                            image_url: WireImageUrl {
                                                url,
                                                detail: *detail,
                                            },
                                        }
                                    }
                                })
                                .collect();
                            if wire_parts.is_empty() {
                                dropped_empty_users += 1;
                            } else {
                                messages.push(WireMessage::User {
                                    content: WireUserContent::Parts(wire_parts),
                                });
                            }
                        }
                    }
                }
                Role::Assistant => match current.as_mut() {
                    Some(builder) if builder.content.is_none() => {
                        dropped_role_images += content.image_count();
                        builder.content = Some(content.text_view());
                    }
                    _ => {
                        flush(&mut messages, &mut current);
                        dropped_role_images += content.image_count();
                        current = Some(WireAssistantBuilder {
                            content: Some(content.text_view()),
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
                    content: output.text_view(),
                });
            }
        }
    }
    flush(&mut messages, &mut current);

    if dropped_role_images > 0 {
        warn!(
            images = dropped_role_images,
            "system/assistant 消息中的图片部件不参与 chat 传输，已丢弃（保留文本）"
        );
    }
    if dropped_empty_users > 0 {
        warn!(count = dropped_empty_users, "部件为空的用户消息已整条丢弃");
    }

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
    messages.push(WireMessage::User {
        content: WireUserContent::Text(Cow::Borrowed("")),
    });
}

// ============================================================================
// 图片剥离（供不支持图片输入的适配器在映射前调用）
// ============================================================================

/// 单 [`Content`] 级图片剥离：图片部件以 `text_view()` 收窄为纯文本，
/// 返回剥离后的内容与丢弃的图片数。纯文本或不含图片的输入零分配直通。
///
/// `strict` 为 true 且含图时返回 [`ProviderError::Request`]。
/// 不做日志 — 计数交给调用方按请求聚合后统一 `warn!`。
pub(crate) fn strip_content_images<'a>(
    content: &'a Content,
    strict: bool,
) -> Result<(Cow<'a, Content>, usize), ProviderError> {
    let image_count = content.image_count();
    if image_count == 0 {
        return Ok((Cow::Borrowed(content), 0));
    }
    if strict {
        return Err(ProviderError::Request(
            "请求包含图片部件，当前 provider 不支持图片输入".to_string(),
        ));
    }
    Ok((
        Cow::Owned(Content::Text(content.text_view().into_owned())),
        image_count,
    ))
}

/// 图片剥离结果：剥离后的条目列表（借用或重建）+ 丢弃的图片数。
pub(crate) type StripItemsResult<'a> = Result<(Cow<'a, [Arc<InputItem>]>, usize), ProviderError>;

/// 条目级图片剥离：遍历 input，消息与工具输出中的图片部件一并剥离，
/// 返回剥离后的条目列表与丢弃的图片总数。不含图片时零分配直通。
///
/// 图片剥离后变空的用户消息整条丢弃；工具输出永不整条丢弃
/// （必须紧跟对应的 function_call），只允许清空为空文本。
/// 不做日志 — 调用方按请求聚合后统一 `warn!`。
pub(crate) fn strip_image_parts<'a>(
    items: &'a [Arc<InputItem>],
    strict: bool,
) -> StripItemsResult<'a> {
    let total_images: usize = items.iter().map(|item| item.image_count()).sum();
    if total_images == 0 {
        return Ok((Cow::Borrowed(items), 0));
    }
    if strict {
        return Err(ProviderError::Request(
            "请求包含图片部件，当前 provider 不支持图片输入".to_string(),
        ));
    }

    let mut stripped: Vec<Arc<InputItem>> = Vec::with_capacity(items.len());
    let mut dropped = 0usize;
    for item in items {
        match &**item {
            InputItem::Message { role, content } => {
                let (content, n) = strip_content_images(content, false)?;
                dropped += n;
                if n == 0 {
                    stripped.push(Arc::clone(item));
                } else if matches!(role, Role::User) && content.text_view().is_empty() {
                    // 剥离后为空的用户消息整条丢弃
                } else {
                    stripped.push(Arc::new(InputItem::Message {
                        role: *role,
                        content: content.into_owned(),
                    }));
                }
            }
            InputItem::FunctionCallOutput { call_id, output } => {
                let (output, n) = strip_content_images(output, false)?;
                dropped += n;
                if n == 0 {
                    stripped.push(Arc::clone(item));
                } else {
                    stripped.push(Arc::new(InputItem::FunctionCallOutput {
                        call_id: call_id.clone(),
                        output: output.into_owned(),
                    }));
                }
            }
            InputItem::FunctionCall { .. } | InputItem::Reasoning { .. } => {
                stripped.push(Arc::clone(item));
            }
        }
    }
    Ok((Cow::Owned(stripped), dropped))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_message() -> WireMessage<'static> {
        WireMessage::Tool {
            tool_call_id: "c1",
            content: "72F".into(),
        }
    }

    fn user_text(text: &str) -> WireMessage<'_> {
        WireMessage::User {
            content: WireUserContent::Text(Cow::Borrowed(text)),
        }
    }

    #[test]
    fn test_ensure_trailing_user_appends_after_tool() {
        let mut messages = vec![user_text("hi"), tool_message()];
        ensure_trailing_user(&mut messages);
        assert_eq!(messages.len(), 3);
        assert!(matches!(
            messages.last(),
            Some(WireMessage::User {
                content: WireUserContent::Text(text)
            }) if text.is_empty()
        ));
    }

    #[test]
    fn test_ensure_trailing_user_appends_after_assistant() {
        let mut messages = vec![WireMessage::Assistant {
            content: Some("done".into()),
            tool_calls: None,
            reasoning_content: None,
        }];
        ensure_trailing_user(&mut messages);
        assert_eq!(messages.len(), 2);
        assert!(matches!(
            messages.last(),
            Some(WireMessage::User {
                content: WireUserContent::Text(text)
            }) if text.is_empty()
        ));
    }

    #[test]
    fn test_ensure_trailing_user_keeps_existing_user() {
        let mut messages = vec![
            WireMessage::System {
                content: "sys".into(),
            },
            user_text("hi"),
        ];
        ensure_trailing_user(&mut messages);
        assert_eq!(messages.len(), 2);
    }

    // ── 用户消息部件映射 ─────────────────────────────────────────────

    fn user_parts_item(parts: Vec<ContentPart>) -> Arc<InputItem> {
        Arc::new(InputItem::Message {
            role: Role::User,
            content: Content::Parts(parts),
        })
    }

    fn image_part(url: &str, detail: Option<ImageDetail>) -> ContentPart {
        ContentPart::Image {
            url: url.to_string(),
            detail,
        }
    }

    #[test]
    fn test_user_parts_map_to_text_and_image_url_parts() {
        let items = vec![user_parts_item(vec![
            ContentPart::Text {
                text: "这是什么图".to_string(),
            },
            image_part("data:image/png;base64,AAAA", None),
            image_part("https://example.com/cat.png", Some(ImageDetail::Low)),
        ])];
        let messages = input_items_to_wire_messages(&items);
        assert_eq!(messages.len(), 1);
        let json = serde_json::to_value(&messages[0]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "这是什么图"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
                    {"type": "image_url", "image_url": {"url": "https://example.com/cat.png", "detail": "low"}},
                ]
            })
        );
    }

    #[test]
    fn test_user_text_content_serializes_as_bare_string() {
        let items = vec![Arc::new(InputItem::Message {
            role: Role::User,
            content: Content::Text("hi".to_string()),
        })];
        let messages = input_items_to_wire_messages(&items);
        let json = serde_json::to_value(&messages[0]).unwrap();
        assert_eq!(json, serde_json::json!({"role": "user", "content": "hi"}));
    }

    #[test]
    fn test_system_and_assistant_parts_drop_images_with_count() {
        let items = vec![
            Arc::new(InputItem::Message {
                role: Role::System,
                content: Content::Parts(vec![
                    ContentPart::Text {
                        text: "系统".to_string(),
                    },
                    image_part("https://example.com/x.png", None),
                ]),
            }),
            Arc::new(InputItem::Message {
                role: Role::Assistant,
                content: Content::Parts(vec![
                    ContentPart::Text {
                        text: "回答".to_string(),
                    },
                    image_part("https://example.com/y.png", None),
                ]),
            }),
        ];
        let messages = input_items_to_wire_messages(&items);
        assert_eq!(messages.len(), 2);
        assert!(matches!(
            &messages[0],
            WireMessage::System { content } if **content == *"系统"
        ));
        // system/assistant 的图片部件被丢弃，文本保留
        assert_eq!(
            serde_json::to_value(&messages[1]).unwrap()["content"],
            "回答"
        );
    }

    #[test]
    fn test_empty_parts_user_message_is_dropped() {
        let items = vec![
            user_parts_item(vec![]),
            Arc::new(InputItem::Message {
                role: Role::User,
                content: Content::Text("hi".to_string()),
            }),
        ];
        let messages = input_items_to_wire_messages(&items);
        assert_eq!(messages.len(), 1);
        assert!(matches!(&messages[0], WireMessage::User { .. }));
    }

    // ── 图片剥离 ─────────────────────────────────────────────────────

    #[test]
    fn test_strip_content_images_three_states() {
        let text = Content::Text("纯文本".to_string());
        // 无图：零分配直通
        let (stripped, dropped) = strip_content_images(&text, false).unwrap();
        assert!(matches!(stripped, Cow::Borrowed(_)));
        assert_eq!(dropped, 0);

        let parts = Content::Parts(vec![
            ContentPart::Text {
                text: "看图".to_string(),
            },
            image_part("data:image/png;base64,AAAA", None),
        ]);
        // 宽松：剥离为纯文本
        let (stripped, dropped) = strip_content_images(&parts, false).unwrap();
        assert_eq!(stripped.text_view(), "看图");
        assert_eq!(dropped, 1);

        // 严格：报错
        assert!(strip_content_images(&parts, true).is_err());
    }

    #[test]
    fn test_strip_image_parts_lenient_drops_empty_user_keeps_tool_output() {
        let items = vec![
            Arc::new(InputItem::Message {
                role: Role::User,
                content: Content::Parts(vec![
                    image_part("https://example.com/a.png", None),
                    image_part("https://example.com/b.png", None),
                ]),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "t".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: Content::Parts(vec![
                    ContentPart::Text {
                        text: "结果".to_string(),
                    },
                    image_part("https://example.com/c.png", None),
                ]),
            }),
        ];
        let (stripped, dropped) = strip_image_parts(&items, false).unwrap();
        // 3 张图：2 张来自被整条丢弃的纯图用户消息，1 张来自工具输出
        assert_eq!(dropped, 3);
        assert_eq!(stripped.len(), 2);
        // 纯图用户消息整条丢弃
        assert!(matches!(
            &*stripped[0],
            InputItem::FunctionCall { call_id, .. } if call_id == "c1"
        ));
        // 工具输出保留条目、清空图片
        assert!(matches!(
            &*stripped[1],
            InputItem::FunctionCallOutput { output, .. }
                if output.text_view() == "结果" && output.image_count() == 0
        ));
    }

    #[test]
    fn test_strip_image_parts_no_images_is_borrowed() {
        let items = vec![Arc::new(InputItem::Message {
            role: Role::User,
            content: Content::Text("hi".to_string()),
        })];
        let (stripped, dropped) = strip_image_parts(&items, true).unwrap();
        assert!(matches!(stripped, Cow::Borrowed(_)));
        assert_eq!(dropped, 0);
    }

    #[test]
    fn test_strip_image_parts_strict_errors() {
        let items = vec![user_parts_item(vec![image_part(
            "https://e.com/x.png",
            None,
        )])];
        assert!(strip_image_parts(&items, true).is_err());
    }
}
