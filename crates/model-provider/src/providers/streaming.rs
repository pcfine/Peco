//! 供各提供商 SSE 管线使用的共享流式基础设施。
//!
//! 本模块将与提供商无关的通用流式状态机（SSE 事件分发、
//! 文本/推理/工具调用累积、用量跟踪、正常的流结束清理）
//! 集中到一个与提供商无关的 [`StreamingProfile`] trait 背后。
//! 各个提供商只需要实现数据块规范化即可。

use std::collections::HashMap;

use async_stream::stream;
use futures::StreamExt;

use crate::providers::sse::{SseEvent, StreamingEventSource};
use crate::{ChatStream, ProviderError, StreamEvent, ToolCall, Usage};

// ============================================================================
// 中间表示类型
// ============================================================================

/// 与提供商无关的单个 SSE 数据块表示。
#[derive(Debug, Clone)]
pub(crate) struct NormalizedChunk {
    /// 文本内容增量（如果有）。
    pub text: Option<String>,
    /// 推理/思考内容增量（如果有）。
    pub reasoning: Option<String>,
    /// 来自此数据块的工具调用增量。
    pub tool_calls: Vec<NormalizedToolCall>,
    /// 此数据块对应选项的结束原因（例如 "stop"、"tool_calls"）。
    pub finish_reason: Option<String>,
    /// 累积的用量信息（如果此数据块中包含）。
    pub usage: Option<NormalizedUsage>,
}

/// 与提供商无关的单个工具调用增量表示。
#[derive(Debug, Clone)]
pub(crate) struct NormalizedToolCall {
    /// 在 tool_calls 数组中的位置。
    pub index: usize,
    /// 工具调用 ID（部分数据块中可能缺失）。
    pub id: Option<String>,
    /// 函数名称（可能缺失）。
    pub name: Option<String>,
    /// 参数片段（可能缺失）。
    pub arguments: Option<String>,
}

/// 与提供商无关的 token 用量数据。
#[derive(Debug, Clone, Copy)]
pub(crate) struct NormalizedUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// 跟踪正在由流式增量累积的进行中的工具调用。
#[derive(Debug, Clone)]
pub(crate) struct PendingToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

// ============================================================================
// StreamingProfile Trait
// ============================================================================

/// 流式数据块处理的提供商标定自定义。
///
/// 实现者将原始 SSE 数据解析为 [`NormalizedChunk`]，
/// 并通过可选钩子控制工具调用状态机行为。
pub(crate) trait StreamingProfile: Send {
    /// 将原始 SSE `data:` 载荷解析为规范化的数据块。
    ///
    /// 如果该数据应被静默跳过（例如无法解析但非致命的内容），
    /// 则返回 `Ok(None)`。
    fn normalize_chunk(&self, data: &str) -> Result<Option<NormalizedChunk>, ProviderError>;

    /// 当 `finish_reason` 为指定值时是否应触发刷新所有待处理的工具调用。
    /// 默认：`"tool_calls"` 时返回 `true`。
    fn is_tool_calls_finish_reason(&self, reason: &str) -> bool {
        reason == "tool_calls"
    }

    /// 此提供商是否需要独立工具调用淘汰逻辑
    /// （新的工具调用替换同一索引位置的现有调用）。
    fn uses_distinct_tool_call_eviction(&self) -> bool {
        false
    }

    /// 此提供商是否在单个 SSE 数据块中发出完整的工具调用
    /// （id + name + 完整的 JSON 参数都在一个增量中到达）。
    /// 为 true 时，此类调用将立即产出而不进行累积。
    fn emits_complete_single_chunk_tool_calls(&self) -> bool {
        false
    }

    /// 将累积的提供商用量转换为公共的 [`Usage`] 类型。
    fn convert_usage(&self, usage: NormalizedUsage) -> Usage {
        Usage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        }
    }
}

// ============================================================================
// 共享辅助函数
// ============================================================================

/// 检查 SSE 数据行是否包含 API 错误载荷（例如 `{"error": {...}}`）。
///
/// 如果载荷是错误，返回 `Some(ProviderError::Api)`；如果是正常数据块
/// （含有 `choices`）或无法解析为 JSON，返回 `None`。
pub(crate) fn provider_error_from_sse_data(data: &str) -> Option<ProviderError> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    // 如果含有 "choices" 键，则是正常数据块，非错误
    if value.get("error").is_none() || value.get("choices").is_some() {
        return None;
    }
    if let Some(message) = value
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(serde_json::Value::as_str)
    {
        tracing::warn!(
            target: "model_provider::streaming",
            %message,
            "提供商返回了流式错误事件"
        );
    }
    Some(ProviderError::Api {
        status: 500,
        body: data.to_string(),
    })
}

