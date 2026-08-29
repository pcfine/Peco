// ============================================================================
// SSE 事件类型定义 + LooperEvent → SSE Event 映射
// ============================================================================

use axum::response::sse::Event;
use model_provider::Usage;
use peco_core::agent::{LooperEvent, TurnFailureReason, TurnOutcome, strip_summary_wrapper};
use serde::Serialize;

/// SSE 事件类型（发给前端）。
///
/// 每种事件类型映射到 SSE `event:` 字段，data 为 JSON 序列化后的内容。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum ChatSseEvent {
    /// 文本增量（逐 token 输出）
    #[serde(rename = "text_delta")]
    TextDelta {
        content: String,
        conversation_id: String,
    },

    /// 推理过程增量（DeepSeek reasoning_content）
    #[serde(rename = "reasoning_delta")]
    ReasoningDelta {
        content: String,
        conversation_id: String,
    },

    /// 工具调用开始
    #[serde(rename = "tool_call_start")]
    ToolCallStart {
        id: String,
        name: String,
        arguments: String,
        conversation_id: String,
    },

    /// 工具执行结果
    #[serde(rename = "tool_result")]
    ToolResult {
        id: String,
        name: String,
        result: String,
        conversation_id: String,
    },

    /// 本轮对话完成
    #[serde(rename = "turn_complete")]
    TurnComplete {
        text: String,
        usage: UsageData,
        conversation_id: String,
    },

    /// 子 Agent 调用开始。
    ///
    /// `call_id` 是关联 `AgentCallStart` 与 `AgentCallEnd` 的唯一标识：
    /// - `delegate_sub_agent`：直接使用 LLM 生成的 tool_call_id
    /// - `run_parallel_sub_agents`：使用 `{tool_call_id}:{index}` 以区分并行任务
    #[serde(rename = "agent_call_start")]
    AgentCallStart {
        /// 关联 ID，与对应的 AgentCallEnd.call_id 匹配
        call_id: String,
        agent_id: String,
        agent_name: String,
        task: String,
        conversation_id: String,
    },

    /// 子 Agent 调用结束。
    ///
    /// `call_id` 与对应的 `AgentCallStart.call_id` 一致，前端可通过此字段配对。
    #[serde(rename = "agent_call_end")]
    AgentCallEnd {
        /// 关联 ID，与对应的 AgentCallStart.call_id 匹配
        call_id: String,
        agent_id: String,
        agent_name: String,
        /// 子 Agent 执行结果（delegate_sub_agent 为完整输出；
        /// run_parallel_sub_agents 为单任务的 JSON 结果）。
        result: String,
        conversation_id: String,
    },

    /// 错误
    #[serde(rename = "error")]
    Error {
        message: String,
        conversation_id: String,
    },

    /// 流结束
    #[serde(rename = "done")]
    Done {
        usage: UsageData,
        conversation_id: String,
    },

    /// 上下文用量快照（每次模型调用后发出）。
    ///
    /// `input_tokens` 为本次调用的 prompt 长度，即当前上下文占用，
    /// 前端据此计算上下文窗口使用百分比。
    #[serde(rename = "usage")]
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        conversation_id: String,
    },

    /// 上下文滚动压缩完成（Peco 永续会话）。
    ///
    /// 更早的对话轮次已被结构化摘要替换并物理驱逐，
    /// 前端据此渲染「更早的对话已归档」分隔线。
    #[serde(rename = "context_compacted")]
    ContextCompacted {
        evicted_turns: usize,
        summary: String,
        conversation_id: String,
    },
}

/// Token 用量数据（精简版，供前端展示）。
#[derive(Debug, Clone, Serialize)]
pub struct UsageData {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl From<Usage> for UsageData {
    fn from(u: Usage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
        }
    }
}

impl ChatSseEvent {
    /// 转换为 axum SSE Event。
    pub fn to_sse_event(&self) -> Result<Event, serde_json::Error> {
        let data = serde_json::to_string(self)?;
        let event_name = match self {
            ChatSseEvent::TextDelta { .. } => "text_delta",
            ChatSseEvent::ReasoningDelta { .. } => "reasoning_delta",
            ChatSseEvent::ToolCallStart { .. } => "tool_call_start",
            ChatSseEvent::ToolResult { .. } => "tool_result",
            ChatSseEvent::TurnComplete { .. } => "turn_complete",
            ChatSseEvent::AgentCallStart { .. } => "agent_call_start",
            ChatSseEvent::AgentCallEnd { .. } => "agent_call_end",
            ChatSseEvent::Error { .. } => "error",
            ChatSseEvent::Done { .. } => "done",
            ChatSseEvent::Usage { .. } => "usage",
            ChatSseEvent::ContextCompacted { .. } => "context_compacted",
        };
        Ok(Event::default().event(event_name).data(data))
    }
}

