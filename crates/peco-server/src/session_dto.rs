// ============================================================================
// Session 快照 DTO 分组辅助 — InputItem → 中立分组消息
// ============================================================================
//
// 迁移后 Session 以 `InputItem` 细粒度存储，而前端快照仍消费旧的 `Message`
// 形状（assistant 消息合并 reasoning + tool_calls）。本模块提供中立分组，
// 并额外保留每条消息的来源与时间戳，供三处快照 handler 复用。

use model_provider::{InputItem, Role, ToolCall};
use peco_core::session::{AnnotatedMessage, MessageSource, strip_merge_markers};

/// 分组输入：内容 + 来源 + 时间戳，三者同源于一条 [`AnnotatedMessage`]。
///
/// 收拢成一条切片而非平行的 `items` / `timestamps` 两条 —— 分组既需要
/// 时间戳（回落 UI 时间轴），也需要来源（识别中断轮），平行切片让长度
/// 失配在类型上依然可能。
#[derive(Debug, Clone, Copy)]
pub struct ItemView<'a> {
    pub item: &'a InputItem,
    pub source: &'a MessageSource,
    pub timestamp_ms: u64,
}

impl<'a> ItemView<'a> {
    /// 把一个 turn 的消息投影为分组视图（零拷贝）。
    pub fn from_turn(msgs: &'a [AnnotatedMessage]) -> Vec<ItemView<'a>> {
        msgs.iter()
            .map(|am| ItemView {
                item: am.message.as_ref(),
                source: &am.source,
                timestamp_ms: am.timestamp_ms,
            })
            .collect()
    }
}

/// 取一条消息来源的中断标记（非中断来源为 `None`）。
fn interrupted_of(source: &MessageSource) -> Option<String> {
    match source {
        MessageSource::InterruptedTurn { reason } => Some(reason.clone()),
        _ => None,
    }
}

/// 提取一轮的中断原因：本轮首个带 `InterruptedTurn` 来源的标记。
///
/// 冻结时合成项与中断说明**都**带真实原因（见 `Session::interrupt_turn`），
/// 故任取其一即可，不需要分辨「取哪一条」。
pub fn turn_interrupted_reason(msgs: &[AnnotatedMessage]) -> Option<String> {
    msgs.iter().find_map(|am| interrupted_of(&am.source))
}

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
    /// 该组含中断轮产物时携带的中断原因（人类可读）。
    pub interrupted_reason: Option<String>,
}

/// 合并中的「当前 assistant 消息」构建状态（含时间戳）。
#[derive(Default)]
struct AssistantBuilder {
    content: Option<String>,
    tool_calls: Vec<ToolCall>,
    reasoning_content: Option<String>,
    timestamp_ms: u64,
    interrupted_reason: Option<String>,
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
            interrupted_reason: self.interrupted_reason,
        })
    }
}

