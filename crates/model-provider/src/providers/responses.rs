//! DeepSeek Responses API 适配器。
//!
//! 提供 [`DeepSeekResponsesAdapter`]，为 DeepSeek 原生 `/responses` 端点实现
//! [`ModelProvider`]。请求/响应按设计文档 §9.1 直通映射到中立词汇表
//! （[`GenerateRequest`]/[`GenerateResult`]/[`StreamChunk`]）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;

use crate::providers::sse::{SseEvent, StreamingEventSource};
use crate::providers::streaming::normalize_tool_call_arguments;
use crate::response::{
    BlockType, ContentBlock, FinishReason, GenerateRequest, GenerateResult, InputItem,
    ReasoningConfig, ReasoningEffort, ResponseError, ResponseStatus, Role, StreamChunk, TextConfig,
    TextFormat, ToolChoice,
};
use crate::{GenerateStream, ModelProvider, ProviderError, Usage};

const DEEPSEEK_API_BASE_URL: &str = "https://api.deepseek.com";

// ============================================================================
// 客户端
// ============================================================================

/// DeepSeek Responses API 客户端，实现 [`ModelProvider`]（`generate`/`stream_generate`）。
///
/// 端点：`{base}/responses`。
pub struct DeepSeekResponsesAdapter {
    http_client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl DeepSeekResponsesAdapter {
    /// 使用给定的 API 密钥创建 Responses 适配器。
    pub fn new(api_key: impl Into<String>) -> Result<Self, ProviderError> {
        let http_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| ProviderError::Request(format!("failed to build HTTP client: {e}")))?;
        Ok(Self {
            http_client,
            api_key: api_key.into(),
            base_url: DEEPSEEK_API_BASE_URL.to_string(),
        })
    }

    /// 通过读取 `DEEPSEEK_API_KEY` 环境变量创建。
    pub fn from_env() -> Result<Self, ProviderError> {
        let api_key = std::env::var("DEEPSEEK_API_KEY")
            .map_err(|_| ProviderError::Request("DEEPSEEK_API_KEY 环境变量未设置".to_string()))?;
        Self::new(api_key)
    }

    /// 设置自定义基础 URL。
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 返回 Responses 端点 URL。
    fn responses_endpoint(&self) -> String {
        // DeepSeek 的 responses 端点无 `/v1` 前缀（chat 端点是 `/v1/chat/completions`，
        // 存量配置常把 base_url 写死为 `…/v1`，若不剥离会拼出 `/v1/responses` → 404）。
        let base = self.base_url.trim_end_matches('/');
        let base = base.strip_suffix("/v1").unwrap_or(base);
        format!("{base}/responses")
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_key)
    }
}

// ============================================================================
// 请求构建
// ============================================================================

fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::Developer => "developer",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

/// 构建单个 Responses `input[]` message 元素。
fn message_item(role: Role, content: &str) -> Value {
    serde_json::json!({
        "type": "message",
        "role": role_to_str(role),
        "content": [{ "type": "input_text", "text": content }]
    })
}

/// 构建单个 Responses `input[]` function_call 元素。
fn function_call_item(call_id: &str, name: &str, arguments: &str) -> Value {
    serde_json::json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments
    })
}

/// 构建单个 Responses `input[]` reasoning 元素（thinking 模式回传思考内容）。
///
/// DeepSeek 在 thinking 模式下要求把上一轮产生的 `reasoning_text` 原样回传，
/// 否则工具调用后的后续请求会返回 400。输入侧仅支持明文 `content`，
/// `summary` / `encrypted_content` 不作为输入 —— 因此 [`response_item_to_block`]
/// 只从输出的 `content` 取推理文本，保证这里拿到的一定是 `reasoning_text` 而非摘要。
fn reasoning_item(content: &str) -> Value {
    serde_json::json!({
        "type": "reasoning",
        "content": [{ "type": "reasoning_text", "text": content }]
    })
}

/// 构建单个 Responses `input[]` function_call_output 元素。
fn function_call_output_item(call_id: &str, output: &str) -> Value {
    serde_json::json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": output
    })
}

/// 冲刷一组工具调用：整组 `function_call` 连续输出，紧跟整组 `function_call_output`。
///
/// 组内**不得**插入任何其它元素（assistant 文本插在中间会被 API 判为
/// “No tool output found”），组内 output 的顺序无所谓（可乱序）。
/// 没有配对 output 的 call 必须丢弃 —— 留着会让整个请求以
/// “No tool output found for tool call X” 失败，这类残缺组只出现在轮次被中断的异常历史里。
fn flush_call_group(
    out: &mut Vec<Value>,
    calls: &mut Vec<(String, Value)>,
    outputs: &mut Vec<(String, Value)>,
) {
    if calls.is_empty() {
        outputs.clear();
        return;
    }
    let matched: Vec<(String, Value)> = calls
        .drain(..)
        .filter(|(id, _)| outputs.iter().any(|(oid, _)| oid == id))
        .collect();
    let matched_ids: HashSet<&str> = matched.iter().map(|(id, _)| id.as_str()).collect();

    for (_, call_value) in &matched {
        out.push(call_value.clone());
    }
    for (id, output_value) in outputs.drain(..) {
        if matched_ids.contains(id.as_str()) {
            out.push(output_value);
        }
    }
}

/// 冲刷暂存的 assistant 文本。
fn flush_text(out: &mut Vec<Value>, pending: &mut Option<String>) {
    if let Some(text) = pending.take()
        && !text.is_empty()
    {
        out.push(message_item(Role::Assistant, &text));
    }
}