// ── 子 Agent 事件关联类型 ───────────────────────────────────────────────────────

/// 子 Agent 调用信息，在 ToolCallStart 阶段写入，ToolResult 阶段读取。
///
/// `call_id` 是前端配对 `AgentCallStart` ↔ `AgentCallEnd` 的唯一标识。
#[derive(Debug, Clone)]
pub struct SubAgentInfo {
    pub call_id: String,
    pub agent_id: String,
    pub agent_name: String,
}

/// 从工具调用参数中解析子 Agent 信息。
///
/// - `delegate_sub_agent`：返回单个 SubAgentInfo，`call_id = tool_call_id`
/// - `run_parallel_sub_agents`：返回多个 SubAgentInfo，`call_id = "{tool_call_id}:{index}"`
///
/// `resolve_agent_id` 用于将 agent_name 映射为 agent_id。
pub fn parse_sub_agent_infos(
    tool_call_id: &str,
    tool_name: &str,
    arguments: &str,
    resolve_agent_id: impl Fn(&str) -> String,
) -> Vec<SubAgentInfo> {
    if tool_name == "delegate_sub_agent" {
        if let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) {
            let agent_name = args["agent_name"].as_str().unwrap_or("unknown");
            return vec![SubAgentInfo {
                call_id: tool_call_id.to_string(),
                agent_id: resolve_agent_id(agent_name),
                agent_name: agent_name.to_string(),
            }];
        }
        return vec![];
    }

    if tool_name == "run_parallel_sub_agents" {
        if let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments)
            && let Some(tasks) = args["tasks"].as_array()
        {
            return tasks
                .iter()
                .enumerate()
                .map(|(index, task)| {
                    let agent_name = task["agent_name"].as_str().unwrap_or("unknown");
                    SubAgentInfo {
                        call_id: format!("{tool_call_id}:{index}"),
                        agent_id: resolve_agent_id(agent_name),
                        agent_name: agent_name.to_string(),
                    }
                })
                .collect();
        }
        return vec![];
    }

    vec![]
}

/// 从子 Agent tool result 中提取单个子 Agent 的输出。
///
/// - `delegate_sub_agent`：result 就是子 Agent 完整输出，直接返回
/// - `run_parallel_sub_agents`：result 是 JSON 数组，按 agent_name 匹配提取
pub fn extract_sub_agent_result(tool_result: &str, info: &SubAgentInfo, tool_name: &str) -> String {
    if tool_name == "delegate_sub_agent" {
        return tool_result.to_string();
    }

    if let Ok(results) = serde_json::from_str::<Vec<serde_json::Value>>(tool_result) {
        for item in &results {
            if item["agent_name"].as_str() == Some(&info.agent_name) {
                if let Some(output) = item["output"].as_str() {
                    return output.to_string();
                }
                if let Some(error) = item["error"].as_str() {
                    return format!("[error] {error}");
                }
                return item.to_string();
            }
        }
    }

    let preview: String = tool_result.chars().take(200).collect();
    if preview.len() < tool_result.len() {
        format!("{preview}...")
    } else {
        preview
    }
}

/// 将 `TurnFailureReason` 格式化为面向用户的消息（随 `error` SSE 事件发送）。
fn format_failure_message(reason: &TurnFailureReason, partial_text: &str) -> String {
    let reason_msg = match reason {
        TurnFailureReason::Cancelled => "对话已被取消".to_string(),
        TurnFailureReason::TotalTimeout => "总运行超时".to_string(),
        TurnFailureReason::PerTurnTimeout => "本轮响应超时".to_string(),
        TurnFailureReason::MaxTurnsExceeded => "已达到最大轮数限制".to_string(),
        TurnFailureReason::HookAbort(msg) => format!("响应已被中断: {msg}"),
        TurnFailureReason::Other(msg) => msg.clone(),
        // TurnFailureReason 是 #[non_exhaustive]，为未来新增的失败原因兜底
        _ => "对话异常终止".to_string(),
    };
    if partial_text.is_empty() {
        reason_msg
    } else {
        format!("{reason_msg}（响应中断前部分输出已展示）")
    }
}

