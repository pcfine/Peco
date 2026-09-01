//! Qwen Responses API 适配器。
//!
//! 提供 [`QwenResponsesAdapter`]，为阿里云百炼 OpenAI 兼容 `/responses` 端点实现
//! [`ModelProvider`]。请求/响应直通映射到中立词汇表
//! （[`GenerateRequest`]/[`GenerateResult`]/[`StreamChunk`]）。
//!
//! 与 [`DeepSeekResponsesAdapter`](crate::DeepSeekResponsesAdapter) 的通用骨架同构，
//! 差异点：
//!
//! 1. 端点拼接**保留** `/v1` 前缀（responses 挂在 `compatible-mode/v1` 下）；
//! 2. reasoning 输出为 `summary`（摘要），输入回传按原形态重建；
//! 3. `function_call_output` 必须紧跟对应的 `function_call` —— 逐对排布，
//!    而非 DeepSeek 的整组排布；
//! 4. `tool_choice` 无 `named` 形式，用 `allowed_tools` 对象忠实映射；
//! 5. `store` 默认 true，显式置 `false`（无状态全量回放，对话不上云留存）。

use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use tracing::{Instrument, debug, trace, warn};

use crate::logging;
use crate::response::{
    BlockType, ContentBlock, FinishReason, GenerateRequest, GenerateResult, InputItem,
    ReasoningConfig, ReasoningEffort, ResponseError, ResponseStatus, Role, StreamChunk, ToolChoice,
};
use crate::streaming::pipeline::normalize_tool_call_arguments;
use crate::streaming::sse::{SseEvent, StreamingEventSource};
use crate::{GenerateStream, ModelProvider, ProviderError, Usage};

/// DashScope OpenAI 兼容模式的基础 URL（chat 与 responses 共用该前缀）。
const QWEN_API_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";

/// Responses `max_output_tokens` 的下限（低于该值网关返回 400）。
const QWEN_MIN_OUTPUT_TOKENS: u32 = 16;

// ============================================================================
// 客户端
// ============================================================================

/// Qwen Responses API 客户端，实现 [`ModelProvider`]（`generate_full`/`generate_stream`）。
///
/// 端点：`{base}/responses`。
pub struct QwenResponsesAdapter {
    http_client: reqwest::Client,
    api_key: String,
    base_url: String,
    /// 严格特性校验：`false` 时静默降级（`debug!`/`warn!`）responses 适配器无法
    /// 忠实承载的特性（`text.format`、多工具下的 `tool_choice: required`），
    /// `true` 时返回 [`ProviderError::Request`]。
    strict_feature_validation: bool,
}

impl QwenResponsesAdapter {
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
            base_url: QWEN_API_BASE_URL.to_string(),
            strict_feature_validation: false,
        })
    }

    /// 通过读取 `DASHSCOPE_API_KEY` 环境变量创建。
    pub fn from_env() -> Result<Self, ProviderError> {
        let api_key = std::env::var("DASHSCOPE_API_KEY")
            .map_err(|_| ProviderError::Request("DASHSCOPE_API_KEY 环境变量未设置".to_string()))?;
        Self::new(api_key)
    }

    /// 设置自定义基础 URL（地域专属域名或代理端点）。
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 设置严格特性校验开关（默认 `false`）。
    pub fn strict_feature_validation(mut self, strict: bool) -> Self {
        self.strict_feature_validation = strict;
        self
    }

    /// 返回 Responses 端点 URL。
    ///
    /// 与 DeepSeek 不同：百炼的 responses 挂在 `compatible-mode/v1` 下，与 chat 共用
    /// 前缀，因此**保留**存量配置中的 `/v1` 后缀，不做剥离。
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

