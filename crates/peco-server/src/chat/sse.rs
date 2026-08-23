// ============================================================================
// SSE 事件类型定义 + LooperEvent → SSE Event 映射
// ============================================================================

use axum::response::sse::Event;
use model_provider::Usage;
use peco_core::agent::LooperEvent;
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

        LooperEvent::TurnComplete { outcome, usage, .. } => {
            let text = outcome.text().unwrap_or("").to_string();
            Some(ChatSseEvent::TurnComplete {
                text,
                usage: usage.into(),
                conversation_id: cid,
            })
        }

        LooperEvent::Shutdown { total_usage, .. } => Some(ChatSseEvent::Done {
            usage: total_usage.into(),
            conversation_id: cid,
        }),

        LooperEvent::ModelUsage { usage, .. } => Some(ChatSseEvent::Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
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