/// 将有序 [`InputItem`] 列表合并为 Responses `input[]` 元素。
///
/// 与 chat 适配器的 `input_items_to_wire_messages` 对称，但排布必须额外满足 DeepSeek
/// thinking 模式的回放约束（以下均为对 `/responses` 实测得出）：
///
/// 1. **按迭代分组，不逐对配对**：模型一次迭代产出「reasoning →（可选文本）→ 若干
///    function_call」，工具结果随后到达。回放必须保持这个分组 ——
///    `call_a, out_a, call_b, out_b` 这种逐对交错会让 `call_b` 前面没有 reasoning，
///    API 报 “The `reasoning_text` in the thinking mode must be passed back”。
///    正确形状是 `reasoning, call_a, call_b, out_a, out_b`。
/// 2. 组内 output 可乱序，但组内不能插入其它元素（见 [`flush_call_group`]）。
/// 3. assistant 文本放在 reasoning 与 call 组**之间**是允许的；夹在 call 组和 output
///    组之间则不允许 —— 所以组开启期间到达的文本一律推迟到整组冲刷之后。
/// 4. **reasoning 必须是一段 assistant span 的第一个元素**（span = 一个 user 消息或一组
///    output 之后、到下一个 user 消息之前的全部 assistant 产出）。assistant 文本排在
///    reasoning 之前同样会触发 “reasoning_text … must be passed back”，即使该 reasoning
///    确实在请求里。`SimpleAgentLooper` 为了迁就 chat 适配器的反向合并会把 Text 落在
///    Reasoning 之前，而 `AgentLooper` 按模型输出顺序落库（Reasoning 在前）—— 本函数统一
///    归位，因此对两种落库顺序都成立。
///
/// 相邻 assistant 文本会合并，避免同一轮被拆成多个 message 元素。
/// `Reasoning` 是否回传由 `carry_reasoning` 决定，判据见 [`should_carry_reasoning`]。
fn input_items_to_responses_values(items: &[Arc<InputItem>], carry_reasoning: bool) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();

    // 当前工具调用组：本次迭代的全部 function_call 及其 output（按出现顺序）。
    let mut calls: Vec<(String, Value)> = Vec::new();
    let mut outputs: Vec<(String, Value)> = Vec::new();
    // 暂存的 assistant 文本；组开启期间到达的文本推迟到组冲刷之后再输出。
    let mut pending_text: Option<String> = None;

    for item in items {
        match &**item {
            InputItem::Reasoning { content } => {
                // 不回传时直接丢弃：这类 reasoning 会被 API 忽略，留着只占上下文。
                if carry_reasoning {
                    // reasoning 标志着新一轮迭代开始，先收束上一组。
                    flush_call_group(&mut out, &mut calls, &mut outputs);
                    // reasoning 必须排在暂存文本**之前**（约束 4）：先 push reasoning
                    // 再冲刷文本，把 assistant 文本归位到 reasoning 之后。
                    out.push(reasoning_item(content));
                    flush_text(&mut out, &mut pending_text);
                }
            }
            InputItem::Message { role, content } => match role {
                Role::System | Role::Developer | Role::User => {
                    flush_call_group(&mut out, &mut calls, &mut outputs);
                    flush_text(&mut out, &mut pending_text);
                    out.push(message_item(*role, content));
                }
                // 仅累积，实际输出时机由下面各分支的 flush_text 决定（相邻文本自然合并）。
                Role::Assistant => match &mut pending_text {
                    Some(existing) => existing.push_str(content),
                    None => pending_text = Some(content.clone()),
                },
            },
            InputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                // 已经收到过 output 却又来新 call —— 说明上一组已结束，先收束。
                if !outputs.is_empty() {
                    flush_call_group(&mut out, &mut calls, &mut outputs);
                }
                // 组尚未开启时，先把文本放到组前面（约束 3 允许的位置）。
                if calls.is_empty() {
                    flush_text(&mut out, &mut pending_text);
                }
                calls.push((
                    call_id.clone(),
                    function_call_item(call_id, name, arguments),
                ));
            }
            InputItem::FunctionCallOutput { call_id, output } => {
                outputs.push((call_id.clone(), function_call_output_item(call_id, output)));
            }
        }
    }

    flush_call_group(&mut out, &mut calls, &mut outputs);
    flush_text(&mut out, &mut pending_text);

    out
}

/// 判定历史中的 `Reasoning` 是否需要回传。
///
/// 不变式：**只要 `function_call` 被回放，其前置 `reasoning` 就必须一并回放** —— DeepSeek
/// thinking 模式下二者缺一，下一轮会返回 400。而 [`input_items_to_responses_values`] 回放
/// `function_call` 是无条件的（历史里有就写进 `input[]`），所以判据不能只看本次请求是否携带
/// tools：无工具的后续轮、子 Agent、轮次之间改配置，都会让 `tools` 为空而历史仍含工具调用 ——
/// 那恰好就是漏传 reasoning 的场景。
///
/// 因此：携带 tools（本轮可能产生新的工具调用）**或**历史含工具调用（需成对回放）时回传；
/// reasoning 显式关闭时一律不回传，避免与关闭态自相矛盾。
fn should_carry_reasoning(
    items: &[Arc<InputItem>],
    has_tools: bool,
    reasoning_enabled: bool,
) -> bool {
    if !reasoning_enabled {
        return false;
    }
    has_tools
        || items
            .iter()
            .any(|item| matches!(&**item, InputItem::FunctionCall { .. }))
}

fn reasoning_config_to_value(reasoning: Option<&ReasoningConfig>) -> Option<Value> {
    let config = reasoning?;
    if !config.enabled {
        return Some(serde_json::json!({ "effort": "none" }));
    }
    config.effort.map(|e| {
        serde_json::json!({ "effort": match e {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::Max => "max",
        }})
    })
}