/// 将片段追加到累积的工具调用参数中，尽可能规范化为有效的 JSON。
/// 处理某些网关在流式传输实际参数片段之前发出的 `"null"` 占位符。
pub(crate) fn normalize_tool_call_arguments(accumulated: &mut String, fragment: &str) {
    // 如果现有内容是 "null" 占位符且传入片段是真实的，
    // 则丢弃占位符。
    if accumulated.trim() == "null" && !fragment.trim().is_empty() {
        accumulated.clear();
    }
    accumulated.push_str(fragment);
    // 如果累积的字符串看起来是完整的 JSON 对象，
    // 则解析并规范化为紧凑表示，以确保确定性输出。
    let trimmed = accumulated.trim();
    if trimmed.starts_with('{')
        && trimmed.ends_with('}')
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        *accumulated = parsed.to_string();
    }
}

/// 判断传入的工具调用增量是否应淘汰同一索引位置的
/// 现有待处理工具调用（同一位置开始了不同的工具调用）。
pub(crate) fn should_evict_tool_call(
    existing: &PendingToolCall,
    incoming: &NormalizedToolCall,
) -> bool {
    let Some(new_id) = incoming.id.as_deref() else {
        return false;
    };
    let Some(new_name) = incoming.name.as_deref() else {
        return false;
    };
    if new_id.is_empty() || new_name.is_empty() {
        return false;
    }
    if existing.id.is_empty() || existing.name.is_empty() {
        return false;
    }
    existing.id != new_id && existing.name != new_name
}

/// 当单个传入的 [`NormalizedToolCall`] 同时包含完整的名称和
/// 语法上完整的 JSON 参数（以 `{` 开头、以 `}` 结尾）时返回 `true`。
/// 此类工具调用可以立即产出，无需进一步累积。
pub(crate) fn is_complete_single_chunk(tc: &NormalizedToolCall) -> bool {
    tc.id.as_ref().is_some_and(|id| !id.is_empty())
        && tc.name.as_ref().is_some_and(|n| !n.is_empty())
        && tc.arguments.as_ref().is_some_and(|a| {
            let a = a.trim();
            a.starts_with('{') && a.ends_with('}')
        })
}

/// 将已完成的待处理工具调用转换为完整的 [`ToolCall`]。
///
/// - 丢弃缺少 id 或 name 的调用。
/// - 将空字符串 / `"null"` 参数规范化为 `"{}"`。
/// - 当参数形成有效的 JSON 对象时，将其压缩为规范形式。
///
/// 返回的 [`ToolCall`] 可以直接执行；使用者不应
/// 尝试从单个流式增量重建它。
pub(crate) fn into_tool_call(pending: &PendingToolCall) -> Option<ToolCall> {
    if pending.id.is_empty() {
        tracing::debug!(
            target: "model_provider::streaming",
            tool_name = %pending.name,
            "丢弃不完整的工具调用：缺少 id"
        );
        return None;
    }
    if pending.name.is_empty() {
        tracing::debug!(
            target: "model_provider::streaming",
            tool_id = %pending.id,
            "丢弃不完整的工具调用：缺少 name"
        );
        return None;
    }
    let mut args = pending.arguments.clone();
    if args.is_empty() || args.trim() == "null" {
        args = "{}".to_string();
    }
    // 当参数形成有效的 JSON 对象时，规范化为紧凑 JSON。
    if let Ok(v @ serde_json::Value::Object(_)) = serde_json::from_str::<serde_json::Value>(&args) {
        args = v.to_string();
    }
    Some(ToolCall::new(
        pending.id.clone(),
        pending.name.clone(),
        args,
    ))
}

// ============================================================================
// 共享流式管线
// ============================================================================

