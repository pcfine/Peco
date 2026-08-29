//! 供各提供商 SSE 管线使用的共享流式基础设施。
//!
//! 本模块将与提供商无关的通用流式状态机（SSE 事件分发、
//! 文本/推理/工具调用累积、用量跟踪、正常的流结束清理）
//! 集中到一个与提供商无关的 [`StreamingProfile`] trait 背后。
//! 各个提供商只需要实现数据块规范化即可。

use std::collections::{HashMap, HashSet};

use async_stream::stream;
use futures::StreamExt;

use crate::response::{BlockType, ContentBlock, FinishReason, StreamChunk};
use crate::streaming::sse::{SseEvent, StreamingEventSource};
use crate::{GenerateStream, ProviderError, ToolCall, Usage};

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
// 中立 StreamChunk 流式管线（chat 适配器 generate_stream 用）
// ============================================================================

/// 文本块的固定 index（chat 适配器流式合成）。
const TEXT_BLOCK_INDEX: usize = 0;
/// 推理块的固定 index。
const REASONING_BLOCK_INDEX: usize = 1;
/// 工具调用块的 index 基（工具调用块 index = `TOOL_BLOCK_INDEX_BASE + wire index`）。
const TOOL_BLOCK_INDEX_BASE: usize = 2;

/// 将已完成的待处理工具调用转换为 [`ContentBlock::ToolCall`]。
///
/// 复用 [`into_tool_call`] 的规范化逻辑（丢弃缺 id/name、空参数归一化为 `"{}"`），
/// 仅在其成功产出时返回块。
fn pending_to_content_block(pending: &PendingToolCall) -> Option<ContentBlock> {
    into_tool_call(pending).map(|tc| ContentBlock::ToolCall {
        call_id: tc.id,
        name: tc.function.name,
        arguments: tc.function.arguments,
    })
}

/// 将 chat 协议的 `finish_reason` 映射为中立 [`FinishReason`]。
fn finish_reason_to_finish(reason: Option<&str>) -> FinishReason {
    match reason {
        Some("stop") => FinishReason::Stop,
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("length") => FinishReason::MaxTokens,
        Some("content_filter") => {
            tracing::warn!(
                wire_finish_reason = reason.unwrap(),
                "Provider finished with content_filter, mapped to Error"
            );
            FinishReason::Error
        }
        None => FinishReason::Stop,
        Some(other) => {
            tracing::warn!(
                wire_finish_reason = other,
                "Unknown finish_reason from provider, mapped to Error"
            );
            FinishReason::Error
        }
    }
}