/// 构建单个 Responses `input[]` reasoning 元素。
///
/// 百炼输出的 reasoning item 是 `summary` 数组（摘要形态），官方说明 reasoning 项
/// 可原样传回 input —— 因此按**收到的形态**重建为 `summary`，而不是 DeepSeek 的
/// `content`/`reasoning_text` 形态（DeepSeek 形态是否被接受未证实，以实测为准）。
fn reasoning_item(content: &str) -> Value {
    serde_json::json!({
        "type": "reasoning",
        "summary": [{ "type": "summary_text", "text": content }]
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

/// 冲刷缓存的工具调用对：逐对输出 `function_call` 紧跟其 `function_call_output`。
///
/// 百炼官方约束：`function_call_output`「必须紧跟对应的 function_call，否则报错」
/// （与 DeepSeek 的整组排布方向相反 —— DeepSeek 禁止逐对交错，百炼要求逐对相邻）。
/// 并行调用下 output 的**到达顺序**可能乱序，这里按 call 的出现顺序为每个 call
/// 取回其 output 重排，保证逐对相邻；两组到达顺序下均产出
/// `call_a, out_a, call_b, out_b`。
///
/// 没有配对 output 的 call 必须丢弃 —— 留着会让整个请求报错，这类残缺项只出现在
/// 轮次被中断的异常历史里，故用 warn。反之，没有配对 call 的 output 同样丢弃。
fn flush_call_pairs(
    out: &mut Vec<Value>,
    calls: &mut Vec<(String, Value)>,
    outputs: &mut Vec<(String, Value)>,
) {
    if calls.is_empty() {
        if !outputs.is_empty() {
            let orphans: Vec<String> = outputs.drain(..).map(|(id, _)| id).collect();
            warn!(
                target: "model_provider::qwen_responses",
                orphan_output_call_ids = ?orphans,
                count = orphans.len(),
                "丢弃无配对 call 的 function_call_output"
            );
        }
        return;
    }

    let total_calls = calls.len();
    let mut dropped_call_ids: Vec<String> = Vec::new();
    for (id, call_value) in calls.drain(..) {
        match outputs.iter().position(|(oid, _)| oid == &id) {
            Some(pos) => {
                let (_, output_value) = outputs.remove(pos);
                out.push(call_value);
                out.push(output_value);
            }
            None => dropped_call_ids.push(id),
        }
    }
    let orphan_output_ids: Vec<String> = outputs.drain(..).map(|(id, _)| id).collect();
    if !dropped_call_ids.is_empty() || !orphan_output_ids.is_empty() {
        warn!(
            target: "model_provider::qwen_responses",
            dropped_call_ids = ?dropped_call_ids,
            orphan_output_call_ids = ?orphan_output_ids,
            total_calls,
            "丢弃无配对 output 的 function_call（工具调用被从回放历史中抹除）"
        );
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
/// 与 [`DeepSeekResponsesAdapter`](crate::DeepSeekResponsesAdapter) 的编码器对称，
/// 但排布遵循百炼的逐对约束：
///
/// 1. **逐对相邻**：每个 `function_call` 之后立即输出其 `function_call_output`。
///    并行调用的 output 乱序到达不影响结果 —— 冲刷时按 call 顺序重排配对。
/// 2. 新 call 开启配对序列时，先冲刷此前暂存的 assistant 文本（保持自然顺序：
///    文本在前，call 在后）；配对序列开启**期间**到达的文本推迟到冲刷之后输出，
///    避免打断 call 与 output 的相邻性。
/// 3. reasoning 到达时先收束未完成的配对序列，再输出 reasoning，最后冲刷暂存
///    文本 —— reasoning 保持 assistant span 的第一个元素（与 DeepSeek 的归位策略
///    一致；百炼对该位置无文档约束，取保守排布对两种落库顺序都成立）。
///
/// 相邻 assistant 文本会合并，避免同一轮被拆成多个 message 元素。
/// `Reasoning` 是否回传由 `carry_reasoning` 决定，判据见 [`should_carry_reasoning`]。
fn input_items_to_responses_values(items: &[Arc<InputItem>], carry_reasoning: bool) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();

    // 当前配对序列：按出现顺序缓存的 function_call 与等待配对的 output。
    let mut calls: Vec<(String, Value)> = Vec::new();
    let mut outputs: Vec<(String, Value)> = Vec::new();
    // 暂存的 assistant 文本；配对序列开启期间到达的文本推迟到冲刷之后再输出。
    let mut pending_text: Option<String> = None;

    for item in items {
        match &**item {
            InputItem::Reasoning { content } => {
                // 不回传时直接丢弃：这类 reasoning 会被 API 忽略，留着只占上下文。
                if carry_reasoning {
                    // reasoning 标志着新一轮迭代开始，先收束上一组。
                    flush_call_pairs(&mut out, &mut calls, &mut outputs);
                    // reasoning 排在暂存文本**之前**：先 push reasoning 再冲刷文本，
                    // 把 assistant 文本归位到 reasoning 之后。
                    out.push(reasoning_item(content));
                    flush_text(&mut out, &mut pending_text);
                }
            }
            InputItem::Message { role, content } => match role {
                Role::System | Role::Developer | Role::User => {
                    flush_call_pairs(&mut out, &mut calls, &mut outputs);
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
                // 配对序列尚未开启时，文本保持在 call 之前的自然位置。
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

    flush_call_pairs(&mut out, &mut calls, &mut outputs);
    flush_text(&mut out, &mut pending_text);

    out
}

/// 判定历史中的 `Reasoning` 是否需要回传。
///
/// 百炼不强制回传 reasoning（官方示例只回传 call 对），但「有则回传」有利于多轮
/// 思考连贯且无报错风险。沿用与 DeepSeek 相同的判据：携带 tools（本轮可能产生
/// 新的工具调用）**或**历史含工具调用（需成对回放）时回传；reasoning 显式关闭时
/// 一律不回传，避免与关闭态自相矛盾。
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

/// reasoning 是否启用：未显式配置（`None`）按 provider 默认启用；否则看 `enabled`。
///
/// 与 [`reasoning_config_to_value`] 共用同一约定 —— 该函数在「启用但未指定 effort」时
/// 省略 `reasoning` 字段、交由 provider 默认，即「省略 == 启用」。两处解读必须同步，
/// 否则回传判定会与请求体实际声明的推理开关脱节。
fn reasoning_enabled(request: &GenerateRequest) -> bool {
    request.reasoning.as_ref().is_none_or(|r| r.enabled)
}

/// reasoning 配置 → wire `reasoning` 对象。
///
/// 百炼 effort 为 7 档（`none/minimal/low/medium/high/xhigh/max`），中立层只表达
/// 4 档；`xhigh`/`max` 仅华北2（北京）与新加坡地域支持，越界由服务端报错，客户端
/// 不做地域预判。未显式配置（`None`）时省略字段依赖服务端默认（官方文档对默认档位
/// 表述不一致，以实测为准）。
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

/// `tool_choice` → wire 值。
///
/// 百炼取值为字符串 `"auto"`/`"none"`/`"required"`，其中 `required` **仅单工具时
/// 可用**；无 OpenAI 的 `{"type":"function","name":...}` named 形式，用官方的
/// `allowed_tools` 对象忠实映射。
///
/// `strict` 为 `true` 时，无法忠实表达的组合（多工具 required）直接报错而不是降级。
fn tool_choice_to_value(
    choice: &ToolChoice,
    tool_count: usize,
    strict: bool,
) -> Result<Value, ProviderError> {
    let value = match choice {
        ToolChoice::Auto => serde_json::json!("auto"),
        ToolChoice::None => serde_json::json!("none"),
        ToolChoice::Required => {
            if tool_count == 1 {
                serde_json::json!("required")
            } else if strict {
                return Err(ProviderError::Request(format!(
                    "tool_choice='required' 仅支持单工具，当前 {tool_count} 个工具"
                )));
            } else {
                warn!(
                    target: "model_provider::qwen_responses",
                    tool_count,
                    "tool_choice='required' 仅支持单工具，降级为 'auto'"
                );
                serde_json::json!("auto")
            }
        }
        ToolChoice::Named { name } => serde_json::json!({
            "type": "allowed_tools",
            "mode": "required",
            "tools": [{ "type": "function", "name": name }]
        }),
    };
    Ok(value)
}

/// 构建 Responses 请求体。
fn build_responses_request_body(
    request: &GenerateRequest,
    stream: bool,
    strict: bool,
) -> Result<Vec<u8>, ProviderError> {
    let carry_reasoning = should_carry_reasoning(
        &request.input,
        !request.tools.is_empty(),
        reasoning_enabled(request),
    );
    let input = input_items_to_responses_values(&request.input, carry_reasoning);

    let mut body = serde_json::Map::new();
    body.insert("model".into(), serde_json::json!(request.model));
    if let Some(instructions) = &request.instructions {
        body.insert("instructions".into(), serde_json::json!(instructions));
    }
    body.insert("input".into(), Value::Array(input));

    // `store` 默认 true（响应上云留存 7 天）；无状态全量回放不需要服务端状态，
    // 显式关闭。用户仍可通过 `additional_params` 覆盖。
    body.insert("store".into(), serde_json::json!(false));

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
        if let Some(choice) = &request.tool_choice {
            body.insert(
                "tool_choice".into(),
                tool_choice_to_value(choice, request.tools.len(), strict)?,
            );
        }
    } else if request.tool_choice.is_some() {
        // 无工具时 tool_choice 无意义，发送可能被网关拒绝，省略。
        debug!(
            target: "model_provider::qwen_responses",
            "请求无 tools，忽略 tool_choice"
        );
    }

    if let Some(t) = request.temperature {
        body.insert("temperature".into(), serde_json::json!(t));
    }
    if let Some(p) = request.top_p {
        body.insert("top_p".into(), serde_json::json!(p));
    }
    if let Some(m) = request.max_output_tokens {
        // 官方下限 16，低于该值网关 400；这里夹取到下限并记录，避免整轮失败。
        if m < QWEN_MIN_OUTPUT_TOKENS {
            debug!(
                target: "model_provider::qwen_responses",
                requested = m,
                clamped = QWEN_MIN_OUTPUT_TOKENS,
                "max_output_tokens 低于官方下限 16，已夹取"
            );
        }
        body.insert(
            "max_output_tokens".into(),
            serde_json::json!(m.max(QWEN_MIN_OUTPUT_TOKENS)),
        );
    }
    if let Some(r) = reasoning_config_to_value(request.reasoning.as_ref()) {
        body.insert("reasoning".into(), r);
    }
    // `text.format` 在百炼 Responses 侧的支持性未证实（官方参数表未列出），
    // 省略并记录，待实测校准后改为直通。
    if request.text.is_some() {
        if strict {
            return Err(ProviderError::Request(
                "text.format 在 Qwen Responses 端点的支持性未证实，严格模式拒绝发送".to_string(),
            ));
        }
        debug!(
            target: "model_provider::qwen_responses",
            "text.format 在 Qwen Responses 端点的支持性未证实，已省略"
        );
    }
    if stream {
        body.insert("stream".into(), serde_json::json!(true));
        // 不传 `stream_options`：usage 经 `response.completed` 事件返回，
        // `stream_options.include_usage` 是 chat completions 的语义。
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

/// 聚合 reasoning item 的文本：`summary` 优先，`content` 兜底。
///
/// 百炼输出的思考内容在 `summary` 数组（`summary_text`）—— 与 DeepSeek 的
/// `content`/`reasoning_text` 相反，这里**必须**取 summary，否则思考内容全丢；
/// `content` 作为兜底兼容服务端可能的完整推理形态。回传侧按 summary 原样重建
/// （见 [`reasoning_item`]），不存在 DeepSeek「summary 被误当 reasoning_text 回传」
/// 的污染问题。
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
        // 内置工具调用 item（web_search_call/code_interpreter_call/mcp_call 等 8 类）：
        // 由服务端执行，中立层不承载，丢弃。数量经流式终止摘要的 unknown_item 计数观测。
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
/// 事件 → chunk 映射（与 DeepSeek 骨架一致，事件名相同）：
/// - `output_item.added` → `BlockStart`（按 item 类型）+ 函数调用 `ToolCallDelta{name}`
/// - `output_text.delta` → `TextDelta`
/// - `reasoning_text.delta` → `ReasoningDelta`（百炼语义是「推理摘要增量」）
/// - `function_call_arguments.delta` → `ToolCallDelta{arguments}`
/// - `output_item.done` → `BlockEnd`
/// - `completed`/`incomplete` → `Usage` + `Finish`
/// - `failed` → 错误（官方流式事件枚举未列出该项，分支保留、不依赖；失败路径的
///   实际表现待实测校准）
///
/// `output_index` 天然单调且区分每个输出 item，直接用作 `StreamChunk` 的 index。
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
                        target: "model_provider::qwen_responses",
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

            if data.trim().is_empty() || data.trim() == "[DONE]" {
                continue;
            }

            let event: Value = match serde_json::from_str(&data) {
                Ok(v) => v,
                Err(e) => {
                    terminated_with_error = true;
                    warn!(
                        target: "model_provider::qwen_responses",
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
                "response.output_text.delta" => {
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
                "response.reasoning_text.delta" => {
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
                                target: "model_provider::qwen_responses",
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
                    warn!(
                        target: "model_provider::qwen_responses",
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
                // 协议规定的生命周期/心跳事件：不携带任何内容，正常流上每请求都会出现。
                // 必须显式忽略 —— 否则它们落到 `other` 臂，`unknown_event_count` 在每条
                // 健康流上都非零，「未知事件」这个告警就永远抓不到真正的异常了。
                "response.created"
                | "response.in_progress"
                | "response.content_part.added"
                | "response.content_part.done"
                | "response.output_text.done"
                | "response.reasoning_text.done"
                | "response.function_call_arguments.done" => {}
                // 百炼内置工具与扩展能力的事件：服务端执行、无 function_call 语义，
                // 内容经 `output_item.added/done` 的 item 类型统一丢弃。
                "response.custom_tool_call_input.delta"
                | "response.custom_tool_call_input.done"
                | "response.web_search_call.in_progress"
                | "response.web_search_call.searching"
                | "response.web_search_call.completed"
                | "response.code_interpreter_call.in_progress"
                | "response.code_interpreter_call.interpreting"
                | "response.code_interpreter_call.completed"
                | "response.mcp_call_arguments.delta"
                | "response.mcp_call_arguments.done"
                | "response.mcp_call.in_progress"
                | "response.mcp_call.completed"
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
            target: "model_provider::qwen_responses",
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
                target: "model_provider::qwen_responses",
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
impl ModelProvider for QwenResponsesAdapter {
    fn name(&self) -> &str {
        "qwen-responses"
    }

    async fn generate_full(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateResult, ProviderError> {
        let request_id = logging::next_request_id();
        let endpoint = self.responses_endpoint();
        let span = tracing::info_span!(
            "qwen_responses_generate_full",
            provider = "qwen-responses",
            endpoint = %endpoint,
            model = %request.model,
            request_id = %request_id,
        );

        async move {
            let started = std::time::Instant::now();
            let body =
                build_responses_request_body(request, false, self.strict_feature_validation)?;
            let input = logging::summarize_input(&request.input);

            debug!(
                target: "model_provider::qwen_responses",
                request_id = %request_id,
                model = %request.model,
                endpoint = %endpoint,
                input_items = request.input.len(),
                messages = input.messages,
                function_calls = input.function_calls,
                function_call_outputs = input.function_call_outputs,
                reasoning_items = input.reasoning,
                carry_reasoning = should_carry_reasoning(
                    &request.input,
                    !request.tools.is_empty(),
                    reasoning_enabled(request),
                ),
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
                target: "model_provider::qwen_responses",
                request_id = %request_id,
                body = %String::from_utf8_lossy(&body),
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
                    target: "model_provider::qwen_responses",
                    request_id = %request_id,
                    provider_request_id = provider_request_id.as_deref().unwrap_or("-"),
                    status = status.as_u16(),
                    latency_ms,
                    body = %body_str,
                    "Qwen responses API 返回错误状态"
                );
                return Err(ProviderError::Api {
                    status: status.as_u16(),
                    body: body_str,
                });
            }

            trace!(
                target: "model_provider::qwen_responses",
                request_id = %request_id,
                body = %String::from_utf8_lossy(&response_body),
                "responses 响应体全文"
            );

            let api_response: ResponsesResponse = serde_json::from_slice(&response_body)?;
            let response_status =
                responses_status_to_response_status(api_response.status.as_deref());
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

            let usage = api_response
                .usage
                .map(responses_usage_to_usage)
                .unwrap_or_default();

            let blocks = logging::summarize_blocks(&output);
            debug!(
                target: "model_provider::qwen_responses",
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
        let body = build_responses_request_body(request, true, self.strict_feature_validation)?;
        let endpoint = self.responses_endpoint();
        let model = request.model.clone();
        let request_id = logging::next_request_id();
        let input = logging::summarize_input(&request.input);

        debug!(
            target: "model_provider::qwen_responses",
            request_id = %request_id,
            model = %model,
            endpoint = %endpoint,
            input_items = request.input.len(),
            messages = input.messages,
            function_calls = input.function_calls,
            function_call_outputs = input.function_call_outputs,
            reasoning_items = input.reasoning,
            carry_reasoning = should_carry_reasoning(
                &request.input,
                !request.tools.is_empty(),
                reasoning_enabled(request),
            ),
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
            target: "model_provider::qwen_responses",
            request_id = %request_id,
            body = %String::from_utf8_lossy(&body),
            "responses 流式请求体全文"
        );

        let span = tracing::info_span!(
            "qwen_responses_stream",
            provider = "qwen-responses",
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

    fn make_request() -> GenerateRequest {
        GenerateRequest {
            model: "qwen3.8-max".to_string(),
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

    fn weather_tool() -> crate::ToolDefinition {
        crate::ToolDefinition {
            name: "get_weather".to_string(),
            description: "获取天气".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    #[test]
    fn test_build_request_body_basic() {
        let request = make_request();
        let body = build_responses_request_body(&request, false, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["model"], "qwen3.8-max");
        assert_eq!(json["instructions"], "你是一个助手");
        // 无状态回放：显式不上云留存（默认 true）。
        assert_eq!(json["store"], false);
        let input = json["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["text"], "你好");
        // 未配置 reasoning 时省略字段，依赖服务端默认。
        assert!(json.get("reasoning").is_none());
    }

    #[test]
    fn test_build_request_body_stream_has_no_stream_options() {
        // `stream_options.include_usage` 是 chat completions 语义；
        // Responses 的 usage 经 `response.completed` 事件返回。
        let request = make_request();
        let body = build_responses_request_body(&request, true, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["stream"], true);
        assert!(json.get("stream_options").is_none());
    }

    #[test]
    fn test_build_request_body_with_tools_flat() {
        let mut request = make_request();
        request.tools = vec![weather_tool()];
        request.tool_choice = Some(ToolChoice::None);
        let body = build_responses_request_body(&request, true, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["name"], "get_weather");
        assert_eq!(json["tools"][0]["description"], "获取天气");
        // Responses 工具定义是扁平的，无嵌套 function 包裹
        assert!(json["tools"][0].get("function").is_none());
        // 百炼 tool_choice 取值为字符串
        assert_eq!(json["tool_choice"], serde_json::json!("none"));
        assert_eq!(json["stream"], true);
    }

    #[test]
    fn test_tool_choice_required_single_tool() {
        let mut request = make_request();
        request.tools = vec![weather_tool()];
        request.tool_choice = Some(ToolChoice::Required);
        let body = build_responses_request_body(&request, false, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["tool_choice"], serde_json::json!("required"));
    }

    #[test]
    fn test_tool_choice_required_multi_tool_downgrades_to_auto() {
        // 官方约束：required 仅单工具可用；宽松模式下降级 auto。
        let mut request = make_request();
        request.tools = vec![weather_tool(), weather_tool()];
        request.tool_choice = Some(ToolChoice::Required);
        let body = build_responses_request_body(&request, false, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["tool_choice"], serde_json::json!("auto"));
    }

    #[test]
    fn test_tool_choice_required_multi_tool_strict_errors() {
        let mut request = make_request();
        request.tools = vec![weather_tool(), weather_tool()];
        request.tool_choice = Some(ToolChoice::Required);
        let err = build_responses_request_body(&request, false, true).unwrap_err();
        assert!(err.to_string().contains("required"));
    }

    #[test]
    fn test_tool_choice_named_maps_to_allowed_tools() {
        // 百炼无 named 形式，用 allowed_tools 对象忠实映射。
        let mut request = make_request();
        request.tools = vec![weather_tool(), weather_tool()];
        request.tool_choice = Some(ToolChoice::Named {
            name: "get_weather".to_string(),
        });
        let body = build_responses_request_body(&request, false, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["tool_choice"],
            serde_json::json!({
                "type": "allowed_tools",
                "mode": "required",
                "tools": [{ "type": "function", "name": "get_weather" }]
            })
        );
    }

    #[test]
    fn test_tool_choice_without_tools_is_omitted() {
        // 无工具时 tool_choice 无意义，发送可能被网关拒绝。
        let mut request = make_request();
        request.tool_choice = Some(ToolChoice::Required);
        let body = build_responses_request_body(&request, false, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("tool_choice").is_none());
    }

    #[test]
    fn test_max_output_tokens_clamped_to_minimum() {
        // 官方下限 16；低于下限夹取而不是让整轮请求 400。
        let mut request = make_request();
        request.max_output_tokens = Some(8);
        let body = build_responses_request_body(&request, false, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["max_output_tokens"], 16);

        request.max_output_tokens = Some(256);
        let body = build_responses_request_body(&request, false, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["max_output_tokens"], 256);
    }

    #[test]
    fn test_reasoning_effort_mappings() {
        let mut request = make_request();
        for (effort, expected) in [
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::Max, "max"),
        ] {
            request.reasoning = Some(ReasoningConfig {
                enabled: true,
                effort: Some(effort),
            });
            let body = build_responses_request_body(&request, false, false).unwrap();
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["reasoning"], serde_json::json!({ "effort": expected }));
        }
    }

    #[test]
    fn test_reasoning_disabled_maps_to_effort_none() {
        let mut request = make_request();
        request.reasoning = Some(ReasoningConfig {
            enabled: false,
            effort: Some(ReasoningEffort::High),
        });
        let body = build_responses_request_body(&request, false, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["reasoning"], serde_json::json!({ "effort": "none" }));
    }

    #[test]
    fn test_build_request_body_preserves_reasoning_when_tools_present() {
        // 携带 tools 时，reasoning 项以百炼输出的 summary 形态回传。
        let mut request = make_request();
        request.tools = vec![weather_tool()];
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "hi".to_string(),
            }),
            Arc::new(InputItem::Reasoning {
                content: "思考".to_string(),
            }),
        ]);
        let body = build_responses_request_body(&request, false, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let input = json["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[1]["type"], "reasoning");
        assert_eq!(input[1]["summary"][0]["type"], "summary_text");
        assert_eq!(input[1]["summary"][0]["text"], "思考");
    }

    #[test]
    fn test_build_request_body_drops_reasoning_when_no_tools() {
        // 未携带 tools 且历史无工具调用时，reasoning 会被 API 忽略，序列化时直接丢弃。
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
        let body = build_responses_request_body(&request, false, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["input"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_build_request_body_keeps_reasoning_for_replayed_calls_without_tools() {
        // 历史含 function_call 时即便本轮 tools 为空（无工具的后续轮 / 子 Agent /
        // 轮次间改配置），前置 reasoning 也一并回传，保持多轮思考连贯。
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

        let body = build_responses_request_body(&request, false, false).unwrap();
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

        let body = build_responses_request_body(&request, false, false).unwrap();
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
    fn test_text_format_omitted_loose_and_rejected_strict() {
        // text.format 支持性未证实：宽松模式省略，严格模式拒绝。
        let mut request = make_request();
        request.text = Some(crate::TextConfig {
            format: Some(crate::TextFormat::JsonObject),
        });
        let body = build_responses_request_body(&request, false, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("text").is_none());

        let err = build_responses_request_body(&request, false, true).unwrap_err();
        assert!(err.to_string().contains("text.format"));
    }

    #[test]
    fn test_response_output_to_blocks() {
        let output = serde_json::json!([
            {
                "type": "reasoning",
                "summary": [{ "type": "summary_text", "text": "思考中" }]
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
    fn test_reasoning_summary_is_kept() {
        // 与 DeepSeek 相反：百炼只输出 summary（摘要），丢弃它思考内容就全丢。
        let summary_only = serde_json::json!({
            "type": "reasoning",
            "summary": [{ "type": "summary_text", "text": "摘要" }]
        });
        match response_item_to_block(&summary_only) {
            Some(ContentBlock::Reasoning { text }) => assert_eq!(text, "摘要"),
            other => panic!("expected Reasoning, got {other:?}"),
        }

        // summary 优先于 content；两者并存时不拼接。
        let both = serde_json::json!({
            "type": "reasoning",
            "summary": [{ "type": "summary_text", "text": "摘要" }],
            "content": [{ "type": "reasoning_text", "text": "完整推理" }]
        });
        match response_item_to_block(&both) {
            Some(ContentBlock::Reasoning { text }) => assert_eq!(text, "摘要"),
            other => panic!("expected Reasoning, got {other:?}"),
        }

        // content 兜底：summary 缺失时取 content。
        let content_only = serde_json::json!({
            "type": "reasoning",
            "content": [{ "type": "reasoning_text", "text": "完整推理" }]
        });
        match response_item_to_block(&content_only) {
            Some(ContentBlock::Reasoning { text }) => assert_eq!(text, "完整推理"),
            other => panic!("expected Reasoning, got {other:?}"),
        }
    }

    #[test]
    fn test_builtin_tool_items_are_dropped() {
        // 内置工具调用 item（服务端执行）不映射到中立层。
        let web_search = serde_json::json!({
            "type": "web_search_call",
            "action": { "query": "weather" }
        });
        assert!(response_item_to_block(&web_search).is_none());
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
    fn test_responses_endpoint_preserves_v1() {
        // 百炼 responses 挂在 compatible-mode/v1 下，保留（而非剥离）`/v1` 后缀。
        let client = QwenResponsesAdapter::new("sk-test")
            .unwrap()
            .with_base_url("https://dashscope.aliyuncs.com/compatible-mode/v1");
        assert_eq!(
            client.responses_endpoint(),
            "https://dashscope.aliyuncs.com/compatible-mode/v1/responses"
        );
        assert_eq!(client.name(), "qwen-responses");

        // 尾斜杠容错。
        let trailing = QwenResponsesAdapter::new("sk-test")
            .unwrap()
            .with_base_url("https://dashscope.aliyuncs.com/compatible-mode/v1/");
        assert_eq!(
            trailing.responses_endpoint(),
            "https://dashscope.aliyuncs.com/compatible-mode/v1/responses"
        );
    }

    /// 提取 `input[]` 的 type 序列，便于断言排布。
    fn types_of(values: &[Value]) -> Vec<&str> {
        values.iter().map(|v| v["type"].as_str().unwrap()).collect()
    }

    #[test]
    fn test_sequential_call_output_pair_is_adjacent() {
        // 百炼官方约束：function_call_output 必须紧跟对应的 function_call。
        let items = vec![
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "hi".to_string(),
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
        ];
        let values = input_items_to_responses_values(&items, true);
        assert_eq!(
            types_of(&values),
            vec!["message", "function_call", "function_call_output"]
        );
        assert_eq!(values[1]["call_id"], "c1");
        assert_eq!(values[2]["call_id"], "c1");
    }

    #[test]
    fn test_parallel_calls_reordered_pairwise() {
        // 回归：并行调用下 output 乱序到达（out_b 先于 out_a），冲刷时按 call
        // 顺序重排配对，产出逐对相邻的 call_a,out_a,call_b,out_b。
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
                "function_call_output",
                "function_call",
                "function_call_output",
            ]
        );
        assert_eq!(values[1]["call_id"], "a");
        assert_eq!(values[2]["call_id"], "a");
        assert_eq!(values[3]["call_id"], "b");
        assert_eq!(values[4]["call_id"], "b");
    }

    #[test]
    fn test_multi_iteration_pairs_keep_own_reasoning() {
        // 多轮 ReAct：每组 pair 前都有自己的 reasoning。
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
    fn test_unpaired_call_and_orphan_output_are_dropped() {
        // 组内每个 call 都必须有配对 output，轮次被中断的残缺项直接丢弃；
        // 反之无配对 call 的 output 同样丢弃。
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
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "ghost".to_string(),
                output: "orphan".to_string(),
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
        // 编码器需把文本归位到 reasoning 之后（与 DeepSeek 的归位策略一致）。
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
    fn test_text_between_call_and_output_is_deferred() {
        // 配对序列开启期间到达的 assistant 文本推迟到冲刷之后，保持 call 与
        // output 相邻不被打断。
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
        assert_eq!(values[2]["content"][0]["text"], "预览文本");
    }

    #[test]
    fn test_text_before_call_keeps_natural_order() {
        // call 之前到达的文本保持在 call 前面（自然顺序）。
        let items = vec![
            Arc::new(InputItem::Message {
                role: Role::Assistant,
                content: "先说两句".to_string(),
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
        ];
        let values = input_items_to_responses_values(&items, true);
        assert_eq!(
            types_of(&values),
            vec!["message", "function_call", "function_call_output"]
        );
        assert_eq!(values[0]["content"][0]["text"], "先说两句");
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

    // ── 流式端到端（本地一次性 SSE 服务，与 qwen/chat.rs 测试同模式） ──

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
        client: &QwenResponsesAdapter,
        request: &GenerateRequest,
    ) -> Vec<Result<StreamChunk, ProviderError>> {
        let mut stream = client.generate_stream(request).await.unwrap();
        let mut chunks = Vec::new();
        while let Some(chunk) = stream.next().await {
            chunks.push(chunk);
        }
        chunks
    }

    /// 全事件链：reasoning 摘要 → 文本，生命周期事件（`response.created`、
    /// `*.done`）被忽略，`output_item.done` 产出完整块，`completed` 收敛。
    #[tokio::test]
    async fn test_stream_text_and_reasoning_assembly() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.created","response":{"id":"r1"}}"#,
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning"}}"#,
            r#"{"type":"response.reasoning_text.delta","output_index":0,"delta":"先想"}"#,
            r#"{"type":"response.reasoning_text.delta","output_index":0,"delta":"一下"}"#,
            r#"{"type":"response.reasoning_text.done","output_index":0,"text":"先想一下"}"#,
            r#"{"type":"response.output_item.added","output_index":1,"item":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"response.output_text.delta","output_index":1,"delta":"你好"}"#,
            r#"{"type":"response.output_text.done","output_index":1,"text":"你好"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","summary":[{"type":"summary_text","text":"先想一下"}]}}"#,
            r#"{"type":"response.output_item.done","output_index":1,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"你好"}]}}"#,
            r#"{"type":"response.completed","response":{"id":"r1","usage":{"input_tokens":6,"output_tokens":4,"total_tokens":10}}}"#,
        ]))
        .await;
        let client = QwenResponsesAdapter::new("sk-test-key")
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
        let client = QwenResponsesAdapter::new("sk-test-key")
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

    /// `response.incomplete` → `Finish::MaxTokens`；未 `output_item.done` 的缓冲
    /// 由安全网兜底合成 BlockEnd。
    #[tokio::test]
    async fn test_stream_incomplete_maps_to_max_tokens_with_safety_net_flush() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"部分"}"#,
            r#"{"type":"response.incomplete","response":{"id":"r3","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":5,"output_tokens":16,"total_tokens":21}}}"#,
        ]))
        .await;
        let client = QwenResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(matches!(
            &chunks[..],
            [
                Ok(StreamChunk::BlockStart { block_type: BlockType::Text, .. }),
                Ok(StreamChunk::TextDelta { .. }),
                Ok(StreamChunk::BlockEnd { block: ContentBlock::Text { text }, .. }),
                Ok(StreamChunk::Usage { .. }),
                Ok(StreamChunk::Finish { reason: FinishReason::MaxTokens }),
            ] if text == "部分"
        ));
    }

    /// 百炼内置工具事件与 item（web_search_call/code_interpreter_call/mcp_call）
    /// 不产生任何 chunk，也不影响后续 message 块。
    #[tokio::test]
    async fn test_stream_builtin_tool_events_are_ignored() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.created","response":{}}"#,
            r#"{"type":"response.web_search_call.in_progress","output_index":0}"#,
            r#"{"type":"response.web_search_call.searching","output_index":0}"#,
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"web_search_call","action":{"query":"weather"}}}"#,
            r#"{"type":"response.web_search_call.completed","output_index":0}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"web_search_call","status":"completed"}}"#,
            r#"{"type":"response.code_interpreter_call.interpreting","output_index":1}"#,
            r#"{"type":"response.mcp_call.in_progress","output_index":2}"#,
            r#"{"type":"response.output_item.added","output_index":3,"item":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"response.output_text.delta","output_index":3,"delta":"done"}"#,
            r#"{"type":"response.output_item.done","output_index":3,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#,
            r#"{"type":"response.completed","response":{"id":"r4","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#,
        ]))
        .await;
        let client = QwenResponsesAdapter::new("sk-test-key")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(matches!(
            &chunks[..],
            [
                Ok(StreamChunk::BlockStart {
                    index: 3,
                    block_type: BlockType::Text
                }),
                Ok(StreamChunk::TextDelta { index: 3, .. }),
                Ok(StreamChunk::BlockEnd { index: 3, .. }),
                Ok(StreamChunk::Usage { .. }),
                Ok(StreamChunk::Finish { .. }),
            ]
        ));
        assert_eq!(chunks.len(), 5);
    }

    /// 官方事件枚举无 `response.failed`，分支仅兜底：产出错误且不再有正常收尾。
    #[tokio::test]
    async fn test_stream_failed_event_aborts_with_api_error() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.created","response":{}}"#,
            r#"{"type":"response.failed","response":{"error":{"code":"ServerError","message":"内部错误"}}}"#,
        ]))
        .await;
        let client = QwenResponsesAdapter::new("sk-test-key")
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

    /// 无 `response.completed`/`incomplete`（EOF 即断开）：缓冲兜底合成 BlockEnd，
    /// Finish 缺省 Stop、Usage 缺省 0 —— 不挂起、不丢已收内容。
    #[tokio::test]
    async fn test_stream_eof_without_terminated_event_uses_safety_net() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"你好"}"#,
        ]))
        .await;
        let client = QwenResponsesAdapter::new("sk-test-key")
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
}