fn text_config_to_value(text: Option<&TextConfig>) -> Option<Value> {
    let config = text?;
    match &config.format {
        Some(TextFormat::Text) => Some(serde_json::json!({ "format": { "type": "text" } })),
        Some(TextFormat::JsonObject) => {
            Some(serde_json::json!({ "format": { "type": "json_object" } }))
        }
        Some(TextFormat::JsonSchema { name, schema }) => Some(serde_json::json!({
            "format": { "type": "json_schema", "name": name, "schema": schema }
        })),
        None => None,
    }
}

fn tool_choice_to_value(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => serde_json::json!("auto"),
        ToolChoice::None => serde_json::json!({ "type": "none" }),
        ToolChoice::Required => serde_json::json!({ "type": "required" }),
        ToolChoice::Named { name } => serde_json::json!({ "type": "function", "name": name }),
    }
}

/// 构建 Responses 请求体。
fn build_responses_request_body(
    request: &GenerateRequest,
    stream: bool,
) -> Result<Vec<u8>, ProviderError> {
    // reasoning 是否启用：未显式配置（None）按 provider 默认启用；否则看 enabled。
    // 与下方 `reasoning_config_to_value` 共用同一约定 —— 该函数在「启用但未指定 effort」时
    // 同样省略 `reasoning` 字段、交由 provider 默认，即「省略 == 启用」。两处解读必须同步，
    // 否则回传判定会与请求体实际声明的推理开关脱节。
    let reasoning_enabled = request.reasoning.as_ref().is_none_or(|r| r.enabled);
    let carry_reasoning =
        should_carry_reasoning(&request.input, !request.tools.is_empty(), reasoning_enabled);
    let input = input_items_to_responses_values(&request.input, carry_reasoning);

    let mut body = serde_json::Map::new();
    body.insert("model".into(), serde_json::json!(request.model));
    if let Some(instructions) = &request.instructions {
        body.insert("instructions".into(), serde_json::json!(instructions));
    }
    body.insert("input".into(), Value::Array(input));

    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters
                })
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }

    if let Some(choice) = &request.tool_choice {
        body.insert("tool_choice".into(), tool_choice_to_value(choice));
    }
    if let Some(t) = request.temperature {
        body.insert("temperature".into(), serde_json::json!(t));
    }
    if let Some(p) = request.top_p {
        body.insert("top_p".into(), serde_json::json!(p));
    }
    if let Some(m) = request.max_output_tokens {
        body.insert("max_output_tokens".into(), serde_json::json!(m));
    }
    if let Some(r) = reasoning_config_to_value(request.reasoning.as_ref()) {
        body.insert("reasoning".into(), r);
    }
    if let Some(t) = text_config_to_value(request.text.as_ref()) {
        body.insert("text".into(), t);
    }
    if stream {
        body.insert("stream".into(), serde_json::json!(true));
        body.insert(
            "stream_options".into(),
            serde_json::json!({ "include_usage": true }),
        );
    }
    if let Some(extra) = &request.additional_params
        && let Value::Object(map) = extra
    {
        for (k, v) in map {
            body.insert(k.clone(), v.clone());
        }
    }

    let bytes = serde_json::to_vec(&Value::Object(body))
        .map_err(|e| ProviderError::Request(format!("序列化 responses 请求失败: {e}")))?;
    Ok(bytes)
}

// ============================================================================
// 响应解析
// ============================================================================

/// Responses API 非流式响应。
#[derive(Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    output: Vec<Value>,
    #[serde(default)]
    error: Option<ResponsesError>,
    #[serde(default)]
    usage: Option<ResponsesUsage>,
}

#[derive(Deserialize)]
struct ResponsesError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
struct ResponsesUsage {
    input_tokens: u32,
    output_tokens: u32,
    total_tokens: u32,
}

fn responses_status_to_response_status(status: Option<&str>) -> ResponseStatus {
    match status {
        Some("completed") => ResponseStatus::Completed,
        Some("incomplete") => ResponseStatus::Incomplete,
        Some("failed") => ResponseStatus::Failed,
        _ => ResponseStatus::Completed,
    }
}

fn responses_error_to_response_error(err: &ResponsesError) -> ResponseError {
    ResponseError {
        code: err.code.clone(),
        message: err.message.clone().unwrap_or_else(|| "unknown".to_string()),
    }
}

fn responses_usage_to_usage(u: ResponsesUsage) -> Usage {
    Usage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        total_tokens: u.total_tokens,
    }
}

/// 将 Responses 返回的 `arguments` 字段规范化为 raw JSON 字符串：
/// 字符串原样、对象/数组等序列化为 JSON；缺失或 `null` 返回 `None`（由调用方回退）。
fn arguments_value_to_string(arg: Option<&Value>) -> Option<String> {
    match arg {
        Some(Value::Null) | None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(v) => Some(v.to_string()),
    }
}

