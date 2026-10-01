// ============================================================================
// SSE 事件类型定义 + LooperEvent → SSE Event 映射
// ============================================================================

use axum::response::sse::Event;
use model_provider::Usage;
use peco_core::agent::{
    LooperEvent, RetryNoticeReason, TurnFailureReason, TurnOutcome, strip_summary_wrapper,
};
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
    ///
    /// `images` 携带工具输出中的图片部件（data URI 或 https URL），
    /// 纯文本工具结果为空数组不序列化。
    #[serde(rename = "tool_result")]
    ToolResult {
        id: String,
        name: String,
        result: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        images: Vec<String>,
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

    /// 本轮模型尝试已作废，正在重试。
    ///
    /// **纯通知**：前端不删除任何已收到的 `text_delta`，只在当前轮的气泡
    /// **之前**插一条居中横幅解释这段残句。该次尝试的产出已从 Session
    /// staging 回退，因此只存在于实时视图里，重载后随快照一并消失。
    ///
    /// `reason` 区分两种重发：`truncated`（截断抬预算）与 `transient`
    ///（限流/网络/5xx 退避重发）。wire 名保持 `truncation_retry` 兼容。
    #[serde(rename = "truncation_retry")]
    TruncationRetry {
        /// 第几次重试（1-based），各 reason 独立计数
        attempt: u32,
        /// 本轮对应 reason 的重试上限
        limit: u32,
        /// 截断那次的输出 token 数（transient 重发为 0）
        output_tokens: u32,
        /// 抬升后的输出预算（transient 重发为当前生效预算）
        retry_budget: u32,
        /// 重发原因：`truncated` | `transient`
        reason: String,
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
            ChatSseEvent::TruncationRetry { .. } => "truncation_retry",
        };
        Ok(Event::default().event(event_name).data(data))
    }
}

