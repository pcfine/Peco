//! OpenAI Responses API 适配器。
//!
//! 提供 [`OpenAiResponsesAdapter`]，为 OpenAI 原生 `/responses` 端点实现
//! [`ModelProvider`]。请求/响应直通映射到中立词汇表
//! （[`GenerateRequest`]/[`GenerateResult`]/[`StreamChunk`]）。
//!
//! 与 [`DeepSeekResponsesAdapter`](crate::DeepSeekResponsesAdapter)、
//! [`QwenResponsesAdapter`](crate::QwenResponsesAdapter) 的通用骨架同构，差异点：
//!
//! 1. 端点拼接**保留** `/v1` 前缀（原生路径即 `/v1/responses`）；
//! 2. reasoning 输出为 `summary` 摘要形态；思考内容**不回传**（无 `store` 时回传
//!    需要加密载体，中立层未承载），历史中的 `Reasoning` 项直接丢弃；
//! 3. `function_call` 与 `function_call_output` 按 `call_id` 配对，无相邻性约束，
//!    历史映射保持原序一一对应；
//! 4. `store` 默认 true，显式置 `false`（无状态全量回放，对话不上云留存）；
//! 5. 系统提示词走顶层 `instructions` 字段，`Role::Developer` 原生直传。

use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use tracing::{Instrument, debug, trace, warn};

use super::chat::{OPENAI_API_BASE_URL, OPENAI_REASONING_MIN_BUDGET};
use crate::logging;
use crate::response::{
    BlockType, Content, ContentBlock, ContentPart, FinishReason, GenerateRequest, GenerateResult,
    InputItem, ReasoningConfig, ReasoningEffort, ResponseError, ResponseStatus, Role, StreamChunk,
    TextConfig, TextFormat, ToolChoice,
};
use crate::streaming::pipeline::normalize_tool_call_arguments;
use crate::streaming::sse::{SseEvent, StreamingEventSource};
use crate::{GenerateStream, ModelProvider, ProviderError, Usage};

// ============================================================================
// 客户端
// ============================================================================