/// 将 Responses 输出 item 映射为中立 [`ContentBlock`]（非流式与流式 `output_item.done` 共用）。
fn response_item_to_block(item: &Value) -> Option<ContentBlock> {
    let item_type = item.get("type").and_then(Value::as_str)?;
    match item_type {
        "message" => {
            let mut text = String::new();
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                for part in content {
                    if part.get("type").and_then(Value::as_str) == Some("output_text")
                        && let Some(t) = part.get("text").and_then(Value::as_str)
                    {
                        text.push_str(t);
                    }
                }
            }
            if text.is_empty() {
                None
            } else {
                Some(ContentBlock::Text { text })
            }
        }
        "reasoning" => {
            // `content`（完整推理）与 `summary`（摘要）可能并存，这里只取 `content`：
            // 输入侧没有 `summary_text` 的对应形式（见 [`reasoning_item`]），把 summary
            // 也收进 `ContentBlock::Reasoning` 会让它在下一轮被当作 `reasoning_text` 回传。
            // summary-only 的 item 视为「无可回传推理」直接丢弃。
            let mut text = String::new();
            if let Some(parts) = item.get("content").and_then(Value::as_array) {
                for part in parts {
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                    }
                }
            }
            if text.is_empty() {
                None
            } else {
                Some(ContentBlock::Reasoning { text })
            }
        }
        "function_call" => {
            let call_id = item.get("call_id").and_then(Value::as_str)?.to_string();
            let name = item.get("name").and_then(Value::as_str)?.to_string();
            let arguments = arguments_value_to_string(item.get("arguments"))
                .unwrap_or_else(|| "{}".to_string());
            Some(ContentBlock::ToolCall {
                call_id,
                name,
                arguments,
            })
        }
        _ => None,
    }
}

fn parse_usage_from_response(resp: &Value) -> Option<Usage> {
    let usage = resp.get("usage")?;
    Some(Usage {
        input_tokens: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        output_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
    })
}

// ============================================================================
// 流式处理
// ============================================================================

/// 正在累积的 Responses 工具调用。
struct PendingResponseToolCall {
    call_id: String,
    name: String,
    arguments: String,
}