/// 按后缀剥离：仅当 `target` 确实以 `suffix` 结尾时才移除。
///
/// 供 `chat` 模块的落库累加器对齐 [`LooperEvent::TruncationRetry`] 用 ——
/// 被作废那次的正文已经进了累加器，但它不属于最终答案。不匹配时是**无操作**：
///
/// - 批量路径根本不发增量，载荷是整段文本，累加器里没有对应内容；
/// - 通知若因任何原因迟到或重复，重复剥离会把第二段正确文本也切掉。
///
/// # 不变量（调用方依赖）
/// 通知到达这一刻，累加器必然以载荷结尾：该次尝试的增量是最后追加进累加器的
/// 内容，之后到通知之间只隔一个 `Finish` / `Usage` 块，不可能再有增量。
/// 剥离因此必须**就地发生在通知上**，不能攒到轮次结束再对着最终文本做 ——
/// 那时残句已经变成前缀（后面还跟着重试的正文）。
pub fn strip_suffix(target: &mut String, suffix: &str) {
    if suffix.is_empty() || !target.ends_with(suffix) {
        return;
    }
    target.truncate(target.len() - suffix.len());
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
///
/// 模型类失败（限流/网络/鉴权/额度/上下文/过滤）是「中文分类前缀 + 原始
/// provider msg」— 分类给用户一句话结论，原始 msg 保留诊断细节，
/// 不因分类而丢失上游信息。
fn format_failure_message(reason: &TurnFailureReason, partial_text: &str) -> String {
    let reason_msg = match reason {
        TurnFailureReason::Cancelled => "对话已被取消".to_string(),
        TurnFailureReason::MaxTurnsExceeded => "已达到最大轮数限制".to_string(),
        TurnFailureReason::HookAbort(msg) => format!("响应已被中断: {msg}"),
        TurnFailureReason::RateLimited { attempts, message } => {
            let retries = attempts.saturating_sub(1);
            format!("触发限流，已重试 {retries} 次：{message}")
        }
        TurnFailureReason::ModelUnavailable { attempts, message } => {
            let retries = attempts.saturating_sub(1);
            format!("模型服务暂时不可用，已重试 {retries} 次：{message}")
        }
        TurnFailureReason::AuthError { message } => format!("API Key 无效或无权限：{message}"),
        TurnFailureReason::QuotaExhausted { message } => format!("额度已耗尽：{message}"),
        TurnFailureReason::ContextOverflow { message } => {
            format!("内容超出上下文窗口：{message}")
        }
        TurnFailureReason::ContentFiltered { message } => {
            format!("输出被内容过滤拦截：{message}")
        }
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

        LooperEvent::ToolResult {
            id,
            name,
            result,
            images,
        } => Some(ChatSseEvent::ToolResult {
            id,
            name,
            result,
            images,
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

        // 通知里刻意**不带** `discarded_text`：它只为落库侧对齐服务，下发
        // 会让「前端不删已发增量」这条约定多出一个诱人的误用口子。
        LooperEvent::TruncationRetry {
            attempt,
            limit,
            output_tokens,
            retry_budget,
            reason,
            ..
        } => Some(ChatSseEvent::TruncationRetry {
            attempt,
            limit,
            output_tokens,
            retry_budget,
            reason: match reason {
                RetryNoticeReason::Transient => "transient",
                // Truncated 与未来新增变体都按截断展示（旧前端不认识新值时
                // 仍能落在默认横幅上）
                _ => "truncated",
            }
            .to_string(),
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
    fn truncation_retry_maps_without_discarded_text() {
        let event = map_looper_event(
            LooperEvent::TruncationRetry {
                turn_index: 3,
                attempt: 1,
                limit: 2,
                output_tokens: 4096,
                retry_budget: 32_768,
                discarded_text: "part".to_string(),
                reason: RetryNoticeReason::Truncated,
            },
            "conv-1",
        );
        let Some(ChatSseEvent::TruncationRetry {
            attempt,
            limit,
            output_tokens,
            retry_budget,
            reason,
            ..
        }) = &event
        else {
            panic!("必须映射为 ChatSseEvent::TruncationRetry，实际 {event:?}");
        };
        assert_eq!((*attempt, *limit), (1, 2));
        assert_eq!((*output_tokens, *retry_budget), (4096, 32_768));
        assert_eq!(reason, "truncated");

        // 下发形态：事件名 + 载荷字段。正文不得出现在线上 —— 前端拿了只会
        // 在「不删已发增量」的约定上多一个诱人的误用口子。
        let json = serde_json::to_value(event.unwrap()).unwrap();
        assert_eq!(json["event"], "truncation_retry");
        assert_eq!(json["data"]["attempt"], 1);
        assert_eq!(json["data"]["retry_budget"], 32_768);
        assert_eq!(json["data"]["reason"], "truncated");
        assert!(
            json["data"].get("discarded_text").is_none(),
            "正文载荷不得下发，实际 {json}"
        );
    }

    #[tokio::test]
    async fn transient_retry_maps_reason_transient() {
        let event = map_looper_event(
            LooperEvent::TruncationRetry {
                turn_index: 0,
                attempt: 1,
                limit: 2,
                output_tokens: 0,
                retry_budget: 0,
                discarded_text: "半个答案".to_string(),
                reason: RetryNoticeReason::Transient,
            },
            "conv-1",
        );
        let json = serde_json::to_value(event.unwrap()).unwrap();
        assert_eq!(json["data"]["reason"], "transient");
    }

    #[test]
    fn typed_failure_reasons_format_chinese_prefix_with_original_message() {
        // 分类前缀给结论，原始 msg 保留诊断 —— 两者都不能缺。
        let cases = vec![
            (
                TurnFailureReason::RateLimited {
                    attempts: 3,
                    message: "Rate limit reached".to_string(),
                },
                "触发限流，已重试 2 次：Rate limit reached",
            ),
            (
                TurnFailureReason::AuthError {
                    message: "Invalid API key".to_string(),
                },
                "API Key 无效或无权限：Invalid API key",
            ),
            (
                TurnFailureReason::QuotaExhausted {
                    message: "insufficient quota".to_string(),
                },
                "额度已耗尽：insufficient quota",
            ),
            (
                TurnFailureReason::ContextOverflow {
                    message: "context_length_exceeded".to_string(),
                },
                "内容超出上下文窗口：context_length_exceeded",
            ),
            (
                TurnFailureReason::ContentFiltered {
                    message: "content_filter".to_string(),
                },
                "输出被内容过滤拦截：content_filter",
            ),
            (
                TurnFailureReason::ModelUnavailable {
                    attempts: 1,
                    message: "connection reset".to_string(),
                },
                "模型服务暂时不可用，已重试 0 次：connection reset",
            ),
        ];
        for (reason, expected) in cases {
            assert_eq!(format_failure_message(&reason, ""), expected);
        }
    }

    #[test]
    fn strip_suffix_removes_only_matching_suffix() {
        let mut acc = "prepart".to_string();
        strip_suffix(&mut acc, "part");
        assert_eq!(acc, "pre");

        // 不匹配 → 无操作（批量路径的整段文本、迟到的重复通知）
        let mut acc = "done".to_string();
        strip_suffix(&mut acc, "part");
        assert_eq!(acc, "done");

        // 空载荷 → 无操作
        let mut acc = "done".to_string();
        strip_suffix(&mut acc, "");
        assert_eq!(acc, "done");

        // 中文按字节切不越界
        let mut acc = "前言残句".to_string();
        strip_suffix(&mut acc, "残句");
        assert_eq!(acc, "前言");

        // 重复剥离是幂等的：第二次不再匹配
        strip_suffix(&mut acc, "残句");
        assert_eq!(acc, "前言");
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