/// 通过提供商无关的管线处理 SSE 事件源。
///
/// 此函数封装了所有通用的流式处理逻辑：
/// - SSE 事件分发（Open / Message / Error）
/// - [DONE] 哨兵值和空数据跳过
/// - SSE 载荷中的 API 错误检测
/// - 文本 / 推理增量产出
/// - 工具调用的累积、淘汰和最终化
/// - 用量跟踪
/// - 正常的流结束清理
///
/// 提供商标定的数据块解析委托给 `profile`。
pub(crate) fn process_normalized_sse_stream<P: StreamingProfile + 'static>(
    event_source: StreamingEventSource,
    profile: P,
    span: tracing::Span,
    endpoint: String,
    model: String,
) -> ChatStream {
    let stream = stream! {
        let _guard = span.enter();

        tracing::debug!(
            target: "model_provider::streaming",
            "开始 SSE 流式处理 (端点={}, 模型={})",
            endpoint,
            model
        );

        let mut pending_tool_calls: HashMap<usize, PendingToolCall> = HashMap::new();
        let mut accumulated_usage: Option<NormalizedUsage> = None;
        let mut terminated_with_error = false;

        futures::pin_mut!(event_source);

        while let Some(event_result) = event_source.next().await {
            // ── SSE 事件分发 ──
            let data = match event_result {
                Ok(SseEvent::Open) => {
                    tracing::debug!(
                        target: "model_provider::streaming",
                        "SSE 连接已打开"
                    );
                    continue;
                }
                Ok(SseEvent::Message(msg_event)) => msg_event.data,
                Err(provider_err) => {
                    terminated_with_error = true;
                    yield Err(provider_err);
                    break;
                }
            };

            // ── 哨兵值 / 空数据跳过 ──
            if data.trim().is_empty() || data.trim() == "[DONE]" {
                continue;
            }

            // ── API 错误检测 ──
            if let Some(error) = provider_error_from_sse_data(&data) {
                terminated_with_error = true;
                yield Err(error);
                break;
            }

            // ── 提供商标定的数据块规范化 ──
            let chunk: NormalizedChunk = match profile.normalize_chunk(&data) {
                Ok(Some(c)) => c,
                Ok(None) => continue,
                Err(err) => {
                    terminated_with_error = true;
                    yield Err(err);
                    break;
                }
            };

            // ── 用量跟踪 ──
            if let Some(usage) = chunk.usage {
                accumulated_usage = Some(usage);
            }

            // ── 文本增量 ──
            if let Some(ref text) = chunk.text
                && !text.is_empty()
            {
                yield Ok(StreamEvent::TextDelta(text.clone()));
            }

            // ── 推理增量 ──
            if let Some(ref reasoning) = chunk.reasoning
                && !reasoning.is_empty()
            {
                yield Ok(StreamEvent::ReasoningDelta(reasoning.clone()));
            }

            // ── 工具调用处理 ──
            for tc in &chunk.tool_calls {
                // ---- 淘汰检查 ----
                if profile.uses_distinct_tool_call_eviction()
                    && let Some(existing) = pending_tool_calls.get(&tc.index)
                    && should_evict_tool_call(existing, tc)
                {
                    if let Some(tc) = into_tool_call(existing) {
                        yield Ok(StreamEvent::ToolCallComplete(tc));
                    }
                    pending_tool_calls.remove(&tc.index);
                }

                // ---- 单数据块完整工具调用 ----
                if profile.emits_complete_single_chunk_tool_calls()
                    && is_complete_single_chunk(tc)
                {
                    if let Some(ref id) = tc.id
                        && let Some(ref name) = tc.name
                        && let Some(ref args_str) = tc.arguments
                    {
                        // 产出完整组装的工具调用 — agent
                        // 会在没有前置增量时
                        // 据此生成显示事件（Name + Arguments）。
                        yield Ok(StreamEvent::ToolCallComplete(ToolCall::new(
                            id.clone(),
                            name.clone(),
                            args_str.clone(),
                        )));
                    }
                    pending_tool_calls.remove(&tc.index);
                    continue;
                }

                // ---- 增量累积 ----
                let entry = pending_tool_calls
                    .entry(tc.index)
                    .or_insert_with(|| PendingToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });

                // ID 更新 — 如果发生变化，刷新旧调用并重新开始
                if let Some(ref id) = tc.id {
                    if !entry.id.is_empty() && entry.id != *id {
                        if let Some(tc) = into_tool_call(entry) {
                            yield Ok(StreamEvent::ToolCallComplete(tc));
                        }
                        entry.arguments.clear();
                    }
                    entry.id = id.clone();
                }

                // 名称更新 — 增量产出
                if let Some(ref name) = tc.name {
                    if !entry.name.is_empty() && entry.name != *name {
                        if let Some(tc) = into_tool_call(entry) {
                            yield Ok(StreamEvent::ToolCallComplete(tc));
                        }
                        entry.arguments.clear();
                    }
                    entry.name = name.clone();

                    yield Ok(StreamEvent::ToolCallDelta {
                        id: entry.id.clone(),
                        name: Some(name.clone()),
                        arguments: serde_json::Value::String(String::new()),
                    });
                }

                // 参数更新 — 增量产出
                if let Some(ref args) = tc.arguments {
                    normalize_tool_call_arguments(&mut entry.arguments, args);
                    yield Ok(StreamEvent::ToolCallDelta {
                        id: entry.id.clone(),
                        name: None,
                        arguments: serde_json::Value::String(args.clone()),
                    });
                }
            }

            // ── 结束原因刷新 ──
            // 内联处理：在 stream! 宏中 yield 不能跨闭包边界。
            if let Some(ref reason) = chunk.finish_reason
                && profile.is_tool_calls_finish_reason(reason)
            {
                let mut indices: Vec<usize> =
                    pending_tool_calls.keys().copied().collect();
                indices.sort();
                for idx in indices {
                    if let Some(pending) = pending_tool_calls.remove(&idx)
                        && let Some(tc) = into_tool_call(&pending)
                    {
                        yield Ok(StreamEvent::ToolCallComplete(tc));
                    }
                }
            }
        }

        // ── 流结束处理 ──
        if terminated_with_error {
            return;
        }

        // 刷新剩余的待处理工具调用（按索引排序以
        // 确保确定性的顺序）。
        let mut indices: Vec<usize> =
            pending_tool_calls.keys().copied().collect();
        indices.sort();
        for idx in indices {
            if let Some(pending) = pending_tool_calls.remove(&idx)
                && let Some(tc) = into_tool_call(&pending)
            {
                yield Ok(StreamEvent::ToolCallComplete(tc));
            }
        }

        let usage = accumulated_usage
            .map(|u| profile.convert_usage(u))
            .unwrap_or_default();
        yield Ok(StreamEvent::End { usage });
    };

    ChatStream::new(Box::pin(stream))
}