/// 处理 Responses 语义 SSE 流，产出中立 [`StreamChunk`]。
///
/// 语义事件 → chunk 映射（§9.1）：
/// - `output_item.added` → `BlockStart`（按 item 类型）+ 函数调用 `ToolCallDelta{name}`
/// - `output_text.delta` → `TextDelta`
/// - `reasoning_text.delta` → `ReasoningDelta`
/// - `function_call_arguments.delta` → `ToolCallDelta{arguments}`
/// - `output_item.done` → `BlockEnd`
/// - `completed`/`incomplete` → `Usage` + `Finish`
/// - `failed` → 错误（嵌套于 `response.error`，非顶层 `error`）
///
/// `output_index` 天然单调且区分每个输出 item，直接用作 `StreamChunk` 的 index。
fn process_responses_sse_stream(
    event_source: StreamingEventSource,
    span: tracing::Span,
    endpoint: String,
    model: String,
) -> GenerateStream {
    let stream = stream! {
        let _guard = span.enter();
        tracing::debug!(
            target: "model_provider::responses",
            "开始 responses SSE 流式处理 (端点={}, 模型={})",
            endpoint, model
        );

        let mut text_buffers: HashMap<usize, String> = HashMap::new();
        let mut reasoning_buffers: HashMap<usize, String> = HashMap::new();
        let mut tool_calls: HashMap<usize, PendingResponseToolCall> = HashMap::new();
        let mut started: HashSet<usize> = HashSet::new();
        let mut usage: Option<Usage> = None;
        let mut finish_reason: Option<FinishReason> = None;
        let mut terminated_with_error = false;

        futures::pin_mut!(event_source);

        while let Some(event_result) = event_source.next().await {
            let data = match event_result {
                Ok(SseEvent::Open) => continue,
                Ok(SseEvent::Message(msg_event)) => msg_event.data,
                Err(provider_err) => {
                    terminated_with_error = true;
                    yield Err(provider_err);
                    break;
                }
            };

            if data.trim().is_empty() || data.trim() == "[DONE]" {
                continue;
            }

            let event: Value = match serde_json::from_str(&data) {
                Ok(v) => v,
                Err(e) => {
                    terminated_with_error = true;
                    yield Err(ProviderError::Stream(format!("解析 responses SSE 事件失败: {e}")));
                    break;
                }
            };
            let Some(event_type) = event.get("type").and_then(Value::as_str).map(str::to_string) else {
                continue;
            };

            match event_type.as_str() {
                "response.output_item.added" => {
                    let Some(output_index) = event.get("output_index").and_then(Value::as_u64).map(|i| i as usize) else {
                        continue;
                    };
                    let Some(item) = event.get("item") else { continue; };
                    let Some(item_type) = item.get("type").and_then(Value::as_str) else { continue; };

                    match item_type {
                        "message" => {
                            if started.insert(output_index) {
                                yield Ok(StreamChunk::BlockStart {
                                    index: output_index,
                                    block_type: BlockType::Text,
                                });
                            }
                            text_buffers.entry(output_index).or_default();
                        }
                        "reasoning" => {
                            if started.insert(output_index) {
                                yield Ok(StreamChunk::BlockStart {
                                    index: output_index,
                                    block_type: BlockType::Reasoning,
                                });
                            }
                            reasoning_buffers.entry(output_index).or_default();
                        }
                        "function_call" => {
                            if started.insert(output_index) {
                                yield Ok(StreamChunk::BlockStart {
                                    index: output_index,
                                    block_type: BlockType::ToolCall,
                                });
                            }
                            let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or_default().to_string();
                            let name = item.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                            tool_calls.insert(output_index, PendingResponseToolCall {
                                call_id: call_id.clone(),
                                name: name.clone(),
                                arguments: String::new(),
                            });
                            yield Ok(StreamChunk::ToolCallDelta {
                                index: output_index,
                                call_id,
                                name: Some(name),
                                arguments: Value::String(String::new()),
                            });
                        }
                        _ => {}
                    }
                }
                "response.output_text.delta" => {
                    let Some(output_index) = event.get("output_index").and_then(Value::as_u64).map(|i| i as usize) else {
                        continue;
                    };
                    let Some(delta) = event.get("delta").and_then(Value::as_str) else { continue; };
                    if !delta.is_empty() {
                        text_buffers.entry(output_index).or_default().push_str(delta);
                        yield Ok(StreamChunk::TextDelta { index: output_index, delta: delta.to_string() });
                    }
                }
                "response.reasoning_text.delta" => {
                    let Some(output_index) = event.get("output_index").and_then(Value::as_u64).map(|i| i as usize) else {
                        continue;
                    };
                    let Some(delta) = event.get("delta").and_then(Value::as_str) else { continue; };
                    if !delta.is_empty() {
                        reasoning_buffers.entry(output_index).or_default().push_str(delta);
                        yield Ok(StreamChunk::ReasoningDelta { index: output_index, delta: delta.to_string() });
                    }
                }
                "response.function_call_arguments.delta" => {
                    let Some(output_index) = event.get("output_index").and_then(Value::as_u64).map(|i| i as usize) else {
                        continue;
                    };
                    let Some(delta) = event.get("delta").and_then(Value::as_str) else { continue; };
                    if let Some(tc) = tool_calls.get_mut(&output_index) {
                        normalize_tool_call_arguments(&mut tc.arguments, delta);
                    }
                    let call_id = tool_calls.get(&output_index).map(|t| t.call_id.clone()).unwrap_or_default();
                    yield Ok(StreamChunk::ToolCallDelta {
                        index: output_index,
                        call_id,
                        name: None,
                        arguments: Value::String(delta.to_string()),
                    });
                }
                "response.output_item.done" => {
                    let Some(output_index) = event.get("output_index").and_then(Value::as_u64).map(|i| i as usize) else {
                        continue;
                    };
                    let item = event.get("item");
                    let block = match item.and_then(|i| i.get("type")).and_then(Value::as_str) {
                        Some("function_call") => {
                            let call_id = item
                                .and_then(|i| i.get("call_id"))
                                .and_then(Value::as_str)
                                .map(|s| s.to_string())
                                .or_else(|| tool_calls.get(&output_index).map(|t| t.call_id.clone()));
                            let name = item
                                .and_then(|i| i.get("name"))
                                .and_then(Value::as_str)
                                .map(|s| s.to_string())
                                .or_else(|| tool_calls.get(&output_index).map(|t| t.name.clone()));
                            let arguments = arguments_value_to_string(
                                item.and_then(|i| i.get("arguments")),
                            )
                            .or_else(|| tool_calls.get(&output_index).map(|t| t.arguments.clone()))
                            .unwrap_or_else(|| "{}".to_string());
                            match (call_id, name) {
                                (Some(call_id), Some(name)) => {
                                    Some(ContentBlock::ToolCall { call_id, name, arguments })
                                }
                                _ => None,
                            }
                        }
                        _ => item.and_then(response_item_to_block),
                    };
                    if let Some(block) = block {
                        yield Ok(StreamChunk::BlockEnd { index: output_index, block });
                    }
                    text_buffers.remove(&output_index);
                    reasoning_buffers.remove(&output_index);
                    tool_calls.remove(&output_index);
                    started.remove(&output_index);
                }
                "response.completed" | "response.incomplete" => {
                    if let Some(resp) = event.get("response") {
                        usage = parse_usage_from_response(resp);
                    }
                    if event_type == "response.completed" {
                        finish_reason = Some(FinishReason::Stop);
                    } else {
                        // 从 incomplete_details.reason 区分截断 / 内容过滤等原因。
                        let reason = event
                            .get("response")
                            .and_then(|r| r.get("incomplete_details"))
                            .and_then(|d| d.get("reason"))
                            .and_then(Value::as_str);
                        if let Some(r) = reason
                            && r != "max_output_tokens"
                        {
                            tracing::warn!(
                                target: "model_provider::responses",
                                reason = r,
                                "responses 流以非 max_output_tokens 原因不完整结束"
                            );
                        }
                        finish_reason = Some(match reason {
                            Some("content_filter") => FinishReason::Error,
                            _ => FinishReason::MaxTokens,
                        });
                    }
                    // `completed`/`incomplete` 是语义流终止事件：立即收敛，避免依赖
                    // SSE EOF（服务端保活或重连会导致挂起直到外层超时）。
                    break;
                }
                "response.failed" => {
                    let body = event
                        .get("response")
                        .and_then(|r| r.get("error"))
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("responses API 失败（无错误详情）")
                        .to_string();
                    tracing::warn!(
                        target: "model_provider::responses",
                        %body,
                        "responses API 返回 failed"
                    );
                    terminated_with_error = true;
                    yield Err(ProviderError::Api { status: 500, body });
                    break;
                }
                _ => {}
            }
        }

        if terminated_with_error {
            return;
        }

        // ── 流结束：为未产出 BlockEnd 的块合成（安全网，正常流程在 output_item.done 已产出）──
        for (idx, text) in text_buffers.drain() {
            if !text.is_empty() {
                yield Ok(StreamChunk::BlockEnd { index: idx, block: ContentBlock::Text { text } });
            }
        }
        for (idx, reasoning) in reasoning_buffers.drain() {
            if !reasoning.is_empty() {
                yield Ok(StreamChunk::BlockEnd { index: idx, block: ContentBlock::Reasoning { text: reasoning } });
            }
        }
        for (idx, tc) in tool_calls.drain() {
            if !tc.call_id.is_empty() && !tc.name.is_empty() {
                let mut args = tc.arguments;
                if args.is_empty() || args.trim() == "null" {
                    args = "{}".to_string();
                }
                yield Ok(StreamChunk::BlockEnd {
                    index: idx,
                    block: ContentBlock::ToolCall { call_id: tc.call_id, name: tc.name, arguments: args },
                });
            }
        }

        yield Ok(StreamChunk::Usage { usage: usage.unwrap_or_default() });
        yield Ok(StreamChunk::Finish { reason: finish_reason.unwrap_or(FinishReason::Stop) });
    };

    GenerateStream::new(Box::pin(stream))
}