/// 以中立 [`StreamChunk`] 为出口的 chat 适配器流式管线
/// （供 `generate_stream` 使用）。
///
/// 关键差异：
/// - 文本/推理增量在流结束时合成为完整的 `BlockEnd`（适配器生成的单调 index）。
/// - 工具调用在 `BlockStart` 首次出现 name 时、`BlockEnd` 在完成时发出，
///   与 wire 的 `tool_calls[].index` 偏移 [`TOOL_BLOCK_INDEX_BASE`] 避免碰撞。
/// - 结束时发出 `Usage` + `Finish`，由 [`crate::BlockAssembler`] 折叠。
pub(crate) fn process_normalized_sse_stream_chunks<P: StreamingProfile + 'static>(
    event_source: StreamingEventSource,
    profile: P,
    span: tracing::Span,
    model: String,
    request_id: String,
) -> GenerateStream {
    // 「开始流式处理」不再单独打点：调用方（provider 的 `generate_stream`）刚打过一条
    // 请求摘要，含相同的 request_id / model / endpoint 以及十几个更有用的字段，
    // 这里再打一条是它的严格子集。流的另一端有终止摘要，起点由请求摘要覆盖。
    let stream = stream! {
        let mut pending_tool_calls: HashMap<usize, PendingToolCall> = HashMap::new();
        let mut started_tool_calls: HashSet<usize> = HashSet::new();
        let mut accumulated_usage: Option<NormalizedUsage> = None;
        let mut text_buffer = String::new();
        let mut reasoning_buffer = String::new();
        let mut text_started = false;
        let mut reasoning_started = false;
        let mut finish_reason: Option<String> = None;
        let mut terminated_with_error = false;

        // ── 诊断计数器 ──
        // 每 chunk 都可能命中的分支不逐条打日志，累积后在流终止摘要里一次性汇报。
        let started_at = std::time::Instant::now();
        let mut first_chunk_at: Option<std::time::Instant> = None;
        let mut event_count: u64 = 0;
        let mut skipped_none_count: u64 = 0;
        let mut text_bytes_total: usize = 0;
        let mut reasoning_bytes_total: usize = 0;
        let mut tool_call_count: u64 = 0;

        futures::pin_mut!(event_source);

        while let Some(event_result) = event_source.next().await {
            let data = match event_result {
                Ok(SseEvent::Open) => continue,
                Ok(SseEvent::Message(msg_event)) => msg_event.data,
                Err(provider_err) => {
                    terminated_with_error = true;
                    tracing::warn!(
                        target: "model_provider::streaming",
                        request_id = %request_id,
                        model = %model,
                        error = %provider_err,
                        event_count,
                        text_bytes = text_bytes_total,
                        reasoning_bytes = reasoning_bytes_total,
                        tool_call_count,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "SSE 流传输错误，中止"
                    );
                    yield Err(provider_err);
                    break;
                }
            };

            event_count += 1;

            if data.trim().is_empty() || data.trim() == "[DONE]" {
                continue;
            }

            if let Some(error) = provider_error_from_sse_data(&data) {
                terminated_with_error = true;
                tracing::warn!(
                    target: "model_provider::streaming",
                    request_id = %request_id,
                    model = %model,
                    error = %error,
                    event_count,
                    text_bytes = text_bytes_total,
                    reasoning_bytes = reasoning_bytes_total,
                    tool_call_count,
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "SSE 流内错误载荷，中止"
                );
                yield Err(error);
                break;
            }

            let chunk: NormalizedChunk = match profile.normalize_chunk(&data) {
                Ok(Some(c)) => c,
                // 无法产出规范化 chunk（如 choices 为空）—— 静默跳过，仅计数。
                Ok(None) => {
                    skipped_none_count += 1;
                    continue;
                }
                Err(err) => {
                    terminated_with_error = true;
                    tracing::warn!(
                        target: "model_provider::streaming",
                        request_id = %request_id,
                        model = %model,
                        error = %err,
                        event_count,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "SSE chunk 规范化失败，中止"
                    );
                    yield Err(err);
                    break;
                }
            };

            if let Some(usage) = chunk.usage {
                accumulated_usage = Some(usage);
            }
            if let Some(ref reason) = chunk.finish_reason {
                finish_reason = Some(reason.clone());
            }

            // ── 文本增量 ──
            if let Some(ref text) = chunk.text
                && !text.is_empty()
            {
                if !text_started {
                    yield Ok(StreamChunk::BlockStart {
                        index: TEXT_BLOCK_INDEX,
                        block_type: BlockType::Text,
                    });
                    text_started = true;
                }
                first_chunk_at.get_or_insert_with(std::time::Instant::now);
                yield Ok(StreamChunk::TextDelta {
                    index: TEXT_BLOCK_INDEX,
                    delta: text.clone(),
                });
                text_buffer.push_str(text);
                text_bytes_total += text.len();
            }

            // ── 推理增量 ──
            if let Some(ref reasoning) = chunk.reasoning
                && !reasoning.is_empty()
            {
                if !reasoning_started {
                    yield Ok(StreamChunk::BlockStart {
                        index: REASONING_BLOCK_INDEX,
                        block_type: BlockType::Reasoning,
                    });
                    reasoning_started = true;
                }
                first_chunk_at.get_or_insert_with(std::time::Instant::now);
                yield Ok(StreamChunk::ReasoningDelta {
                    index: REASONING_BLOCK_INDEX,
                    delta: reasoning.clone(),
                });
                reasoning_buffer.push_str(reasoning);
                reasoning_bytes_total += reasoning.len();
            }

            // ── 工具调用处理 ──
            for tc in &chunk.tool_calls {
                let stream_idx = TOOL_BLOCK_INDEX_BASE + tc.index;

                // ---- 淘汰检查 ----
                if profile.uses_distinct_tool_call_eviction()
                    && let Some(existing) = pending_tool_calls.get(&tc.index)
                    && should_evict_tool_call(existing, tc)
                {
                    // 同一 wire index 上开始了不同的工具调用 —— 上游行为异常的信号。
                    tracing::debug!(
                        target: "model_provider::streaming",
                        request_id = %request_id,
                        index = tc.index,
                        old_id = %existing.id,
                        old_name = %existing.name,
                        new_id = tc.id.as_deref().unwrap_or("-"),
                        new_name = tc.name.as_deref().unwrap_or("-"),
                        "淘汰同索引上的旧工具调用"
                    );
                    if let Some(block) = pending_to_content_block(existing) {
                        yield Ok(StreamChunk::BlockEnd {
                            index: stream_idx,
                            block,
                        });
                    }
                    pending_tool_calls.remove(&tc.index);
                    started_tool_calls.remove(&tc.index);
                }

                // ---- 单数据块完整工具调用 ----
                if profile.emits_complete_single_chunk_tool_calls()
                    && is_complete_single_chunk(tc)
                {
                    let pending = PendingToolCall {
                        id: tc.id.clone().unwrap_or_default(),
                        name: tc.name.clone().unwrap_or_default(),
                        arguments: tc.arguments.clone().unwrap_or_default(),
                    };
                    if let Some(block) = pending_to_content_block(&pending) {
                        if started_tool_calls.insert(tc.index) {
                            yield Ok(StreamChunk::BlockStart {
                                index: stream_idx,
                                block_type: BlockType::ToolCall,
                            });
                        }
                        first_chunk_at.get_or_insert_with(std::time::Instant::now);
                        tool_call_count += 1;
                        yield Ok(StreamChunk::ToolCallDelta {
                            index: stream_idx,
                            call_id: pending.id.clone(),
                            name: Some(pending.name.clone()),
                            arguments: serde_json::Value::String(String::new()),
                        });
                        yield Ok(StreamChunk::ToolCallDelta {
                            index: stream_idx,
                            call_id: pending.id.clone(),
                            name: None,
                            arguments: serde_json::Value::String(pending.arguments.clone()),
                        });
                        yield Ok(StreamChunk::BlockEnd {
                            index: stream_idx,
                            block,
                        });
                    }
                    pending_tool_calls.remove(&tc.index);
                    started_tool_calls.remove(&tc.index);
                    continue;
                }

                // ---- 增量累积 ----
                let entry = pending_tool_calls.entry(tc.index).or_insert_with(|| PendingToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });

                // ID 更新
                if let Some(ref id) = tc.id {
                    if !entry.id.is_empty() && entry.id != *id {
                        tracing::debug!(
                            target: "model_provider::streaming",
                            request_id = %request_id,
                            index = tc.index,
                            old_id = %entry.id,
                            new_id = %id,
                            "同索引工具调用 id 变更，清空已累积参数"
                        );
                        if let Some(block) = pending_to_content_block(entry) {
                            yield Ok(StreamChunk::BlockEnd {
                                index: stream_idx,
                                block,
                            });
                        }
                        entry.arguments.clear();
                        started_tool_calls.remove(&tc.index);
                    }
                    entry.id = id.clone();
                }

                // 名称更新
                if let Some(ref name) = tc.name {
                    if !entry.name.is_empty() && entry.name != *name {
                        tracing::debug!(
                            target: "model_provider::streaming",
                            request_id = %request_id,
                            index = tc.index,
                            old_name = %entry.name,
                            new_name = %name,
                            "同索引工具调用 name 变更，清空已累积参数"
                        );
                        if let Some(block) = pending_to_content_block(entry) {
                            yield Ok(StreamChunk::BlockEnd {
                                index: stream_idx,
                                block,
                            });
                        }
                        entry.arguments.clear();
                        started_tool_calls.remove(&tc.index);
                    }
                    entry.name = name.clone();

                    if started_tool_calls.insert(tc.index) {
                        yield Ok(StreamChunk::BlockStart {
                            index: stream_idx,
                            block_type: BlockType::ToolCall,
                        });
                        first_chunk_at.get_or_insert_with(std::time::Instant::now);
                        tool_call_count += 1;
                    }
                    yield Ok(StreamChunk::ToolCallDelta {
                        index: stream_idx,
                        call_id: entry.id.clone(),
                        name: Some(name.clone()),
                        arguments: serde_json::Value::String(String::new()),
                    });
                }

                // 参数更新
                if let Some(ref args) = tc.arguments {
                    normalize_tool_call_arguments(&mut entry.arguments, args);
                    yield Ok(StreamChunk::ToolCallDelta {
                        index: stream_idx,
                        call_id: entry.id.clone(),
                        name: None,
                        arguments: serde_json::Value::String(args.clone()),
                    });
                }
            }

            // ── 结束原因刷新（tool_calls）──
            if let Some(ref reason) = chunk.finish_reason
                && profile.is_tool_calls_finish_reason(reason)
            {
                let mut indices: Vec<usize> = pending_tool_calls.keys().copied().collect();
                indices.sort();
                for idx in indices {
                    if let Some(pending) = pending_tool_calls.remove(&idx) {
                        if let Some(block) = pending_to_content_block(&pending) {
                            yield Ok(StreamChunk::BlockEnd {
                                index: TOOL_BLOCK_INDEX_BASE + idx,
                                block,
                            });
                        }
                        started_tool_calls.remove(&idx);
                    }
                }
            }
        }

        // ── 流结束处理 ──
        if terminated_with_error {
            return;
        }

        // 刷新剩余的待处理工具调用（按 index 排序）。
        // MaxTokens（"length"）截断时，未闭合的 tool call 不得 flush 成完整 BlockEnd：
        // 留给 [`crate::BlockAssembler`] 的 `Finish{MaxTokens}` 隐式丢弃，避免把畸形
        // FunctionCall 临时 staging 后又在 rollback 时产生半成品工具调用。
        let truncated = matches!(finish_reason.as_deref(), Some("length"));
        if truncated {
            if !pending_tool_calls.is_empty() {
                let mut dropped: Vec<usize> = pending_tool_calls.keys().copied().collect();
                dropped.sort();
                tracing::debug!(
                    target: "model_provider::streaming",
                    request_id = %request_id,
                    indices = ?dropped,
                    count = dropped.len(),
                    "MaxTokens 截断，丢弃未闭合的工具调用"
                );
            }
        } else {
            let mut indices: Vec<usize> = pending_tool_calls.keys().copied().collect();
            indices.sort();
            for idx in indices {
                if let Some(pending) = pending_tool_calls.remove(&idx) {
                    if let Some(block) = pending_to_content_block(&pending) {
                        yield Ok(StreamChunk::BlockEnd {
                            index: TOOL_BLOCK_INDEX_BASE + idx,
                            block,
                        });
                    }
                    started_tool_calls.remove(&idx);
                }
            }
        }

        // 合成文本 / 推理完整块。
        if text_started && !text_buffer.is_empty() {
            yield Ok(StreamChunk::BlockEnd {
                index: TEXT_BLOCK_INDEX,
                block: ContentBlock::Text {
                    text: std::mem::take(&mut text_buffer),
                },
            });
        }
        if reasoning_started && !reasoning_buffer.is_empty() {
            yield Ok(StreamChunk::BlockEnd {
                index: REASONING_BLOCK_INDEX,
                block: ContentBlock::Reasoning {
                    text: std::mem::take(&mut reasoning_buffer),
                },
            });
        }

        let usage = accumulated_usage
            .map(|u| profile.convert_usage(u))
            .unwrap_or_default();
        let reason = finish_reason_to_finish(finish_reason.as_deref());

        tracing::debug!(
            target: "model_provider::streaming",
            request_id = %request_id,
            model = %model,
            event_count,
            text_bytes = text_bytes_total,
            reasoning_bytes = reasoning_bytes_total,
            tool_call_count,
            finish_reason = %reason.as_str(),
            wire_finish_reason = finish_reason.as_deref().unwrap_or("-"),
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            total_tokens = usage.total_tokens,
            skipped_none_count,
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            ttfc_ms = first_chunk_at
                .map(|t| t.duration_since(started_at).as_millis() as u64)
                .unwrap_or(0),
            "SSE 流式处理结束（中立 chunk）"
        );

        yield Ok(StreamChunk::Usage { usage });
        yield Ok(StreamChunk::Finish { reason });
    };

    GenerateStream::new_instrumented(Box::pin(stream), span)
}