/// 将 `LooperEvent` 映射为 `Option<ChatSseEvent>`。
///
/// 部分 LooperEvent（如状态转换）不产生面向客户端的 SSE 事件，返回 None。
/// `conversation_id` 用于填充每个事件的会话标识。
pub fn map_looper_event(event: LooperEvent, conversation_id: &str) -> Option<ChatSseEvent> {
    let cid = conversation_id.to_string();
    match event {
        LooperEvent::TextDelta { delta } => Some(ChatSseEvent::TextDelta {
            content: delta,
            conversation_id: cid,
        }),

        LooperEvent::ReasoningDelta { delta } => Some(ChatSseEvent::ReasoningDelta {
            content: delta,
            conversation_id: cid,
        }),

        LooperEvent::ToolCallStart {
            id,
            name,
            arguments,
        } => Some(ChatSseEvent::ToolCallStart {
            id,
            name,
            arguments,
            conversation_id: cid,
        }),

        LooperEvent::ToolResult { id, name, result } => Some(ChatSseEvent::ToolResult {
            id,
            name,
            result,
            conversation_id: cid,
        }),

        LooperEvent::TurnComplete { outcome, usage, .. } => match outcome {
            TurnOutcome::Success { text } => Some(ChatSseEvent::TurnComplete {
                text,
                usage: usage.into(),
                conversation_id: cid,
            }),
            // 失败轮次发出 error 事件（前端据此展示错误提示），丢弃 usage。
            TurnOutcome::Failed {
                reason,
                partial_text,
            } => {
                let message = format_failure_message(&reason, &partial_text);
                Some(ChatSseEvent::Error {
                    message,
                    conversation_id: cid,
                })
            }
        },

        LooperEvent::Shutdown { total_usage, .. } => Some(ChatSseEvent::Done {
            usage: total_usage.into(),
            conversation_id: cid,
        }),

        LooperEvent::ModelUsage { usage, .. } => Some(ChatSseEvent::Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            conversation_id: cid,
        }),

        LooperEvent::ContextCompacted {
            evicted_turns,
            summary,
            ..
        } => Some(ChatSseEvent::ContextCompacted {
            evicted_turns,
            // 与恢复路径（GET /session、归档）一致：剥掉内部定界标签再下发
            summary: strip_summary_wrapper(&summary).to_owned(),
            conversation_id: cid,
        }),

        // 以下事件不产生面向客户端的 SSE 事件
        LooperEvent::ToolCallDelta { .. }
        | LooperEvent::ReactStateChange { .. }
        | LooperEvent::OuterStateChange { .. }
        | LooperEvent::TurnStart { .. }
        | _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_outcome_maps_to_turn_complete() {
        let event = map_looper_event(
            LooperEvent::TurnComplete {
                turn_index: 0,
                outcome: TurnOutcome::Success {
                    text: "你好".to_string(),
                },
                usage: Usage::default(),
            },
            "conv-1",
        );
        assert!(matches!(
            event,
            Some(ChatSseEvent::TurnComplete { ref text, .. }) if text == "你好"
        ));
    }

    #[test]
    fn failed_outcome_maps_to_error_event() {
        // 失败轮次必须携带错误信息发送 error 事件（而非空文本的 turn_complete）
        let event = map_looper_event(
            LooperEvent::TurnComplete {
                turn_index: 0,
                outcome: TurnOutcome::Failed {
                    reason: TurnFailureReason::Other(
                        "Stream error: API error (402): Insufficient Balance".to_string(),
                    ),
                    partial_text: String::new(),
                },
                usage: Usage::default(),
            },
            "conv-1",
        );
        let Some(ChatSseEvent::Error { message, .. }) = event else {
            panic!("failed outcome must map to ChatSseEvent::Error, got {event:?}");
        };
        assert!(message.contains("Insufficient Balance"));
    }

    #[test]
    fn cancelled_outcome_maps_to_friendly_message() {
        let event = map_looper_event(
            LooperEvent::TurnComplete {
                turn_index: 0,
                outcome: TurnOutcome::Failed {
                    reason: TurnFailureReason::Cancelled,
                    partial_text: "部分输出".to_string(),
                },
                usage: Usage::default(),
            },
            "conv-1",
        );
        let Some(ChatSseEvent::Error { message, .. }) = event else {
            panic!("failed outcome must map to ChatSseEvent::Error, got {event:?}");
        };
        assert!(message.contains("取消"));
        assert!(message.contains("部分输出已展示"));
    }
}