// ============================================================================
// ModelProvider 实现
// ============================================================================

#[async_trait]
impl ModelProvider for DeepSeekResponsesAdapter {
    fn name(&self) -> &str {
        "deepseek-responses"
    }

    async fn generate(&self, request: &GenerateRequest) -> Result<GenerateResult, ProviderError> {
        let body = build_responses_request_body(request, false)?;
        let endpoint = self.responses_endpoint();

        let response = self
            .http_client
            .post(&endpoint)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await?;

        let status = response.status();
        let response_body = response.bytes().await?;

        if !status.is_success() {
            let body_str = String::from_utf8_lossy(&response_body).to_string();
            tracing::warn!(
                target: "model_provider::responses",
                status = status.as_u16(),
                body = %body_str,
                "DeepSeek responses API 返回错误状态"
            );
            return Err(ProviderError::Api {
                status: status.as_u16(),
                body: body_str,
            });
        }

        let api_response: ResponsesResponse = serde_json::from_slice(&response_body)?;
        let response_status = responses_status_to_response_status(api_response.status.as_deref());
        let output: Vec<ContentBlock> = api_response
            .output
            .iter()
            .filter_map(response_item_to_block)
            .collect();
        let error = if response_status == ResponseStatus::Failed {
            api_response
                .error
                .as_ref()
                .map(responses_error_to_response_error)
                .or_else(|| {
                    Some(ResponseError {
                        code: None,
                        message: "responses API failed".to_string(),
                    })
                })
        } else {
            None
        };

        Ok(GenerateResult {
            id: api_response.id,
            output,
            usage: api_response
                .usage
                .map(responses_usage_to_usage)
                .unwrap_or_default(),
            status: response_status,
            error,
        })
    }