/// 将一 turn 的有序 [`ItemView`] 列表合并回 [`GroupedMessage`] 列表。
///
/// 合并规则：`FunctionCall` / `Reasoning` 追加到当前 assistant 消息，遇
/// `Message{role: Assistant}` 时若当前组尚无文本则合并、否则刷新开启新组。
/// 每条消息的时间戳取该组首个 item 的时间戳。
pub fn group_input_items(items: &[ItemView<'_>]) -> Vec<GroupedMessage> {
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

    for view in items {
        let item = view.item;
        let ts = view.timestamp_ms;
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
                        interrupted_reason: None,
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
                    // MergedPending（pending 批量合并）的 user 消息带 ---
                    // 分隔标记，模型侧原样保留，展示前按来源剥离 —— 用户
                    // 自己输入的 --- 行不在此列（source != MergedPending）。
                    let text = if matches!(view.source, MessageSource::MergedPending) {
                        strip_merge_markers(content)
                    } else {
                        content.text_view().into_owned()
                    };
                    messages.push(GroupedMessage {
                        role: "user",
                        content: Some(text),
                        images,
                        tool_calls: Vec::new(),
                        reasoning_content: None,
                        tool_call_id: None,
                        timestamp_ms: ts,
                        interrupted_reason: None,
                    });
                }
                Role::Assistant => {
                    // 当前 assistant 组尚无文本时合并文本（「Reasoning → FunctionCall →
                    // Message{Assistant}」合成单条）；否则刷新后开启新组。时间戳保持
                    // 该组首个 item 的时间戳。
                    let interrupted = interrupted_of(view.source);
                    match current.as_mut() {
                        Some(builder) if builder.content.is_none() => {
                            builder.content = Some(content.text_view().into_owned());
                            // 中断说明（`Role::Assistant` + `InterruptedTurn`）会落进
                            // 已有组（前一条是 Reasoning 时），标记必须一并带上。
                            if interrupted.is_some() {
                                builder.interrupted_reason = interrupted;
                            }
                        }
                        _ => {
                            flush(&mut messages, &mut current);
                            current = Some(AssistantBuilder {
                                content: Some(content.text_view().into_owned()),
                                timestamp_ms: ts,
                                interrupted_reason: interrupted,
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
                    // 冻结时合成的工具输出也带 `InterruptedTurn`；标记挂上，让
                    // 该工具卡片同样可识别为中断产物。
                    interrupted_reason: interrupted_of(view.source),
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
    use peco_core::session::MessageId;

    /// 快捷分组：造普通来源的消息后走真实投影路径。
    ///
    /// 来源用 `UserInput` 占位 —— 分组只看「是否 `InterruptedTurn`」，
    /// 其余变体一律等价。时间戳由 `AnnotatedMessage::new` 内部取当前时刻，
    /// 分组断言不涉及它。
    fn group(items: Vec<InputItem>) -> Vec<GroupedMessage> {
        let msgs: Vec<AnnotatedMessage> = items
            .into_iter()
            .map(|item| AnnotatedMessage::new(MessageId(0), 0, item, MessageSource::UserInput))
            .collect();
        group_input_items(&ItemView::from_turn(&msgs))
    }

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
        let grouped = group(vec![user_parts_message()]);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].role, "user");
        assert_eq!(grouped[0].images, vec!["data:image/png;base64,QUJD"]);
        // text 视图保持文本部件拼接。
        assert_eq!(grouped[0].content.as_deref(), Some("看看这张图"));
    }

    #[test]
    fn text_only_user_message_has_empty_images() {
        let grouped = group(vec![InputItem::Message {
            role: Role::User,
            content: Content::Text("纯文本".to_string()),
        }]);
        assert_eq!(grouped[0].role, "user");
        assert!(grouped[0].images.is_empty());
    }

    #[test]
    fn non_user_roles_never_carry_images() {
        // assistant 图不计入：图片仅对 user 角色有输入语义。
        let grouped = group(vec![
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
        ]);
        assert!(
            grouped
                .iter()
                .all(|m| m.role == "user" || m.images.is_empty())
        );
    }

    // ── 中断轮标记 ────────────────────────────────────────────────────

    /// 冻结轮的真实形状：悬空 FC 被合成 Output，末尾追加 assistant 说明，
    /// 两者来源都是 `InterruptedTurn`（且都带真实原因）。
    fn frozen_turn() -> Vec<AnnotatedMessage> {
        let reason = || MessageSource::InterruptedTurn {
            reason: "cancelled".to_string(),
        };
        vec![
            AnnotatedMessage::new(
                MessageId(0),
                0,
                InputItem::Message {
                    role: Role::User,
                    content: Content::Text("跑个长任务".to_string()),
                },
                MessageSource::UserInput,
            ),
            AnnotatedMessage::new(
                MessageId(1),
                0,
                InputItem::FunctionCall {
                    call_id: "c1".to_string(),
                    name: "shell".to_string(),
                    arguments: "{}".to_string(),
                },
                MessageSource::ModelGeneration,
            ),
            AnnotatedMessage::new(
                MessageId(2),
                0,
                InputItem::FunctionCallOutput {
                    call_id: "c1".to_string(),
                    output: "[interrupted] tool execution was interrupted before completion".into(),
                },
                reason(),
            ),
            AnnotatedMessage::new(
                MessageId(3),
                0,
                InputItem::Message {
                    role: Role::Assistant,
                    content: Content::Text("[interrupted] cancelled".to_string()),
                },
                reason(),
            ),
        ]
    }

    #[test]
    fn interrupted_turn_marks_assistant_group() {
        let msgs = frozen_turn();
        let grouped = group_input_items(&ItemView::from_turn(&msgs));

        // 末组是中断说明（冻结时的最后一条），标记随来源落到该组。
        let notice = grouped.last().expect("分组非空");
        assert_eq!(notice.role, "assistant");
        assert_eq!(notice.interrupted_reason.as_deref(), Some("cancelled"));

        // 模型产出的工具调用组不该被标成中断产物 —— 否则「带标记」失去区分力。
        let call_group = grouped
            .iter()
            .find(|m| !m.tool_calls.is_empty())
            .expect("存在工具调用组");
        assert!(call_group.interrupted_reason.is_none());

        assert_eq!(
            turn_interrupted_reason(&msgs).as_deref(),
            Some("cancelled"),
            "轮级标记应与组级标记同源"
        );
    }

    #[test]
    fn completed_turn_has_no_interrupted_marker() {
        // 防「所有 assistant 都被标中断」：正常轮不得产出任何标记。
        let msgs = vec![
            AnnotatedMessage::new(
                MessageId(0),
                0,
                InputItem::Message {
                    role: Role::User,
                    content: Content::Text("问".to_string()),
                },
                MessageSource::UserInput,
            ),
            AnnotatedMessage::new(
                MessageId(1),
                0,
                InputItem::Message {
                    role: Role::Assistant,
                    content: Content::Text("答".to_string()),
                },
                MessageSource::ModelGeneration,
            ),
        ];
        let grouped = group_input_items(&ItemView::from_turn(&msgs));
        assert!(grouped.iter().all(|m| m.interrupted_reason.is_none()));
        assert_eq!(turn_interrupted_reason(&msgs), None);
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