/// OpenAI Responses API 客户端，实现 [`ModelProvider`]（`generate_full`/`generate_stream`）。
///
/// 端点：`{base}/responses`。
pub struct OpenAiResponsesAdapter {
    http_client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl OpenAiResponsesAdapter {
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
            base_url: OPENAI_API_BASE_URL.to_string(),
        })
    }

    /// 通过读取 `OPENAI_API_KEY` 环境变量创建。
    pub fn from_env() -> Result<Self, ProviderError> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| ProviderError::Request("OPENAI_API_KEY 环境变量未设置".to_string()))?;
        Self::new(api_key)
    }

    /// 设置自定义基础 URL。
    ///
    /// 适用于 OpenAI 兼容网关（OpenRouter、vLLM 等），URL 需包含版本路径
    /// （如 `https://openrouter.ai/api/v1`）。网关对 `/responses` 的兼容面
    /// 各不相同，接入前需实测。
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 返回 Responses 端点 URL。
    ///
    /// 原生路径即 `/v1/responses`，与 chat 共用前缀，**保留** base URL 中的
    /// `/v1` 后缀，不做剥离。
    fn responses_endpoint(&self) -> String {
        format!("{}/responses", self.base_url.trim_end_matches('/'))
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
///
/// 内容部件类型：assistant 消息是模型输出语义，用 `output_text`；
/// system/developer/user 用 `input_text`。
fn message_item(role: Role, content: &str) -> Value {
    let part_type = match role {
        Role::Assistant => "output_text",
        Role::System | Role::Developer | Role::User => "input_text",
    };
    serde_json::json!({
        "type": "message",
        "role": role_to_str(role),
        "content": [{ "type": part_type, "text": content }]
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

/// 构建单个 Responses `input[]` function_call_output 元素。
fn function_call_output_item(call_id: &str, output: &str) -> Value {
    serde_json::json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": output
    })
}

/// 将有序 [`InputItem`] 列表映射为 Responses `input[]` 元素。
///
/// `function_call` 与 `function_call_output` 由服务端按 `call_id` 配对，对相邻性
/// 无约束，因此映射保持历史原序一一对应，无需重排。`Reasoning` 项不回传：
/// 无 `store` 时回传思考内容需要请求 `include` 加密载体并原样传回，中立层
/// [`ContentBlock::Reasoning`] 未承载该不透明载荷 —— 丢弃并计数记录。
/// 穷尽 match 不设通配臂：中立层新增输入变体时在此处显式决定映射。
fn input_items_to_responses_values(items: &[Arc<InputItem>]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut dropped_reasoning = 0usize;
    let mut dropped_role_images = 0usize;

    for item in items {
        match &**item {
            InputItem::Message { role, content } => match role {
                // 用户消息部件数组正映射：input_text + input_image 混排。
                Role::User => out.push(user_message_item(content)),
                _ => {
                    // messages 之外的角色不承载图片，经文本视图收窄并计数。
                    dropped_role_images += content.image_count();
                    let text = content.text_view();
                    out.push(message_item(*role, &text));
                }
            },
            InputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => out.push(function_call_item(call_id, name, arguments)),
            InputItem::FunctionCallOutput { call_id, output } => {
                let text = output.text_view();
                out.push(function_call_output_item(call_id, &text));
            }
            InputItem::Reasoning { .. } => dropped_reasoning += 1,
        }
    }

    if dropped_reasoning > 0 {
        debug!(
            target: "model_provider::openai",
            dropped_reasoning_items = dropped_reasoning,
            "历史中的思考内容不回传，已丢弃"
        );
    }
    if dropped_role_images > 0 {
        warn!(
            target: "model_provider::openai",
            images = dropped_role_images,
            "system/assistant 消息中的图片部件不参与传输，已丢弃（保留文本）"
        );
    }

    out
}

/// 构建单个 Responses `input[]` user message 元素。
///
/// 纯文本 → 单 `input_text`；部件数组 → `input_text` / `input_image` 混排
/// （`image_url` 为字符串形态，`detail` 仅显式设置时发送）。
fn user_message_item(content: &Content) -> Value {
    match content {
        Content::Text(s) => message_item(Role::User, s),
        Content::Parts(parts) => {
            let content_parts: Vec<Value> = parts
                .iter()
                .map(|part| match part {
                    ContentPart::Text { text } => {
                        serde_json::json!({ "type": "input_text", "text": text })
                    }
                    ContentPart::Image { url, detail } => {
                        let mut v = serde_json::json!({ "type": "input_image", "image_url": url });
                        if let Some(detail) = detail {
                            v["detail"] = serde_json::json!(detail);
                        }
                        v
                    }
                })
                .collect();
            serde_json::json!({
                "type": "message",
                "role": "user",
                "content": content_parts
            })
        }
    }
}

/// reasoning 配置 → wire `reasoning` 对象。
///
/// effort 档位命名与 chat completions 同名（`low`/`medium`/`high`、
/// [`ReasoningEffort::Max`] → `xhigh`）；`none` 关闭档位目前仅 gpt-5.1 系列接受，
/// 档位是否被目标模型支持由服务端判定，客户端不做模型预判。请求
/// `summary: "auto"` 使思考摘要以 reasoning item 回传。启用但未指定 effort、
/// 或未配置 reasoning 时省略字段交由模型默认（省略时不产生思考摘要）。
fn reasoning_config_to_value(reasoning: Option<&ReasoningConfig>) -> Option<Value> {
    let config = reasoning?;
    if !config.enabled {
        return Some(serde_json::json!({ "effort": "none" }));
    }
    config.effort.map(|e| {
        serde_json::json!({
            "effort": match e {
                ReasoningEffort::Low => "low",
                ReasoningEffort::Medium => "medium",
                ReasoningEffort::High => "high",
                ReasoningEffort::Max => "xhigh",
            },
            "summary": "auto",
        })
    })
}

/// `tool_choice` → wire 值。
///
/// Responses 侧取值：字符串 `"auto"`，对象 `{"type":"none"}`/`{"type":"required"}`，
/// named 形式为扁平的 `{"type":"function","name":...}`（区别于 chat 的嵌套
/// `function` 包装）。
fn tool_choice_to_value(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => serde_json::json!("auto"),
        ToolChoice::None => serde_json::json!({ "type": "none" }),
        ToolChoice::Required => serde_json::json!({ "type": "required" }),
        ToolChoice::Named { name } => serde_json::json!({ "type": "function", "name": name }),
    }
}

/// [`TextConfig`] → wire `text` 对象。
///
/// `JsonSchema` 固定 `strict: true`（保证输出合规；schema 不符合结构化输出子集时
/// 请求显式报错，优于静默不合规）。`Text` 与未配置均省略整个 `text` 字段。
fn text_config_to_value(text: Option<&TextConfig>) -> Option<Value> {
    let format = text?.format.as_ref()?;
    match format {
        TextFormat::Text => None,
        TextFormat::JsonObject => Some(serde_json::json!({
            "format": { "type": "json_object" }
        })),
        TextFormat::JsonSchema { name, schema } => Some(serde_json::json!({
            "format": { "type": "json_schema", "name": name, "strict": true, "schema": schema }
        })),
    }
}

/// 构建 Responses 请求体。
fn build_responses_request_body(
    request: &GenerateRequest,
    stream: bool,
) -> Result<Vec<u8>, ProviderError> {
    let input = input_items_to_responses_values(&request.input);
    let reasoning = reasoning_config_to_value(request.reasoning.as_ref());

    let mut body = serde_json::Map::new();
    body.insert("model".into(), serde_json::json!(request.model));
    // 系统提示词走顶层 `instructions`，不混入 `input` 历史。
    if let Some(instructions) = &request.instructions {
        body.insert("instructions".into(), serde_json::json!(instructions));
    }
    body.insert("input".into(), Value::Array(input));

    // `store` 默认 true（响应上云留存）；无状态全量回放不需要服务端状态，
    // 显式关闭。用户仍可通过 `additional_params` 覆盖。
    body.insert("store".into(), serde_json::json!(false));

    if !request.tools.is_empty() {
        // Responses 工具定义是扁平的，无嵌套 `function` 包装。
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
        if let Some(choice) = &request.tool_choice {
            body.insert("tool_choice".into(), tool_choice_to_value(choice));
        }
    } else if request.tool_choice.is_some() {
        // 无工具时 tool_choice 无意义，发送可能被网关拒绝，省略。
        debug!(
            target: "model_provider::openai",
            "请求无 tools，忽略 tool_choice"
        );
    }

    // 推理 token 计入 max_output_tokens：预算过小会导致可见输出被推理挤占。
    // 仅记录提醒，不主动改写用户配置（是否调整由调用方决定）。
    if let Some(reasoning_value) = &reasoning
        && reasoning_value
            .get("effort")
            .and_then(Value::as_str)
            .is_some_and(|e| e != "none")
        && let Some(budget) = request
            .max_output_tokens
            .filter(|m| *m < OPENAI_REASONING_MIN_BUDGET)
    {
        debug!(
            target: "model_provider::openai",
            max_output_tokens = budget,
            suggested_min = OPENAI_REASONING_MIN_BUDGET,
            "推理生效时 max_output_tokens 低于建议下限，可见输出可能被推理挤占；保留原值不改写"
        );
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
    if let Some(reasoning_value) = reasoning {
        body.insert("reasoning".into(), reasoning_value);
    }
    if let Some(text) = text_config_to_value(request.text.as_ref()) {
        body.insert("text".into(), text);
    }
    if stream {
        // usage 经 `response.completed`/`response.incomplete` 事件返回，
        // `stream_options.include_usage` 是 chat completions 的语义。
        body.insert("stream".into(), serde_json::json!(true));
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
    incomplete_details: Option<ResponsesIncompleteDetails>,
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
struct ResponsesIncompleteDetails {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
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

/// 非流式响应的错误载体。
///
/// `failed` 取顶层 `error`（缺省时合成兜底消息）；`incomplete` 将截断原因放进
/// message（对齐 chat 侧 `length` 的语义）；`completed` 无错误。
fn response_error_for(
    status: ResponseStatus,
    api_response: &ResponsesResponse,
) -> Option<ResponseError> {
    match status {
        ResponseStatus::Failed => Some(
            api_response
                .error
                .as_ref()
                .map(responses_error_to_response_error)
                .unwrap_or_else(|| ResponseError {
                    code: None,
                    message: "responses API failed".to_string(),
                }),
        ),
        ResponseStatus::Incomplete => Some(ResponseError {
            code: None,
            message: api_response
                .incomplete_details
                .as_ref()
                .and_then(|d| d.reason.clone())
                .unwrap_or_else(|| "incomplete".to_string()),
        }),
        ResponseStatus::Completed => None,
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

/// 聚合 reasoning item 的文本：`summary` 优先，`content` 兜底。
///
/// OpenAI 输出的思考内容在 `summary` 数组（`summary_text`，原始思维链不暴露），
/// 必须取 summary 否则思考内容全丢；`content` 作为兜底兼容服务端可能的
/// 完整推理形态。
fn reasoning_item_text(item: &Value) -> String {
    let mut text = String::new();
    for key in ["summary", "content"] {
        if let Some(parts) = item.get(key).and_then(Value::as_array) {
            for part in parts {
                if let Some(t) = part.get("text").and_then(Value::as_str) {
                    text.push_str(t);
                }
            }
        }
        if !text.is_empty() {
            break;
        }
    }
    text
}

/// 将 Responses 输出 item 映射为中立 [`ContentBlock`]（非流式与流式 `output_item.done` 共用）。
fn response_item_to_block(item: &Value) -> Option<ContentBlock> {
    let item_type = item.get("type").and_then(Value::as_str)?;
    match item_type {
        "message" => {
            // `output_text` 为正常输出；`refusal` 为拒答文本，同样进 Text 块，
            // 避免拒答轮在下游表现为空响应。
            let mut text = String::new();
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                for part in content {
                    if matches!(
                        part.get("type").and_then(Value::as_str),
                        Some("output_text") | Some("refusal")
                    ) && let Some(t) = part.get("text").and_then(Value::as_str)
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
            let text = reasoning_item_text(item);
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
        // 内置工具调用 item（web_search_call/code_interpreter_call/mcp_call 等）：
        // 由服务端执行，中立层不承载，丢弃。数量经流式终止摘要的 unknown_item 计数观测。
        _ => None,
    }
}

/// 非流式块序归一：Reasoning → Text → ToolCall（类内相对顺序保持）。
///
/// 与 chat 适配器的产出顺序一致，下游消费方按此顺序处理；流式路径按
/// `output_index` 实时产出，不做重排。
fn reorder_blocks(blocks: Vec<ContentBlock>) -> Vec<ContentBlock> {
    let mut reasoning = Vec::new();
    let mut text = Vec::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Reasoning { .. } => reasoning.push(block),
            ContentBlock::Text { .. } => text.push(block),
            ContentBlock::ToolCall { .. } => tool_calls.push(block),
        }
    }
    reasoning.extend(text);
    reasoning.extend(tool_calls);
    reasoning
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
/// 事件 → chunk 映射（与 DeepSeek/Qwen 骨架一致）：
/// - `output_item.added` → `BlockStart`（按 item 类型）+ 函数调用 `ToolCallDelta{name}`
/// - `output_text.delta` / `refusal.delta` → `TextDelta`（拒答文本并入文本流，拒答轮不表现为空响应）
/// - `reasoning_summary_text.delta` → `ReasoningDelta`
/// - `function_call_arguments.delta` → `ToolCallDelta{arguments}`
/// - `output_item.done` → `BlockEnd`
/// - `completed`/`incomplete` → usage + `Finish`（截断只发 `incomplete`，不发 `completed`）
/// - `failed` / 顶层 `error` → 错误，中止流
///
/// Responses SSE 是具名事件协议，无 `[DONE]` 终止帧；`output_index` 唯一标识
/// 每个输出 item，直接用作 `StreamChunk` 的 index。
fn process_responses_sse_stream(
    event_source: StreamingEventSource,
    span: tracing::Span,
    model: String,
    request_id: String,
) -> GenerateStream {
    // 「开始流式处理」不再单独打点：调用方（`generate_stream`）刚打过一条请求摘要，
    // 含相同的 request_id / model / endpoint 以及更多字段，这里再打是它的严格子集。
    let stream = stream! {
        use std::collections::{HashMap, HashSet};

        let mut text_buffers: HashMap<usize, String> = HashMap::new();
        let mut reasoning_buffers: HashMap<usize, String> = HashMap::new();
        let mut tool_calls: HashMap<usize, PendingResponseToolCall> = HashMap::new();
        let mut started: HashSet<usize> = HashSet::new();
        let mut usage: Option<Usage> = None;
        let mut finish_reason: Option<FinishReason> = None;
        let mut terminated_with_error = false;

        // ── 诊断计数器 ──
        // 每 chunk 都可能命中的分支不逐条打日志，累积后在流终止摘要里一次性汇报；
        // 「未知类型」额外收集去重集合，即使同一类型重复上万次，摘要行也保持有界。
        let started_at = std::time::Instant::now();
        let mut first_chunk_at: Option<std::time::Instant> = None;
        let mut event_count: u64 = 0;
        let mut text_bytes_total: usize = 0;
        let mut reasoning_bytes_total: usize = 0;
        let mut tool_call_count: u64 = 0;
        let mut unknown_event_types: HashSet<String> = HashSet::new();
        let mut unknown_event_count: u64 = 0;
        let mut unknown_item_types: HashSet<String> = HashSet::new();
        let mut unknown_item_count: u64 = 0;
        // 缺 type / 缺 output_index / 缺 delta / 缺 item 都归为「上游发了畸形事件」，
        // 恒为 0；分成四个计数器只会让每条终止摘要行都多三个恒 0 字段。
        let mut malformed_event_count: u64 = 0;

        futures::pin_mut!(event_source);

        while let Some(event_result) = event_source.next().await {
            let data = match event_result {
                Ok(SseEvent::Open) => continue,
                Ok(SseEvent::Message(msg_event)) => msg_event.data,
                Err(provider_err) => {
                    terminated_with_error = true;
                    warn!(
                        target: "model_provider::openai",
                        request_id = %request_id,
                        model = %model,
                        error = %provider_err,
                        event_count,
                        text_bytes = text_bytes_total,
                        reasoning_bytes = reasoning_bytes_total,
                        tool_call_count,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "responses SSE 流传输错误，中止"
                    );
                    yield Err(provider_err);
                    break;
                }
            };

            event_count += 1;

            // OpenAI 不发 `[DONE]`；空帧与哨兵帧的容忍留给兼容网关。
            if data.trim().is_empty() || data.trim() == "[DONE]" {
                continue;
            }

            let event: Value = match serde_json::from_str(&data) {
                Ok(v) => v,
                Err(e) => {
                    terminated_with_error = true;
                    warn!(
                        target: "model_provider::openai",
                        request_id = %request_id,
                        model = %model,
                        error = %e,
                        event_count,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "解析 responses SSE 事件失败，中止"
                    );
                    yield Err(ProviderError::Stream(format!("解析 responses SSE 事件失败: {e}")));
                    break;
                }
            };
            let Some(event_type) = event.get("type").and_then(Value::as_str).map(str::to_string) else {
                malformed_event_count += 1;
                continue;
            };

            match event_type.as_str() {
                "response.output_item.added" => {
                    let Some(output_index) = event.get("output_index").and_then(Value::as_u64).map(|i| i as usize) else {
                        malformed_event_count += 1;
                        continue;
                    };
                    let Some(item) = event.get("item") else {
                        malformed_event_count += 1;
                        continue;
                    };
                    let Some(item_type) = item.get("type").and_then(Value::as_str) else {
                        malformed_event_count += 1;
                        continue;
                    };

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
                            tool_call_count += 1;
                            first_chunk_at.get_or_insert_with(std::time::Instant::now);
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
                        // 内置工具 item（web_search_call/code_interpreter_call 等）：
                        // 整块内容被静默丢弃 —— 累积后在终止摘要汇报。
                        other => {
                            unknown_item_count += 1;
                            // 去重集合只存一份类型名；`contains` 先查一次，避免对
                            // 每次重复出现的同类型反复分配 `String`。
                            if !unknown_item_types.contains(other) {
                                unknown_item_types.insert(other.to_string());
                            }
                        }
                    }
                }
                // `output_text.delta` 为正常文本增量；`refusal.delta` 为拒答文本增量，
                // 二者同形（output_index + delta），并入同一条文本缓冲。
                "response.output_text.delta" | "response.refusal.delta" => {
                    let Some(output_index) = event.get("output_index").and_then(Value::as_u64).map(|i| i as usize) else {
                        malformed_event_count += 1;
                        continue;
                    };
                    let Some(delta) = event.get("delta").and_then(Value::as_str) else {
                        malformed_event_count += 1;
                        continue;
                    };
                    if !delta.is_empty() {
                        text_bytes_total += delta.len();
                        first_chunk_at.get_or_insert_with(std::time::Instant::now);
                        text_buffers.entry(output_index).or_default().push_str(delta);
                        yield Ok(StreamChunk::TextDelta { index: output_index, delta: delta.to_string() });
                    }
                }
                "response.reasoning_summary_text.delta" => {
                    let Some(output_index) = event.get("output_index").and_then(Value::as_u64).map(|i| i as usize) else {
                        malformed_event_count += 1;
                        continue;
                    };
                    let Some(delta) = event.get("delta").and_then(Value::as_str) else {
                        malformed_event_count += 1;
                        continue;
                    };
                    if !delta.is_empty() {
                        reasoning_bytes_total += delta.len();
                        first_chunk_at.get_or_insert_with(std::time::Instant::now);
                        reasoning_buffers.entry(output_index).or_default().push_str(delta);
                        yield Ok(StreamChunk::ReasoningDelta { index: output_index, delta: delta.to_string() });
                    }
                }
                "response.function_call_arguments.delta" => {
                    let Some(output_index) = event.get("output_index").and_then(Value::as_u64).map(|i| i as usize) else {
                        malformed_event_count += 1;
                        continue;
                    };
                    let Some(delta) = event.get("delta").and_then(Value::as_str) else {
                        malformed_event_count += 1;
                        continue;
                    };
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
                        malformed_event_count += 1;
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
                            warn!(
                                target: "model_provider::openai",
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
                    // SSE EOF（服务端保活或重连会导致挂起直到外层超时）。截断时
                    // 服务端只发 `incomplete`，不发 `completed`。
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
                    warn!(
                        target: "model_provider::openai",
                        request_id = %request_id,
                        model = %model,
                        %body,
                        event_count,
                        text_bytes = text_bytes_total,
                        reasoning_bytes = reasoning_bytes_total,
                        tool_call_count,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "responses API 返回 failed"
                    );
                    terminated_with_error = true;
                    yield Err(ProviderError::Api { status: 500, body });
                    break;
                }
                // 顶层 `error` 事件（非 `response.failed` 形态）：同为致命错误。
                "error" => {
                    let body = event
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("responses API 返回 error 事件（无错误详情）")
                        .to_string();
                    warn!(
                        target: "model_provider::openai",
                        request_id = %request_id,
                        model = %model,
                        %body,
                        event_count,
                        text_bytes = text_bytes_total,
                        reasoning_bytes = reasoning_bytes_total,
                        tool_call_count,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "responses API 返回 error 事件"
                    );
                    terminated_with_error = true;
                    yield Err(ProviderError::Api { status: 500, body });
                    break;
                }
                // 协议规定的生命周期/心跳事件：不携带任何内容，正常流上每请求都会出现。
                // 必须显式忽略 —— 否则它们落到 `other` 臂，`unknown_event_count` 在每条
                // 健康流上都非零，「未知事件」这个告警就永远抓不到真正的异常了。
                "response.created"
                | "response.in_progress"
                | "response.queued"
                | "response.content_part.added"
                | "response.content_part.done"
                | "response.output_text.done"
                | "response.output_text.annotation.added"
                | "response.output_text.annotation.done"
                | "response.reasoning_summary_part.added"
                | "response.reasoning_summary_part.done"
                | "response.reasoning_summary_text.done"
                | "response.refusal.done"
                | "response.function_call_arguments.done" => {}
                // 内置工具与扩展能力的事件：服务端执行、无 function_call 语义，
                // 内容经 `output_item.added/done` 的 item 类型统一丢弃。
                "response.custom_tool_call_input.delta"
                | "response.custom_tool_call_input.done"
                | "response.web_search_call.in_progress"
                | "response.web_search_call.searching"
                | "response.web_search_call.completed"
                | "response.code_interpreter_call.in_progress"
                | "response.code_interpreter_call.interpreting"
                | "response.code_interpreter_call.completed"
                | "response.mcp_call.in_progress"
                | "response.mcp_call.completed"
                | "response.mcp_call_arguments.delta"
                | "response.mcp_call_arguments.done"
                | "response.file_search_call.in_progress"
                | "response.file_search_call.searching"
                | "response.file_search_call.completed" => {}
                // 未知事件类型：上游新增/改名事件时，现象是「流跑完但没内容」而
                // 日志毫无线索 —— 累积后在终止摘要汇报。
                other => {
                    unknown_event_count += 1;
                    if !unknown_event_types.contains(other) {
                        unknown_event_types.insert(other.to_string());
                    }
                }
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

        let usage = usage.unwrap_or_default();
        let reason = finish_reason.unwrap_or(FinishReason::Stop);

        debug!(
            target: "model_provider::openai",
            request_id = %request_id,
            model = %model,
            event_count,
            text_bytes = text_bytes_total,
            reasoning_bytes = reasoning_bytes_total,
            tool_call_count,
            finish_reason = %reason.as_str(),
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            total_tokens = usage.total_tokens,
            unknown_event_count,
            unknown_item_count,
            malformed_event_count,
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            ttfc_ms = first_chunk_at
                .map(|t| t.duration_since(started_at).as_millis() as u64)
                .unwrap_or(0),
            "responses SSE 流式处理结束"
        );
        // 去重后的类型集合可能较长，只在 trace 输出。
        if !unknown_event_types.is_empty() || !unknown_item_types.is_empty() {
            trace!(
                target: "model_provider::openai",
                request_id = %request_id,
                unknown_event_types = ?unknown_event_types,
                unknown_item_types = ?unknown_item_types,
                "responses 流中出现未处理的事件/item 类型"
            );
        }

        yield Ok(StreamChunk::Usage { usage });
        yield Ok(StreamChunk::Finish { reason });
    };

    GenerateStream::new_instrumented(Box::pin(stream), span)
}

// ============================================================================
// ModelProvider 实现
// ============================================================================

#[async_trait]
impl ModelProvider for OpenAiResponsesAdapter {
    fn name(&self) -> &str {
        "openai-responses"
    }

    async fn generate_full(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateResult, ProviderError> {
        let request_id = logging::next_request_id();
        let endpoint = self.responses_endpoint();
        let span = tracing::info_span!(
            "openai_responses_generate_full",
            provider = "openai-responses",
            endpoint = %endpoint,
            model = %request.model,
            request_id = %request_id,
        );

        async move {
            let started = std::time::Instant::now();
            let body = build_responses_request_body(request, false)?;
            let input = logging::summarize_input(&request.input);

            debug!(
                target: "model_provider::openai",
                request_id = %request_id,
                model = %request.model,
                endpoint = %endpoint,
                input_items = request.input.len(),
                messages = input.messages,
                function_calls = input.function_calls,
                function_call_outputs = input.function_call_outputs,
                reasoning_items = input.reasoning,
                tools = request.tools.len(),
                instructions_chars = request
                    .instructions
                    .as_deref()
                    .map(|s| s.chars().count())
                    .unwrap_or(0),
                body_bytes = body.len(),
                stream = false,
                "发送 responses 生成请求"
            );
            // 含用户对话原文，仅 trace 级别输出。`body` 随后被 move 进请求，故在此之前取。
            trace!(
                target: "model_provider::openai",
                request_id = %request_id,
                body = %logging::truncate_data_uris(&String::from_utf8_lossy(&body)),
                "responses 请求体全文"
            );

            let response = self
                .http_client
                .post(&endpoint)
                .header("Authorization", self.auth_header())
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                .await?;

            let status = response.status();
            // 必须在 `bytes()` 消费响应之前读头。
            let provider_request_id = logging::request_id_header(&response);
            let response_body = response.bytes().await?;
            let latency_ms = started.elapsed().as_millis() as u64;

            if !status.is_success() {
                let body_str = String::from_utf8_lossy(&response_body).to_string();
                warn!(
                    target: "model_provider::openai",
                    request_id = %request_id,
                    provider_request_id = provider_request_id.as_deref().unwrap_or("-"),
                    status = status.as_u16(),
                    latency_ms,
                    body = %body_str,
                    "OpenAI responses API 返回错误状态"
                );
                return Err(ProviderError::Api {
                    status: status.as_u16(),
                    body: body_str,
                });
            }

            trace!(
                target: "model_provider::openai",
                request_id = %request_id,
                body = %String::from_utf8_lossy(&response_body),
                "responses 响应体全文"
            );

            let api_response: ResponsesResponse = serde_json::from_slice(&response_body)?;
            let response_status =
                responses_status_to_response_status(api_response.status.as_deref());
            let output = reorder_blocks(
                api_response
                    .output
                    .iter()
                    .filter_map(response_item_to_block)
                    .collect(),
            );
            let error = response_error_for(response_status, &api_response);

            let usage = api_response
                .usage
                .map(responses_usage_to_usage)
                .unwrap_or_default();

            let blocks = logging::summarize_blocks(&output);
            debug!(
                target: "model_provider::openai",
                request_id = %request_id,
                provider_request_id = provider_request_id.as_deref().unwrap_or("-"),
                latency_ms,
                status = ?response_status,
                // 上游 output item 数与映射后的块数不一致，说明有 item 被 `response_item_to_block` 丢弃
                raw_items = api_response.output.len(),
                blocks = output.len(),
                text_blocks = blocks.text,
                reasoning_blocks = blocks.reasoning,
                tool_call_blocks = blocks.tool_calls,
                input_tokens = usage.input_tokens,
                output_tokens = usage.output_tokens,
                total_tokens = usage.total_tokens,
                "responses 生成完成"
            );

            Ok(GenerateResult {
                id: api_response.id,
                output,
                usage,
                status: response_status,
                error,
            })
        }
        .instrument(span)
        .await
    }

    async fn generate_stream(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateStream, ProviderError> {
        let body = build_responses_request_body(request, true)?;
        let endpoint = self.responses_endpoint();
        let model = request.model.clone();
        let request_id = logging::next_request_id();
        let input = logging::summarize_input(&request.input);

        debug!(
            target: "model_provider::openai",
            request_id = %request_id,
            model = %model,
            endpoint = %endpoint,
            input_items = request.input.len(),
            messages = input.messages,
            function_calls = input.function_calls,
            function_call_outputs = input.function_call_outputs,
            reasoning_items = input.reasoning,
            tools = request.tools.len(),
            instructions_chars = request
                .instructions
                .as_deref()
                .map(|s| s.chars().count())
                .unwrap_or(0),
            body_bytes = body.len(),
            stream = true,
            "发送 responses 流式生成请求"
        );
        trace!(
            target: "model_provider::openai",
            request_id = %request_id,
            body = %logging::truncate_data_uris(&String::from_utf8_lossy(&body)),
            "responses 流式请求体全文"
        );

        let span = tracing::info_span!(
            "openai_responses_stream",
            provider = "openai-responses",
            endpoint = %endpoint,
            model = %model,
            request_id = %request_id,
        );

        let event_source = StreamingEventSource::new(
            self.http_client.clone(),
            endpoint.clone(),
            body,
            self.auth_header(),
        );

        Ok(process_responses_sse_stream(
            event_source,
            span,
            model,
            request_id,
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
    use crate::response::ImageDetail;

    fn make_request() -> GenerateRequest {
        GenerateRequest {
            model: crate::providers::openai::chat::OPENAI_GPT5_2.to_string(),
            instructions: Some("You are a helpful assistant.".to_string()),
            input: Arc::from([Arc::new(InputItem::Message {
                role: Role::User,
                content: "你好".into(),
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

    fn weather_tool() -> crate::ToolDefinition {
        crate::ToolDefinition {
            name: "get_weather".to_string(),
            description: "获取天气".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    fn types_of(values: &[Value]) -> Vec<&str> {
        values
            .iter()
            .filter_map(|v| v.get("type").and_then(Value::as_str))
            .collect()
    }

    // ── 请求体 ──

    #[test]
    fn test_build_request_body_basic() {
        let request = make_request();
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["model"], "gpt-5.2");
        assert_eq!(json["instructions"], "You are a helpful assistant.");
        // 无状态回放：显式不上云留存（默认 true）。
        assert_eq!(json["store"], false);
        let input = json["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][0]["text"], "你好");
        // 未配置 reasoning 时省略字段，依赖模型默认。
        assert!(json.get("reasoning").is_none());
        // 非流式不携带 stream 字段。
        assert!(json.get("stream").is_none());
    }

    #[test]
    fn test_build_request_body_instructions_top_level() {
        // 顶层 instructions 只承载 GenerateRequest.instructions；历史中的 system
        // 消息仍按消息回放，不与 instructions 合并。
        let mut request = make_request();
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::System,
                content: "历史系统消息".into(),
            }),
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "你好".into(),
            }),
        ]);
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["instructions"], "You are a helpful assistant.");
        let input = json["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["role"], "system");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[1]["role"], "user");
    }

    #[test]
    fn test_build_request_body_developer_role_is_native() {
        let mut request = make_request();
        request.input = Arc::from([Arc::new(InputItem::Message {
            role: Role::Developer,
            content: "开发指令".into(),
        })]);
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["input"][0]["role"], "developer");
        assert_eq!(json["input"][0]["content"][0]["type"], "input_text");
    }

    #[test]
    fn test_build_request_body_assistant_uses_output_text() {
        // assistant 消息是模型输出语义，内容部件为 output_text；历史映射保持
        // 一一对应，相邻 assistant 文本不合并。
        let mut request = make_request();
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::Assistant,
                content: "前半".into(),
            }),
            Arc::new(InputItem::Message {
                role: Role::Assistant,
                content: "后半".into(),
            }),
        ]);
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let input = json["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["role"], "assistant");
        assert_eq!(input[0]["content"][0]["type"], "output_text");
        assert_eq!(input[0]["content"][0]["text"], "前半");
        assert_eq!(input[1]["content"][0]["text"], "后半");
    }

    #[test]
    fn test_build_request_body_drops_reasoning_items() {
        // 思考内容不回传：Reasoning 项丢弃，其余项保持原序。
        let mut request = make_request();
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "问题".into(),
            }),
            Arc::new(InputItem::Reasoning {
                content: "思考".into(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "call_1".to_string(),
                name: "get_weather".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "call_1".to_string(),
                output: "晴".into(),
            }),
            Arc::new(InputItem::Reasoning {
                content: "再思考".into(),
            }),
        ]);
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let input = json["input"].as_array().unwrap();
        assert_eq!(
            types_of(input),
            vec!["message", "function_call", "function_call_output"]
        );
        assert_eq!(input[1]["call_id"], "call_1");
        assert_eq!(input[2]["call_id"], "call_1");
    }

    #[test]
    fn test_build_request_body_stream_has_no_stream_options() {
        // `stream_options.include_usage` 是 chat completions 语义；
        // Responses 的 usage 经 `response.completed`/`incomplete` 事件返回。
        let request = make_request();
        let body = build_responses_request_body(&request, true).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["stream"], true);
        assert!(json.get("stream_options").is_none());
    }

    #[test]
    fn test_build_request_body_with_tools_flat() {
        let mut request = make_request();
        request.tools = vec![weather_tool()];
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
    fn test_tool_choice_mappings() {
        // Auto → 字符串；None/Required → 单键对象；Named → 扁平 function 形式
        // （区别于 chat 的嵌套 `function` 包装）。
        let cases = [
            (ToolChoice::Auto, serde_json::json!("auto")),
            (ToolChoice::None, serde_json::json!({ "type": "none" })),
            (
                ToolChoice::Required,
                serde_json::json!({ "type": "required" }),
            ),
            (
                ToolChoice::Named {
                    name: "get_weather".to_string(),
                },
                serde_json::json!({ "type": "function", "name": "get_weather" }),
            ),
        ];
        for (choice, expected) in cases {
            let mut request = make_request();
            request.tools = vec![weather_tool()];
            request.tool_choice = Some(choice);
            let body = build_responses_request_body(&request, false).unwrap();
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["tool_choice"], expected);
        }
    }

    #[test]
    fn test_tool_choice_without_tools_is_omitted() {
        // 无工具时 tool_choice 无意义，发送可能被网关拒绝。
        let mut request = make_request();
        request.tool_choice = Some(ToolChoice::Required);
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("tool_choice").is_none());
    }

    #[test]
    fn test_reasoning_effort_mappings() {
        // effort 档位与 chat 适配器同名（Max → xhigh）；请求 summary 使思考
        // 摘要以 reasoning item 回传。
        let cases = [
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::Max, "xhigh"),
        ];
        for (effort, expected) in cases {
            let mut request = make_request();
            request.reasoning = Some(ReasoningConfig {
                enabled: true,
                effort: Some(effort),
            });
            let body = build_responses_request_body(&request, false).unwrap();
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                json["reasoning"],
                serde_json::json!({ "effort": expected, "summary": "auto" })
            );
        }
    }

    #[test]
    fn test_reasoning_disabled_maps_to_effort_none() {
        let mut request = make_request();
        request.reasoning = Some(ReasoningConfig {
            enabled: false,
            effort: Some(ReasoningEffort::High),
        });
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        // 显式关闭不带 summary：关闭态不请求思考摘要。
        assert_eq!(json["reasoning"], serde_json::json!({ "effort": "none" }));
    }

    #[test]
    fn test_reasoning_enabled_without_effort_omits_field() {
        // 启用但未指定 effort：省略字段交由模型默认（与 deepseek/qwen 的
        // 「省略 == 默认启用」解读一致）。
        let mut request = make_request();
        request.reasoning = Some(ReasoningConfig {
            enabled: true,
            effort: None,
        });
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("reasoning").is_none());
    }

    #[test]
    fn test_build_request_body_text_format_mappings() {
        // Text 与未配置省略整个 text 字段；JsonSchema 为扁平 format 对象
        // （区别于 chat response_format 的 `json_schema` 包装键），固定 strict。
        let mut request = make_request();
        request.text = Some(TextConfig {
            format: Some(TextFormat::Text),
        });
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("text").is_none());

        request.text = Some(TextConfig {
            format: Some(TextFormat::JsonObject),
        });
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["text"]["format"]["type"], "json_object");

        request.text = Some(TextConfig {
            format: Some(TextFormat::JsonSchema {
                name: "result".to_string(),
                schema: serde_json::json!({ "type": "object" }),
            }),
        });
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["text"]["format"],
            serde_json::json!({
                "type": "json_schema",
                "name": "result",
                "strict": true,
                "schema": { "type": "object" }
            })
        );
    }

    #[test]
    fn test_build_request_body_additional_params_override() {
        // additional_params 最后合并，可覆盖显式插入的字段（如 store）。
        let mut request = make_request();
        request.additional_params = Some(serde_json::json!({
            "store": true,
            "user": "u1"
        }));
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["store"], true);
        assert_eq!(json["user"], "u1");
    }

    #[test]
    fn test_max_output_tokens_budget_hint_keeps_value() {
        // 推理生效且预算低于建议下限时仅 debug 提醒，wire 值保持原样不改写。
        let mut request = make_request();
        request.reasoning = Some(ReasoningConfig {
            enabled: true,
            effort: Some(ReasoningEffort::High),
        });
        request.max_output_tokens = Some(4096);
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["max_output_tokens"], 4096);
    }

    #[test]
    fn test_responses_endpoint() {
        let client = OpenAiResponsesAdapter::new("sk-test-key").unwrap();
        assert_eq!(
            client.responses_endpoint(),
            "https://api.openai.com/v1/responses"
        );
        let gateway = OpenAiResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url("https://openrouter.ai/api/v1/");
        assert_eq!(
            gateway.responses_endpoint(),
            "https://openrouter.ai/api/v1/responses"
        );
    }

    // ── 响应解析 ──

    #[test]
    fn test_response_output_to_blocks() {
        let items = [
            serde_json::json!({
                "type": "message",
                "content": [
                    { "type": "output_text", "text": "第一段" },
                    { "type": "output_text", "text": "第二段" }
                ]
            }),
            serde_json::json!({
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "{\"city\":\"SF\"}"
            }),
            serde_json::json!({
                "type": "function_call",
                "call_id": "call_2",
                "name": "get_weather"
            }),
            serde_json::json!({
                "type": "function_call",
                "call_id": "call_3",
                "name": "get_weather",
                "arguments": serde_json::json!({ "city": "SF" })
            }),
            serde_json::json!({ "type": "web_search_call", "status": "completed" }),
        ];
        let blocks: Vec<ContentBlock> = items.iter().filter_map(response_item_to_block).collect();
        assert_eq!(blocks.len(), 4);
        assert_eq!(
            blocks[0],
            ContentBlock::Text {
                text: "第一段第二段".to_string()
            }
        );
        assert!(matches!(
            &blocks[1],
            ContentBlock::ToolCall { call_id, name, arguments }
                if call_id == "call_1" && name == "get_weather" && arguments == "{\"city\":\"SF\"}"
        ));
        // arguments 缺失回退 "{}"。
        assert!(matches!(
            &blocks[2],
            ContentBlock::ToolCall { arguments, .. } if arguments == "{}"
        ));
        // arguments 为对象时序列化为 JSON 字符串。
        assert!(matches!(
            &blocks[3],
            ContentBlock::ToolCall { arguments, .. } if arguments == "{\"city\":\"SF\"}"
        ));
    }

    #[test]
    fn test_response_message_includes_refusal_parts() {
        // refusal 部件与 output_text 一样进 Text 块，拒答内容对下游可见。
        let mixed = serde_json::json!({
            "type": "message",
            "content": [
                { "type": "output_text", "text": "a" },
                { "type": "refusal", "text": "b" }
            ]
        });
        assert_eq!(
            response_item_to_block(&mixed),
            Some(ContentBlock::Text {
                text: "ab".to_string()
            })
        );

        let refusal_only = serde_json::json!({
            "type": "message",
            "content": [{ "type": "refusal", "text": "无法协助" }]
        });
        assert_eq!(
            response_item_to_block(&refusal_only),
            Some(ContentBlock::Text {
                text: "无法协助".to_string()
            })
        );
    }

    #[test]
    fn test_reasoning_summary_is_kept() {
        // summary（summary_text）是 OpenAI 思考内容的主形态。
        let summary = serde_json::json!({
            "type": "reasoning",
            "summary": [
                { "type": "summary_text", "text": "第一段" },
                { "type": "summary_text", "text": "第二段" }
            ]
        });
        assert_eq!(
            response_item_to_block(&summary),
            Some(ContentBlock::Reasoning {
                text: "第一段第二段".to_string()
            })
        );

        // summary 缺失时以 content 兜底；两者皆空丢弃。
        let content_fallback = serde_json::json!({
            "type": "reasoning",
            "content": [{ "type": "reasoning_text", "text": "完整推理" }]
        });
        assert_eq!(
            response_item_to_block(&content_fallback),
            Some(ContentBlock::Reasoning {
                text: "完整推理".to_string()
            })
        );

        let empty = serde_json::json!({ "type": "reasoning", "summary": [] });
        assert_eq!(response_item_to_block(&empty), None);
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
        // 缺失/未知状态按正常完成处理。
        assert_eq!(
            responses_status_to_response_status(None),
            ResponseStatus::Completed
        );
        assert_eq!(
            responses_status_to_response_status(Some("queued")),
            ResponseStatus::Completed
        );
    }

    #[test]
    fn test_incomplete_status_sets_error_message() {
        // 截断原因放进 error.message（对齐 chat 侧 `length` 的语义）。
        let resp: ResponsesResponse = serde_json::from_value(serde_json::json!({
            "id": "resp_1",
            "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" }
        }))
        .unwrap();
        let error = response_error_for(ResponseStatus::Incomplete, &resp).unwrap();
        assert_eq!(error.message, "max_output_tokens");

        // 缺 incomplete_details 时给兜底文案。
        let bare: ResponsesResponse =
            serde_json::from_value(serde_json::json!({ "id": "resp_2", "status": "incomplete" }))
                .unwrap();
        let error = response_error_for(ResponseStatus::Incomplete, &bare).unwrap();
        assert_eq!(error.message, "incomplete");
    }

    #[test]
    fn test_failed_status_sets_error() {
        let resp: ResponsesResponse = serde_json::from_value(serde_json::json!({
            "id": "resp_1",
            "status": "failed",
            "error": { "code": "server_error", "message": "内部错误" }
        }))
        .unwrap();
        let error = response_error_for(ResponseStatus::Failed, &resp).unwrap();
        assert_eq!(error.code.as_deref(), Some("server_error"));
        assert_eq!(error.message, "内部错误");

        // 无 error 载体时合成兜底消息。
        let bare: ResponsesResponse =
            serde_json::from_value(serde_json::json!({ "id": "resp_2", "status": "failed" }))
                .unwrap();
        let error = response_error_for(ResponseStatus::Failed, &bare).unwrap();
        assert!(error.message.contains("responses API failed"));

        assert!(response_error_for(ResponseStatus::Completed, &resp).is_none());
    }

    #[test]
    fn test_non_stream_block_order_normalized() {
        // 非流式块序归一为 Reasoning → Text → ToolCall；类内相对顺序保持。
        let items = [
            serde_json::json!({
                "type": "message",
                "content": [{ "type": "output_text", "text": "答案" }]
            }),
            serde_json::json!({
                "type": "reasoning",
                "summary": [{ "type": "summary_text", "text": "思考" }]
            }),
            serde_json::json!({
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "{}"
            }),
        ];
        let blocks: Vec<ContentBlock> = items.iter().filter_map(response_item_to_block).collect();
        let ordered = reorder_blocks(blocks);
        assert!(matches!(
            &ordered[..],
            [
                ContentBlock::Reasoning { text: r },
                ContentBlock::Text { text: t },
                ContentBlock::ToolCall { .. },
            ] if r == "思考" && t == "答案"
        ));
    }

    #[test]
    fn test_usage_partial_fields_default_to_zero() {
        // usage 字段缺失按 0 处理，不使整个响应解析失败。
        let resp: ResponsesResponse = serde_json::from_value(serde_json::json!({
            "id": "resp_1",
            "status": "completed",
            "usage": { "input_tokens": 5 }
        }))
        .unwrap();
        let usage = responses_usage_to_usage(resp.usage.unwrap());
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn test_name_reports_openai_responses() {
        let client = OpenAiResponsesAdapter::new("sk-test-key").unwrap();
        assert_eq!(client.name(), "openai-responses");
    }

    // ── 流式端到端（本地一次性 SSE 服务，与 qwen/responses.rs 测试同模式） ──

    /// 将 SSE data 载荷列表拼成请求体可发送的 SSE 文本。
    fn sse_events(datas: &[&str]) -> String {
        datas
            .iter()
            .map(|d| format!("data: {d}\n\n"))
            .collect::<String>()
    }

    /// 启动一个返回固定 SSE 响应的一次性本地 HTTP 服务，返回其 base URL。
    async fn spawn_sse_server(sse_body: String) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            use tokio::io::AsyncWriteExt;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse_body.len(),
                sse_body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
        format!("http://{addr}")
    }

    /// 消费完整的流，返回全部 chunk（含错误）。
    async fn collect_stream(
        client: &OpenAiResponsesAdapter,
        request: &GenerateRequest,
    ) -> Vec<Result<StreamChunk, ProviderError>> {
        let mut stream = client.generate_stream(request).await.unwrap();
        let mut chunks = Vec::new();
        while let Some(chunk) = stream.next().await {
            chunks.push(chunk);
        }
        chunks
    }

    /// 全事件链：思考摘要 → 文本，生命周期事件（`response.created`、
    /// `*.done`）被忽略，`output_item.done` 产出完整块，`completed` 收敛。
    #[tokio::test]
    async fn test_stream_text_and_reasoning_assembly() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.created","response":{"id":"r1"}}"#,
            r#"{"type":"response.in_progress","response":{}}"#,
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning"}}"#,
            r#"{"type":"response.reasoning_summary_part.added","summary_index":0}"#,
            r#"{"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"先想"}"#,
            r#"{"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"一下"}"#,
            r#"{"type":"response.reasoning_summary_text.done","output_index":0}"#,
            r#"{"type":"response.reasoning_summary_part.done","summary_index":0}"#,
            r#"{"type":"response.output_item.added","output_index":1,"item":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"response.content_part.added","output_index":1,"part_index":0}"#,
            r#"{"type":"response.output_text.delta","output_index":1,"delta":"你好"}"#,
            r#"{"type":"response.output_text.done","output_index":1,"text":"你好"}"#,
            r#"{"type":"response.content_part.done","output_index":1,"part_index":0}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","summary":[{"type":"summary_text","text":"先想一下"}]}}"#,
            r#"{"type":"response.output_item.done","output_index":1,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"你好"}]}}"#,
            r#"{"type":"response.completed","response":{"id":"r1","usage":{"input_tokens":6,"output_tokens":4,"total_tokens":10}}}"#,
        ]))
        .await;
        let client = OpenAiResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(matches!(
            &chunks[..],
            [
                Ok(StreamChunk::BlockStart { index: 0, block_type: BlockType::Reasoning }),
                Ok(StreamChunk::ReasoningDelta { index: 0, delta }),
                Ok(StreamChunk::ReasoningDelta { index: 0, .. }),
                Ok(StreamChunk::BlockStart { index: 1, block_type: BlockType::Text }),
                Ok(StreamChunk::TextDelta { index: 1, .. }),
                Ok(StreamChunk::BlockEnd { index: 0, block: ContentBlock::Reasoning { text } }),
                Ok(StreamChunk::BlockEnd { index: 1, block: ContentBlock::Text { text: final_text } }),
                Ok(StreamChunk::Usage { usage }),
                Ok(StreamChunk::Finish { reason: FinishReason::Stop }),
            ]
            if delta == "先想"
                && text == "先想一下"
                && final_text == "你好"
                && *usage == (Usage {
                    input_tokens: 6,
                    output_tokens: 4,
                    total_tokens: 10,
                })
        ));
        assert_eq!(chunks.len(), 9);
    }

    #[tokio::test]
    async fn test_stream_tool_call_assembly() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call_1","name":"get_weather","arguments":""}}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"city\":"}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"\"SF\"}"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"call_1","name":"get_weather","arguments":"{\"city\":\"SF\"}"}}"#,
            r#"{"type":"response.completed","response":{"id":"r2","usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}"#,
        ]))
        .await;
        let client = OpenAiResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(matches!(
            &chunks[..],
            [
                Ok(StreamChunk::BlockStart {
                    block_type: BlockType::ToolCall,
                    ..
                }),
                Ok(StreamChunk::ToolCallDelta { .. }),
                Ok(StreamChunk::ToolCallDelta { .. }),
                Ok(StreamChunk::ToolCallDelta { .. }),
                Ok(StreamChunk::BlockEnd {
                    block: ContentBlock::ToolCall { .. },
                    ..
                }),
                Ok(StreamChunk::Usage { .. }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop
                }),
            ]
        ));
        // 元素级断言（模式内 guard 是实验特性，与 chat.rs 测试同做法拆开断言）。
        assert!(matches!(
            chunks[1],
            Ok(StreamChunk::ToolCallDelta { name: Some(ref n), .. }) if n == "get_weather"
        ));
        assert!(matches!(
            chunks[2],
            Ok(StreamChunk::ToolCallDelta { name: None, ref arguments, .. })
                if arguments == &Value::String("{\"city\":".to_string())
        ));
        assert!(matches!(
            chunks[3],
            Ok(StreamChunk::ToolCallDelta { name: None, ref arguments, .. })
                if arguments == &Value::String("\"SF\"}".to_string())
        ));
        assert!(matches!(
            chunks[4],
            Ok(StreamChunk::BlockEnd {
                block: ContentBlock::ToolCall { ref call_id, ref name, ref arguments },
                ..
            }) if call_id == "call_1"
                && name == "get_weather"
                && arguments == r#"{"city":"SF"}"#
        ));
        assert!(matches!(
            &chunks[5],
            Ok(StreamChunk::Usage { usage })
                if *usage == (Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    total_tokens: 15,
                })
        ));
        assert_eq!(chunks.len(), 7);
    }

    /// `response.incomplete` → `Finish::MaxTokens`；截断时服务端不发
    /// `response.completed`，未 `output_item.done` 的缓冲由安全网兜底合成 BlockEnd。
    #[tokio::test]
    async fn test_stream_incomplete_maps_to_max_tokens_with_safety_net_flush() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"部分"}"#,
            r#"{"type":"response.incomplete","response":{"id":"r3","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":5,"output_tokens":16,"total_tokens":21}}}"#,
        ]))
        .await;
        let client = OpenAiResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(matches!(
            &chunks[..],
            [
                Ok(StreamChunk::BlockStart { block_type: BlockType::Text, .. }),
                Ok(StreamChunk::TextDelta { .. }),
                Ok(StreamChunk::BlockEnd { block: ContentBlock::Text { text }, .. }),
                Ok(StreamChunk::Usage { usage }),
                Ok(StreamChunk::Finish { reason: FinishReason::MaxTokens }),
            ] if text == "部分"
                && *usage == (Usage {
                    input_tokens: 5,
                    output_tokens: 16,
                    total_tokens: 21,
                })
        ));
    }

    /// 内容过滤导致的 incomplete → `Finish::Error`，而非 MaxTokens。
    #[tokio::test]
    async fn test_stream_incomplete_content_filter_maps_to_error() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"部分"}"#,
            r#"{"type":"response.incomplete","response":{"id":"r4","incomplete_details":{"reason":"content_filter"},"usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7}}}"#,
        ]))
        .await;
        let client = OpenAiResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(matches!(
            chunks.last(),
            Some(Ok(StreamChunk::Finish {
                reason: FinishReason::Error
            }))
        ));
    }

    /// OpenAI 全套生命周期事件不产生任何 chunk，也不影响内容块组装。
    #[tokio::test]
    async fn test_stream_lifecycle_events_are_ignored() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.queued","response":{}}"#,
            r#"{"type":"response.created","response":{}}"#,
            r#"{"type":"response.in_progress","response":{}}"#,
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"response.content_part.added","output_index":0,"part_index":0}"#,
            r#"{"type":"response.output_text.annotation.added","output_index":0,"annotation_index":0}"#,
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"done"}"#,
            r#"{"type":"response.output_text.annotation.done","output_index":0,"annotation_index":0}"#,
            r#"{"type":"response.output_text.done","output_index":0,"text":"done"}"#,
            r#"{"type":"response.content_part.done","output_index":0,"part_index":0}"#,
            r#"{"type":"response.refusal.done","output_index":0}"#,
            r#"{"type":"response.function_call_arguments.done","output_index":1}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#,
            r#"{"type":"response.completed","response":{"id":"r5","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#,
        ]))
        .await;
        let client = OpenAiResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(matches!(
            &chunks[..],
            [
                Ok(StreamChunk::BlockStart {
                    index: 0,
                    block_type: BlockType::Text
                }),
                Ok(StreamChunk::TextDelta { index: 0, .. }),
                Ok(StreamChunk::BlockEnd { index: 0, .. }),
                Ok(StreamChunk::Usage { .. }),
                Ok(StreamChunk::Finish { .. }),
            ]
        ));
        assert_eq!(chunks.len(), 5);
    }

    /// 拒答以 `response.refusal.delta` 增量流出且无 output_text：
    /// 并入文本流产出 TextDelta，拒答内容照常可见。
    #[tokio::test]
    async fn test_stream_refusal_delta_yields_text_delta() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"response.refusal.delta","output_index":0,"delta":"无法"}"#,
            r#"{"type":"response.refusal.delta","output_index":0,"delta":"协助"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","content":[{"type":"refusal","text":"无法协助"}]}}"#,
            r#"{"type":"response.completed","response":{"id":"r6","usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}}"#,
        ]))
        .await;
        let client = OpenAiResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(matches!(
            &chunks[..],
            [
                Ok(StreamChunk::BlockStart { block_type: BlockType::Text, .. }),
                Ok(StreamChunk::TextDelta { delta, .. }),
                Ok(StreamChunk::TextDelta { .. }),
                Ok(StreamChunk::BlockEnd { block: ContentBlock::Text { text }, .. }),
                Ok(StreamChunk::Usage { .. }),
                Ok(StreamChunk::Finish { reason: FinishReason::Stop }),
            ] if delta == "无法" && text == "无法协助"
        ));
    }

    /// `response.failed` 为致命事件：产出错误且不再有正常收尾。
    #[tokio::test]
    async fn test_stream_failed_event_aborts_with_api_error() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.created","response":{}}"#,
            r#"{"type":"response.failed","response":{"error":{"code":"ServerError","message":"内部错误"}}}"#,
        ]))
        .await;
        let client = OpenAiResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(matches!(
            chunks.last(),
            Some(Err(ProviderError::Api { status: 500, .. }))
        ));
        assert!(
            !chunks
                .iter()
                .any(|c| matches!(c, Ok(StreamChunk::Finish { .. })))
        );
    }

    /// 顶层 `error` 事件（非 `response.failed` 形态）同样致命。
    #[tokio::test]
    async fn test_stream_error_event_aborts_with_api_error() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"error","code":"server_error","message":"上游过载"}"#,
        ]))
        .await;
        let client = OpenAiResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(matches!(
            chunks.last(),
            Some(Err(ProviderError::Api { status: 500, body }))
                if body.contains("上游过载")
        ));
        assert!(
            !chunks
                .iter()
                .any(|c| matches!(c, Ok(StreamChunk::Finish { .. })))
        );
    }

    /// 未知事件与畸形事件（缺 type / 缺 output_index）被跳过计数，
    /// 不中断后续内容组装。
    #[tokio::test]
    async fn test_stream_unknown_and_malformed_events_are_skipped() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.future_thing.added","output_index":9}"#,
            r#"{"output_index":0}"#,
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"好"}"#,
            r#"{"type":"response.output_text.delta","delta":"无定位"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"好"}]}}"#,
            r#"{"type":"response.completed","response":{"id":"r7","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#,
        ]))
        .await;
        let client = OpenAiResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(matches!(
            &chunks[..],
            [
                Ok(StreamChunk::BlockStart {
                    block_type: BlockType::Text,
                    ..
                }),
                Ok(StreamChunk::TextDelta { .. }),
                Ok(StreamChunk::BlockEnd { .. }),
                Ok(StreamChunk::Usage { .. }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop
                }),
            ]
        ));
    }

    /// 无 `response.completed`/`incomplete`（EOF 即断开）：缓冲兜底合成 BlockEnd，
    /// Finish 缺省 Stop、Usage 缺省 0 —— 不挂起、不丢已收内容。
    #[tokio::test]
    async fn test_stream_eof_without_terminal_event_uses_safety_net() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"你好"}"#,
        ]))
        .await;
        let client = OpenAiResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(matches!(
            &chunks[..],
            [
                Ok(StreamChunk::BlockStart { block_type: BlockType::Text, .. }),
                Ok(StreamChunk::TextDelta { .. }),
                Ok(StreamChunk::BlockEnd { block: ContentBlock::Text { text }, .. }),
                Ok(StreamChunk::Usage { usage }),
                Ok(StreamChunk::Finish { reason: FinishReason::Stop }),
            ] if text == "你好"
                && *usage == Usage::default()
        ));
    }

    /// OpenAI 不发 `[DONE]`，但对兼容网关发来的哨兵帧保持容忍。
    #[tokio::test]
    async fn test_stream_ignores_done_sentinel() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"你好"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"你好"}]}}"#,
            r#"{"type":"response.completed","response":{"id":"r8","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#,
            "[DONE]",
        ]))
        .await;
        let client = OpenAiResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(matches!(
            chunks.last(),
            Some(Ok(StreamChunk::Finish {
                reason: FinishReason::Stop
            }))
        ));
    }

    #[test]
    fn test_openai_responses_user_image_parts_body() {
        // 用户消息部件数组 → input_text + input_image 混排；
        // image_url 为字符串形态，detail 仅显式设置时发送。
        let request = GenerateRequest {
            input: Arc::from([Arc::new(InputItem::Message {
                role: Role::User,
                content: Content::Parts(vec![
                    ContentPart::Text {
                        text: "这是什么图".to_string(),
                    },
                    ContentPart::Image {
                        url: "https://example.com/cat.png".to_string(),
                        detail: Some(ImageDetail::High),
                    },
                    ContentPart::Image {
                        url: "data:image/png;base64,AAAA".to_string(),
                        detail: None,
                    },
                ]),
            })]),
            ..make_request()
        };
        let body = build_responses_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["input"][0],
            serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "这是什么图"},
                    {"type": "input_image", "image_url": "https://example.com/cat.png", "detail": "high"},
                    {"type": "input_image", "image_url": "data:image/png;base64,AAAA"},
                ]
            })
        );
    }
}