    async fn stream_generate(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateStream, ProviderError> {
        let body = build_responses_request_body(request, true)?;
        let endpoint = self.responses_endpoint();
        let model = request.model.clone();

        let span = tracing::info_span!("deepseek_responses_stream", model = %model);

        let event_source = StreamingEventSource::new(
            self.http_client.clone(),
            endpoint.clone(),
            body,
            self.auth_header(),
        );

        Ok(process_responses_sse_stream(
            event_source,
            span,
            endpoint,
            model,
        ))
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn make_request() -> GenerateRequest {
        GenerateRequest {
            model: "deepseek-v4-pro".to_string(),
            instructions: Some("你是一个助手".to_string()),
            input: Arc::from([Arc::new(InputItem::Message {
                role: Role::User,
                content: "你好".to_string(),
            })]),
            tools: vec![],
            tool_choice: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            reasoning: None,
            text: None,
            additional_params: None,
        }
    }

    #[test]
    fn test_build_request_body_basic() {
        let request = make_request();
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["model"], "deepseek-v4-pro");
        assert_eq!(json["instructions"], "你是一个助手");
        let input = json["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["text"], "你好");
    }

    #[test]
    fn test_build_request_body_with_tools_flat() {
        let mut request = make_request();
        request.tools = vec![crate::ToolDefinition {
            name: "get_weather".to_string(),
            description: "获取天气".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        }];
        request.tool_choice = Some(ToolChoice::None);
        let body = build_responses_request_body(&request, true).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["name"], "get_weather");
        assert_eq!(json["tools"][0]["description"], "获取天气");
        // Responses 工具定义是扁平的，无嵌套 function 包裹
        assert!(json["tools"][0].get("function").is_none());
        assert_eq!(json["tool_choice"], serde_json::json!({ "type": "none" }));
        assert_eq!(json["stream"], true);
    }

    #[test]
    fn test_build_request_body_preserves_reasoning_when_tools_present() {
        let mut request = make_request();
        request.tools = vec![crate::ToolDefinition {
            name: "get_weather".to_string(),
            description: "获取天气".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        }];
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "hi".to_string(),
            }),
            Arc::new(InputItem::Reasoning {
                content: "思考".to_string(),
            }),
        ]);
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let input = json["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        // 携带 tools 时，reasoning 项以 thinking 模式要求的 reasoning_text 形式回传。
        assert_eq!(input[1]["type"], "reasoning");
        assert_eq!(input[1]["content"][0]["type"], "reasoning_text");
        assert_eq!(input[1]["content"][0]["text"], "思考");
    }

    #[test]
    fn test_build_request_body_drops_reasoning_when_no_tools() {
        let mut request = make_request();
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "hi".to_string(),
            }),
            Arc::new(InputItem::Reasoning {
                content: "思考".to_string(),
            }),
        ]);
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        // 未携带 tools 且历史无工具调用时，reasoning 会被 API 忽略，序列化时直接丢弃。
        assert_eq!(json["input"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_build_request_body_keeps_reasoning_for_replayed_calls_without_tools() {
        // 历史含 function_call 时其回放是无条件的，即便本轮 tools 为空
        // （无工具的后续轮 / 子 Agent / 轮次间改配置），前置 reasoning 也必须一并回传，
        // 否则 DeepSeek thinking 模式会对这次请求返回 400。
        let mut request = make_request();
        request.input = Arc::from([
            Arc::new(InputItem::Reasoning {
                content: "思考".to_string(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "get_weather".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: "72F".to_string(),
            }),
        ]);
        assert!(request.tools.is_empty());

        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let types: Vec<&str> = json["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            types,
            vec!["reasoning", "function_call", "function_call_output"]
        );
    }

    #[test]
    fn test_build_request_body_drops_reasoning_when_disabled() {
        // reasoning 显式关闭时不回传，避免与关闭态自相矛盾 —— 即使历史含工具调用。
        let mut request = make_request();
        request.reasoning = Some(ReasoningConfig {
            enabled: false,
            effort: None,
        });
        request.input = Arc::from([
            Arc::new(InputItem::Reasoning {
                content: "思考".to_string(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "get_weather".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: "72F".to_string(),
            }),
        ]);

        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let types: Vec<&str> = json["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, vec!["function_call", "function_call_output"]);
        assert_eq!(json["reasoning"], serde_json::json!({ "effort": "none" }));
    }

    #[test]
    fn test_response_output_to_blocks() {
        let output = serde_json::json!([
            {
                "type": "reasoning",
                "content": [{ "type": "reasoning_text", "text": "思考中" }]
            },
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "{\"city\":\"SF\"}"
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "今天晴天" }]
            }
        ]);
        let blocks: Vec<ContentBlock> = output
            .as_array()
            .unwrap()
            .iter()
            .filter_map(response_item_to_block)
            .collect();
        assert_eq!(blocks.len(), 3);
        assert!(matches!(blocks[0], ContentBlock::Reasoning { .. }));
        assert!(matches!(blocks[1], ContentBlock::ToolCall { .. }));
        assert!(matches!(blocks[2], ContentBlock::Text { .. }));
    }

    #[test]
    fn test_reasoning_summary_only_is_dropped() {
        // 输入侧没有 summary_text 形式，summary-only 的推理无法合法回传；
        // 若收进 ContentBlock 只会在下一轮被误标为 reasoning_text。
        let summary_only = serde_json::json!({
            "type": "reasoning",
            "summary": [{ "type": "summary_text", "text": "摘要" }]
        });
        assert!(response_item_to_block(&summary_only).is_none());

        // content 与 summary 并存时只取 content，不拼接。
        let both = serde_json::json!({
            "type": "reasoning",
            "content": [{ "type": "reasoning_text", "text": "完整推理" }],
            "summary": [{ "type": "summary_text", "text": "摘要" }]
        });
        match response_item_to_block(&both) {
            Some(ContentBlock::Reasoning { text }) => assert_eq!(text, "完整推理"),
            other => panic!("expected Reasoning, got {other:?}"),
        }
    }

    #[test]
    fn test_response_item_function_call_arguments_forms() {
        // 对象形式 arguments → 序列化为 JSON 字符串（而非静默退化为 "{}"）
        let object_form = serde_json::json!({
            "type": "function_call",
            "call_id": "call_1",
            "name": "get_weather",
            "arguments": { "city": "SF" }
        });
        match response_item_to_block(&object_form) {
            Some(ContentBlock::ToolCall { arguments, .. }) => {
                assert_eq!(arguments, "{\"city\":\"SF\"}");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }

        // 字符串形式 arguments → 原样保留
        let string_form = serde_json::json!({
            "type": "function_call",
            "call_id": "call_1",
            "name": "get_weather",
            "arguments": "{\"city\":\"SF\"}"
        });
        match response_item_to_block(&string_form) {
            Some(ContentBlock::ToolCall { arguments, .. }) => {
                assert_eq!(arguments, "{\"city\":\"SF\"}");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn test_responses_status_mapping() {
        assert_eq!(
            responses_status_to_response_status(Some("completed")),
            ResponseStatus::Completed
        );
        assert_eq!(
            responses_status_to_response_status(Some("incomplete")),
            ResponseStatus::Incomplete
        );
        assert_eq!(
            responses_status_to_response_status(Some("failed")),
            ResponseStatus::Failed
        );
        assert_eq!(
            responses_status_to_response_status(None),
            ResponseStatus::Completed
        );
    }

    #[test]
    fn test_responses_endpoint() {
        let client = DeepSeekResponsesAdapter::new("sk-test")
            .unwrap()
            .with_base_url("https://custom.example.com");
        assert_eq!(
            client.responses_endpoint(),
            "https://custom.example.com/responses"
        );
        assert_eq!(client.name(), "deepseek-responses");
    }

    #[test]
    fn test_responses_endpoint_strips_v1() {
        // 存量配置常把 base_url 写死为 `…/v1`（chat 端点），responses 端点需剥离。
        let client = DeepSeekResponsesAdapter::new("sk-test")
            .unwrap()
            .with_base_url("https://api.deepseek.com/v1");
        assert_eq!(
            client.responses_endpoint(),
            "https://api.deepseek.com/responses"
        );

        let trailing = DeepSeekResponsesAdapter::new("sk-test")
            .unwrap()
            .with_base_url("https://api.deepseek.com/v1/");
        assert_eq!(
            trailing.responses_endpoint(),
            "https://api.deepseek.com/responses"
        );
    }

    #[test]
    fn test_input_items_merge_and_pair_calls() {
        let items = vec![
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "hi".to_string(),
            }),
            Arc::new(InputItem::Message {
                role: Role::Assistant,
                content: "让我查一下".to_string(),
            }),
            Arc::new(InputItem::Reasoning {
                content: "思考中".to_string(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "get_weather".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: "72F".to_string(),
            }),
            Arc::new(InputItem::Message {
                role: Role::Assistant,
                content: "今天晴天".to_string(),
            }),
        ];
        let values = input_items_to_responses_values(&items, true);
        // assistant 文本被归位到 reasoning 之后（约束 4），call 组与 output 组各自成块。
        assert_eq!(
            types_of(&values),
            vec![
                "message",
                "reasoning",
                "message",
                "function_call",
                "function_call_output",
                "message",
            ]
        );
        assert_eq!(values[2]["content"][0]["text"], "让我查一下");
        assert_eq!(values[3]["call_id"], "c1");
        assert_eq!(values[4]["call_id"], "c1");
    }

    /// 提取 `input[]` 的 type 序列，便于断言排布。
    fn types_of(values: &[Value]) -> Vec<&str> {
        values.iter().map(|v| v["type"].as_str().unwrap()).collect()
    }

    #[test]
    fn test_parallel_calls_stay_grouped() {
        // 回归：单轮并行工具调用曾被逐对配对成 call_a, out_a, call_b, out_b，
        // 使 call_b 前面没有 reasoning，DeepSeek thinking 模式报
        // "The `reasoning_text` in the thinking mode must be passed back to the API."
        // 正确排布是整组 call 紧跟整组 output。
        let items = vec![
            Arc::new(InputItem::Reasoning {
                content: "同时查两个城市".to_string(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "a".to_string(),
                name: "get_weather".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "b".to_string(),
                name: "get_weather".to_string(),
                arguments: "{}".to_string(),
            }),
            // 并发执行下 output 可能乱序到达 —— API 允许组内乱序。
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "b".to_string(),
                output: "Tokyo 20C".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "a".to_string(),
                output: "Paris 18C".to_string(),
            }),
        ];
        let values = input_items_to_responses_values(&items, true);
        assert_eq!(
            types_of(&values),
            vec![
                "reasoning",
                "function_call",
                "function_call",
                "function_call_output",
                "function_call_output",
            ]
        );
        assert_eq!(values[1]["call_id"], "a");
        assert_eq!(values[2]["call_id"], "b");
    }

    #[test]
    fn test_multi_iteration_groups_keep_own_reasoning() {
        // 多轮 ReAct：每一组 call 前都必须有自己的 reasoning，组间不能粘连。
        let items = vec![
            Arc::new(InputItem::Reasoning {
                content: "r1".to_string(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "t".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: "o1".to_string(),
            }),
            Arc::new(InputItem::Reasoning {
                content: "r2".to_string(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "c2".to_string(),
                name: "t".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c2".to_string(),
                output: "o2".to_string(),
            }),
        ];
        let values = input_items_to_responses_values(&items, true);
        assert_eq!(
            types_of(&values),
            vec![
                "reasoning",
                "function_call",
                "function_call_output",
                "reasoning",
                "function_call",
                "function_call_output",
            ]
        );
    }

    #[test]
    fn test_unpaired_call_is_dropped() {
        // 组内每个 call 都必须有配对 output，否则 API 报
        // "No tool output found for tool call X" —— 轮次被中断的残缺组直接丢弃。
        let items = vec![
            Arc::new(InputItem::Reasoning {
                content: "r".to_string(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "done".to_string(),
                name: "t".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "interrupted".to_string(),
                name: "t".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "done".to_string(),
                output: "ok".to_string(),
            }),
        ];
        let values = input_items_to_responses_values(&items, true);
        assert_eq!(
            types_of(&values),
            vec!["reasoning", "function_call", "function_call_output"]
        );
        assert_eq!(values[1]["call_id"], "done");
    }

    #[test]
    fn test_reasoning_normalized_before_assistant_text() {
        // 回归：SimpleAgentLooper（workflow 的 agent 步骤）把 Text 落在 Reasoning 之前，
        // 而 API 要求 reasoning 必须是 assistant span 的第一个元素，否则报
        // "The `reasoning_text` in the thinking mode must be passed back to the API."
        // 编码器需把文本归位到 reasoning 之后。
        let items = vec![
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "查天气".to_string(),
            }),
            // ↓ SimpleAgentLooper 的落库顺序：Text 在 Reasoning 之前
            Arc::new(InputItem::Message {
                role: Role::Assistant,
                content: "我查一下".to_string(),
            }),
            Arc::new(InputItem::Reasoning {
                content: "需要调用工具".to_string(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "get_weather".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: "18C".to_string(),
            }),
        ];
        let values = input_items_to_responses_values(&items, true);
        assert_eq!(
            types_of(&values),
            vec![
                "message",
                "reasoning",
                "message",
                "function_call",
                "function_call_output",
            ]
        );
        assert_eq!(values[0]["role"], "user");
        assert_eq!(values[2]["role"], "assistant");
        assert_eq!(values[2]["content"][0]["text"], "我查一下");
    }

    #[test]
    fn test_adjacent_assistant_text_is_merged() {
        let items = vec![
            Arc::new(InputItem::Message {
                role: Role::Assistant,
                content: "前半".to_string(),
            }),
            Arc::new(InputItem::Message {
                role: Role::Assistant,
                content: "后半".to_string(),
            }),
        ];
        let values = input_items_to_responses_values(&items, true);
        assert_eq!(types_of(&values), vec!["message"]);
        assert_eq!(values[0]["content"][0]["text"], "前半后半");
    }

    #[test]
    fn test_input_items_reorders_text_after_call() {
        // 工具调用后紧跟的 assistant 文本应后置到 output 之后，保持 call→output 相邻。
        let items = vec![
            Arc::new(InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "get_weather".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::Message {
                role: Role::Assistant,
                content: "预览文本".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: "72F".to_string(),
            }),
        ];
        let values = input_items_to_responses_values(&items, true);
        let types: Vec<&str> = values.iter().map(|v| v["type"].as_str().unwrap()).collect();
        assert_eq!(
            types,
            vec!["function_call", "function_call_output", "message"]
        );
    }
}
