//! DeepSeek Anthropic 兼容端点适配器。
//!
//! 提供 [`DeepSeekAnthropicAdapter`]，为 DeepSeek 的 `/anthropic/v1/messages`
//! 端点实现 [`ModelProvider`]，接入其**服务端** `web_search` 工具：请求侧把中立标记
//! [`SERVER_TOOL_WEB_SEARCH`] 翻译成 `web_search_20250305` 工具声明，响应侧把
//! `server_tool_use` / `web_search_tool_result` 落到中立层的
//! [`ContentBlock::ServerToolUse`] / [`ContentBlock::ServerToolResult`] ——
//! 引擎只透传，不进入客户端工具执行循环。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Map, Value};
use tracing::{Instrument, warn};

use crate::logging;
use crate::providers::chat_common::strip_image_parts;
use crate::response::{
    BlockType, Content, ContentBlock, FinishReason, GenerateRequest, GenerateResult, InputItem,
    ResponseError, ResponseStatus, Role, SERVER_TOOL_WEB_SEARCH, StreamChunk,
};
use crate::streaming::sse::{SseEvent, StreamingEventSource};
use crate::{GenerateStream, ModelProvider, ProviderError, Usage};

use super::chat::DEEPSEEK_API_BASE_URL;

/// 服务端 web_search 的单次响应最大检索次数（协议字段 `max_uses`）。
const WEB_SEARCH_MAX_USES: u32 = 5;
/// Anthropic 协议版本头。
const ANTHROPIC_VERSION: &str = "2023-06-01";

// ============================================================================
// 客户端
// ============================================================================

/// DeepSeek Anthropic 兼容端点客户端，实现 [`ModelProvider`]。
///
/// 端点：`{base}/anthropic/v1/messages`。
pub struct DeepSeekAnthropicAdapter {
    http_client: reqwest::Client,
    api_key: String,
    base_url: String,
    strict_feature_validation: bool,
}

impl DeepSeekAnthropicAdapter {
    /// 使用给定的 API 密钥创建适配器。
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
            strict_feature_validation: false,
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

    /// 开启严格特性校验：不支持的输入（如图片部件）直接报错而非宽松降级。
    pub fn strict_feature_validation(mut self, strict: bool) -> Self {
        self.strict_feature_validation = strict;
        self
    }

    /// 返回 Anthropic messages 端点 URL：剥离 base 尾部 `/v1` 后拼 `/anthropic/v1/messages`。
    fn messages_endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let base = base.strip_suffix("/v1").unwrap_or(base);
        format!("{base}/anthropic/v1/messages")
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_key)
    }
}

// ============================================================================
// 请求构建
// ============================================================================

/// 一组未收束的 assistant 侧产出（Reasoning + 消息文本 + 工具调用）。
///
/// 收束为一条 assistant 消息时统一归位为 thinking → text → tool_use。
#[derive(Default)]
struct AssistantGroup {
    thinking: Vec<String>,
    text: String,
    /// `(call_id, tool_use 消息块)`。
    tool_uses: Vec<(String, Value)>,
}

impl AssistantGroup {
    fn is_empty(&self) -> bool {
        self.thinking.is_empty() && self.text.is_empty() && self.tool_uses.is_empty()
    }
}

/// Anthropic `messages[]` 的分组构建器：收束 assistant 组、配对工具结果、
/// 保证 user/assistant 角色交替（连续同角色会被协议 400 拒绝）。
#[derive(Default)]
struct MessageBuilder {
    messages: Vec<Value>,
    /// 顶层 `system` 段（instructions 在前，历史 System/Developer 追加其后）。
    system_parts: Vec<String>,
    group: AssistantGroup,
    /// 待落位的工具结果（到达于其所属 assistant 组收束之前）。
    outputs: Vec<(String, Value)>,
    /// 最近一次 assistant 收束实际回放的 tool_use id 集合。
    last_call_ids: HashSet<String>,
}

impl MessageBuilder {
    /// 收束当前 assistant 组为一条消息。
    ///
    /// 无配对 output 的 tool_use 就地丢弃并 warn（残缺组只出现在轮次被中断的
    /// 异常历史里，留着会被协议以「无对应 tool_result」拒绝）。
    fn flush_group(&mut self) {
        if self.group.is_empty() {
            self.last_call_ids.clear();
            return;
        }
        let group = std::mem::take(&mut self.group);
        let mut content = Vec::new();
        for thinking in &group.thinking {
            content.push(serde_json::json!({ "type": "thinking", "thinking": thinking }));
        }
        if !group.text.is_empty() {
            content.push(serde_json::json!({ "type": "text", "text": group.text }));
        }

        let mut matched_ids: HashSet<String> = HashSet::new();
        let mut dropped: Vec<String> = Vec::new();
        for (call_id, tool_use) in group.tool_uses {
            if self.outputs.iter().any(|(oid, _)| oid == &call_id) {
                matched_ids.insert(call_id.clone());
                content.push(tool_use);
            } else {
                dropped.push(call_id);
            }
        }
        if !dropped.is_empty() {
            warn!(
                target: "model_provider::deepseek",
                dropped_call_ids = ?dropped,
                count = dropped.len(),
                "dropping tool_use without matching output (removed from replayed history)"
            );
        }

        self.last_call_ids = matched_ids;
        if !content.is_empty() {
            self.messages
                .push(serde_json::json!({ "role": "assistant", "content": content }));
        }
    }

    /// 落位待处理的工具结果为一条 user 消息；只回放配对成功的 id。
    fn flush_outputs(&mut self) {
        if self.outputs.is_empty() {
            return;
        }
        let outputs = std::mem::take(&mut self.outputs);
        let mut results = Vec::new();
        let mut orphaned: Vec<String> = Vec::new();
        for (call_id, result) in outputs {
            if self.last_call_ids.contains(&call_id) {
                results.push(result);
            } else {
                orphaned.push(call_id);
            }
        }
        if !orphaned.is_empty() {
            warn!(
                target: "model_provider::deepseek",
                orphaned_call_ids = ?orphaned,
                count = orphaned.len(),
                "dropping tool_result without matching replayed tool_use"
            );
        }
        if !results.is_empty() {
            self.push_user(Value::Array(results));
        }
    }

    /// 推入一条 user 内容；上一条已是 user 时合并（协议要求角色交替）。
    fn push_user(&mut self, content: Value) {
        if let Some(last) = self.messages.last_mut()
            && last.get("role").and_then(Value::as_str) == Some("user")
        {
            merge_user_content(last, content);
            return;
        }
        self.messages
            .push(serde_json::json!({ "role": "user", "content": content }));
    }

    fn finish(self, instructions: Option<&str>) -> (Option<String>, Vec<Value>) {
        let mut system_parts = self.system_parts;
        if let Some(instructions) = instructions {
            system_parts.insert(0, instructions.to_string());
        }
        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        };
        (system, self.messages)
    }
}

/// 把新到的 user 内容并入上一条 user 消息（string ↔ array 两种形态都兼容）。
fn merge_user_content(message: &mut Value, incoming: Value) {
    let Some(content) = message.get_mut("content") else {
        return;
    };
    match (&mut *content, incoming) {
        (Value::String(existing), Value::String(new_text)) => {
            let orig = std::mem::take(existing);
            *content = Value::Array(vec![
                serde_json::json!({ "type": "text", "text": orig }),
                serde_json::json!({ "type": "text", "text": new_text }),
            ]);
        }
        (Value::String(existing), Value::Array(mut blocks)) => {
            let orig = std::mem::take(existing);
            let mut merged = vec![serde_json::json!({ "type": "text", "text": orig })];
            merged.append(&mut blocks);
            *content = Value::Array(merged);
        }
        (Value::Array(arr), Value::String(text)) => {
            arr.push(serde_json::json!({ "type": "text", "text": text }));
        }
        (Value::Array(arr), Value::Array(blocks)) => arr.extend(blocks),
        _ => {}
    }
}

/// user 内容编码：纯文本 → 裸字符串；部件数组 → text 块数组（图片已被剥离）。
fn user_content_value(content: &Content) -> Value {
    match content {
        Content::Text(text) => Value::String(text.clone()),
        Content::Parts(parts) => {
            let blocks: Vec<Value> = parts
                .iter()
                .filter_map(|part| match part {
                    crate::response::ContentPart::Text { text } => {
                        Some(serde_json::json!({ "type": "text", "text": text }))
                    }
                    _ => None,
                })
                .collect();
            if blocks.is_empty() {
                Value::String(String::new())
            } else {
                Value::Array(blocks)
            }
        }
    }
}

/// 把 FunctionCall 的 raw `arguments` 字符串解析成对象（失败回退 `{}` 并 warn）。
fn parse_tool_arguments(arguments: &str) -> Value {
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(map)) => Value::Object(map),
        Ok(_) => {
            warn!(
                target: "model_provider::deepseek",
                "tool arguments JSON is not an object; falling back to empty object"
            );
            Value::Object(Map::new())
        }
        Err(_) => {
            warn!(
                target: "model_provider::deepseek",
                "failed to parse tool arguments JSON; falling back to empty object"
            );
            Value::Object(Map::new())
        }
    }
}

/// 把有序 [`InputItem`] 列表转换为 Anthropic `messages[]`（附带顶层 `system`）。
///
/// - 历史 System/Developer 并入 `system`（instructions 在前）；
/// - 连续的 Reasoning + assistant 消息 + FunctionCall 收束为一条 assistant 消息；
/// - FunctionCallOutput 收束为 user 消息的 `tool_result` 块，连续结果合并以满足
///   Anthropic 的角色交替约束。
fn input_items_to_anthropic_messages(
    items: &[Arc<InputItem>],
    instructions: Option<&str>,
) -> (Option<String>, Vec<Value>) {
    let mut builder = MessageBuilder::default();

    for item in items {
        match &**item {
            InputItem::Reasoning { content } => {
                if !builder.outputs.is_empty() {
                    builder.flush_group();
                    builder.flush_outputs();
                }
                builder.group.thinking.push(content.clone());
            }
            InputItem::Message { role, content } => match role {
                Role::System | Role::Developer => {
                    builder.flush_group();
                    builder.flush_outputs();
                    builder.system_parts.push(content.text_view().into_owned());
                }
                Role::User => {
                    builder.flush_group();
                    builder.flush_outputs();
                    let value = user_content_value(content);
                    builder.push_user(value);
                }
                Role::Assistant => {
                    if !builder.outputs.is_empty() {
                        builder.flush_group();
                        builder.flush_outputs();
                    }
                    builder.group.text.push_str(&content.text_view());
                }
            },
            InputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                if !builder.outputs.is_empty() {
                    builder.flush_group();
                    builder.flush_outputs();
                }
                let input = parse_tool_arguments(arguments);
                builder.group.tool_uses.push((
                    call_id.clone(),
                    serde_json::json!({
                        "type": "tool_use",
                        "id": call_id,
                        "name": name,
                        "input": input,
                    }),
                ));
            }
            InputItem::FunctionCallOutput { call_id, output } => {
                let result = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": output.text_view().into_owned(),
                });
                builder.outputs.push((call_id.clone(), result));
            }
        }
    }

    builder.flush_group();
    builder.flush_outputs();
    builder.finish(instructions)
}

/// 构建 Anthropic messages 请求体。
fn build_request_body(
    request: &GenerateRequest,
    stream: bool,
    strict: bool,
) -> Result<Vec<u8>, ProviderError> {
    let (stripped_input, images_dropped) = strip_image_parts(&request.input, strict)?;
    if images_dropped > 0 {
        warn!(
            target: "model_provider::deepseek",
            images = images_dropped,
            "deepseek anthropic does not support image input; image parts stripped (text kept)"
        );
    }
    let (system, messages) =
        input_items_to_anthropic_messages(&stripped_input, request.instructions.as_deref());

    let mut body = Map::new();
    body.insert("model".into(), serde_json::json!(request.model));
    if let Some(system) = system {
        body.insert("system".into(), serde_json::json!(system));
    }
    body.insert("messages".into(), Value::Array(messages));

    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|t| {
            if t.name == SERVER_TOOL_WEB_SEARCH {
                serde_json::json!({
                    "type": "web_search_20250305",
                    "name": "web_search",
                    "max_uses": WEB_SEARCH_MAX_USES,
                })
            } else {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            }
        })
        .collect();
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
    }

    if let Some(max_tokens) = request.max_output_tokens {
        body.insert("max_tokens".into(), serde_json::json!(max_tokens));
    }
    if let Some(t) = request.temperature {
        body.insert("temperature".into(), serde_json::json!(t));
    }
    if let Some(p) = request.top_p {
        body.insert("top_p".into(), serde_json::json!(p));
    }
    if let Some(reasoning) = &request.reasoning {
        let thinking = if reasoning.enabled {
            serde_json::json!({ "type": "enabled" })
        } else {
            serde_json::json!({ "type": "disabled" })
        };
        body.insert("thinking".into(), thinking);
    }
    if stream {
        body.insert("stream".into(), serde_json::json!(true));
    }

    serde_json::to_vec(&Value::Object(body))
        .map_err(|e| ProviderError::Request(format!("序列化 anthropic 请求失败: {e}")))
}

// ============================================================================
// 响应解析
// ============================================================================

#[derive(Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    id: String,
    #[serde(default)]
    content: Vec<Value>,
    stop_reason: Option<String>,
    usage: Option<AnthropicUsage>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize, Default)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    server_tool_use: Option<AnthropicServerToolUse>,
}

#[derive(Deserialize, Default)]
struct AnthropicServerToolUse {
    #[serde(default)]
    web_search_requests: u64,
}

/// Anthropic 的 input 不含缓存段：并入 input 以与其它适配器「全量提示词」口径一致。
fn anthropic_usage_to_usage(u: &AnthropicUsage) -> Usage {
    let input = u.input_tokens + u.cache_creation_input_tokens + u.cache_read_input_tokens;
    Usage {
        input_tokens: input,
        output_tokens: u.output_tokens,
        total_tokens: input + u.output_tokens,
    }
}

/// `stop_reason` → (状态, 结束原因, 失败文案)。未知取值按失败处理。
fn map_stop_reason(stop: Option<&str>) -> (ResponseStatus, Option<FinishReason>, Option<String>) {
    match stop {
        Some("end_turn") | Some("stop_sequence") => {
            (ResponseStatus::Completed, Some(FinishReason::Stop), None)
        }
        Some("tool_use") => (
            ResponseStatus::Completed,
            Some(FinishReason::ToolCalls),
            None,
        ),
        Some("max_tokens") => (
            ResponseStatus::Incomplete,
            Some(FinishReason::MaxTokens),
            None,
        ),
        None => (ResponseStatus::Completed, None, None),
        Some(other) => {
            warn!(
                target: "model_provider::deepseek",
                stop_reason = other,
                "unexpected anthropic stop_reason; treating response as failed"
            );
            (
                ResponseStatus::Failed,
                Some(FinishReason::Error),
                Some(format!("unexpected stop_reason: {other}")),
            )
        }
    }
}

/// 流式 `stop_reason` → [`FinishReason`]（无 stop_reason 按 Stop 收敛）。
fn stop_reason_to_finish(stop: Option<&str>) -> FinishReason {
    match stop {
        Some("tool_use") => FinishReason::ToolCalls,
        Some("max_tokens") => FinishReason::MaxTokens,
        Some(other) if other != "end_turn" && other != "stop_sequence" => FinishReason::Error,
        _ => FinishReason::Stop,
    }
}

/// Anthropic `content[]` 单块 → 中立 [`ContentBlock`]；未知类型返回 None。
fn content_block_to_neutral(block: &Value) -> Option<ContentBlock> {
    let kind = block.get("type").and_then(Value::as_str)?;
    match kind {
        "thinking" => {
            let text = block.get("thinking").and_then(Value::as_str)?;
            (!text.is_empty()).then(|| ContentBlock::Reasoning {
                text: text.to_string(),
            })
        }
        "text" => {
            let text = block.get("text").and_then(Value::as_str)?;
            (!text.is_empty()).then(|| ContentBlock::Text {
                text: text.to_string(),
            })
        }
        "tool_use" => Some(ContentBlock::ToolCall {
            call_id: block.get("id").and_then(Value::as_str)?.to_string(),
            name: block.get("name").and_then(Value::as_str)?.to_string(),
            arguments: block
                .get("input")
                .map(Value::to_string)
                .unwrap_or_else(|| "{}".to_string()),
        }),
        "server_tool_use" => Some(ContentBlock::ServerToolUse {
            call_id: block.get("id").and_then(Value::as_str)?.to_string(),
            name: block.get("name").and_then(Value::as_str)?.to_string(),
            input: block
                .get("input")
                .map(Value::to_string)
                .unwrap_or_else(|| "{}".to_string()),
        }),
        "web_search_tool_result" => Some(ContentBlock::ServerToolResult {
            call_id: block
                .get("tool_use_id")
                .and_then(Value::as_str)?
                .to_string(),
            name: "web_search".to_string(),
            content: block.get("content")?.to_string(),
        }),
        _ => None,
    }
}

/// 解析非流式响应体为 [`GenerateResult`]。
fn parse_non_stream(
    response_body: &[u8],
    request_id: &str,
) -> Result<GenerateResult, ProviderError> {
    let api_response: AnthropicResponse = serde_json::from_slice(response_body)?;
    if let Some(error) = &api_response.error {
        return Err(ProviderError::Api {
            status: 500,
            body: error.to_string(),
        });
    }

    let mut output = Vec::with_capacity(api_response.content.len());
    let mut skipped_types: HashSet<String> = HashSet::new();
    for block in &api_response.content {
        match content_block_to_neutral(block) {
            Some(block) => output.push(block),
            None => {
                let kind = block
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>")
                    .to_string();
                skipped_types.insert(kind);
            }
        }
    }
    if !skipped_types.is_empty() {
        warn!(
            target: "model_provider::deepseek",
            request_id = %request_id,
            skipped_types = ?skipped_types,
            "skipped unknown or malformed anthropic content blocks"
        );
    }

    let (status, finish_reason, failure) = map_stop_reason(api_response.stop_reason.as_deref());
    let usage = api_response.usage.as_ref().map(anthropic_usage_to_usage);
    if let Some(usage) = &usage {
        let web_search_requests = api_response
            .usage
            .as_ref()
            .and_then(|u| u.server_tool_use.as_ref())
            .map(|s| s.web_search_requests)
            .unwrap_or(0);
        tracing::debug!(
            target: "model_provider::deepseek",
            request_id = %request_id,
            web_search_requests,
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            "anthropic response carried server web_search usage"
        );
    }

    Ok(GenerateResult {
        id: api_response.id,
        output,
        usage: usage.unwrap_or_default(),
        status,
        finish_reason,
        error: failure.map(|message| ResponseError {
            code: None,
            message,
        }),
    })
}

// ============================================================================
// 流式处理
// ============================================================================

/// 单个内容块的流式累积态（由 `content_block_start` 建立，`content_block_stop` 收束）。
enum PendingBlock {
    Text(String),
    Thinking(String),
    /// 客户端工具：输入增量经 [`StreamChunk::ToolCallDelta`] 下发（与 responses 适配器一致）。
    ToolUse {
        call_id: String,
        name: String,
        arguments: String,
    },
    /// 服务端工具：只累积、不发 delta。
    ServerToolUse {
        call_id: String,
        name: String,
        input: String,
    },
    ServerToolResult {
        call_id: String,
        content: Value,
    },
    Unknown,
}

impl PendingBlock {
    /// 收束为完整块；无有效内容返回 None。
    fn into_block(self) -> Option<ContentBlock> {
        match self {
            PendingBlock::Text(text) if !text.is_empty() => Some(ContentBlock::Text { text }),
            PendingBlock::Thinking(text) if !text.is_empty() => {
                Some(ContentBlock::Reasoning { text })
            }
            PendingBlock::ToolUse {
                call_id,
                name,
                arguments,
            } if !call_id.is_empty() && !name.is_empty() => {
                let arguments = if arguments.is_empty() || arguments.trim() == "null" {
                    "{}".to_string()
                } else {
                    arguments
                };
                Some(ContentBlock::ToolCall {
                    call_id,
                    name,
                    arguments,
                })
            }
            PendingBlock::ServerToolUse {
                call_id,
                name,
                input,
            } if !call_id.is_empty() && !name.is_empty() => {
                let input = if input.is_empty() || input.trim() == "null" {
                    "{}".to_string()
                } else {
                    input
                };
                Some(ContentBlock::ServerToolUse {
                    call_id,
                    name,
                    input,
                })
            }
            PendingBlock::ServerToolResult { call_id, content } if !call_id.is_empty() => {
                Some(ContentBlock::ServerToolResult {
                    call_id,
                    name: "web_search".to_string(),
                    content: content.to_string(),
                })
            }
            _ => None,
        }
    }
}

/// 流式 usage 累计器（`message_start` 给 input/cache，`message_delta` 给 output/ws）。
#[derive(Default)]
struct UsageAccum {
    input_tokens: u32,
    cache_creation: u32,
    cache_read: u32,
    output_tokens: u32,
    web_search_requests: u64,
}

impl UsageAccum {
    fn merge(&mut self, fragment: &Value) {
        if let Some(v) = fragment.get("input_tokens").and_then(Value::as_u64) {
            self.input_tokens = v as u32;
        }
        if let Some(v) = fragment
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
        {
            self.cache_creation = v as u32;
        }
        if let Some(v) = fragment
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
        {
            self.cache_read = v as u32;
        }
        if let Some(v) = fragment.get("output_tokens").and_then(Value::as_u64) {
            self.output_tokens = v as u32;
        }
        if let Some(v) = fragment
            .get("server_tool_use")
            .and_then(|s| s.get("web_search_requests"))
            .and_then(Value::as_u64)
        {
            self.web_search_requests = v;
        }
    }

    fn to_usage(&self) -> Usage {
        let input = self.input_tokens + self.cache_creation + self.cache_read;
        Usage {
            input_tokens: input,
            output_tokens: self.output_tokens,
            total_tokens: input + self.output_tokens,
        }
    }
}

/// 事件类型：优先 data 内的 `type` 字段，缺失时回退到 SSE 具名事件。
fn event_type(data: &Value, fallback: &str) -> String {
    data.get("type")
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

/// 处理 Anthropic 语义 SSE 流，产出中立 [`StreamChunk`]。
///
/// 事件 → chunk 映射：
/// - `content_block_start` → `BlockStart`（按 content_block.type 映射）；客户端
///   `tool_use` 同时发 `ToolCallDelta{name}`（与 responses 适配器一致）
/// - `text_delta`/`thinking_delta` → `TextDelta`/`ReasoningDelta`
/// - `input_json_delta` → 累积；客户端工具附带 `ToolCallDelta{arguments}`，服务端工具不发
/// - `content_block_stop` → `BlockEnd`（完整块）
/// - `message_start`/`message_delta` → 累计 usage 与 stop_reason
/// - `message_stop` → `Usage` + `Finish`；`ping` 忽略
fn process_anthropic_sse_stream(
    event_source: StreamingEventSource,
    span: tracing::Span,
    model: String,
    request_id: String,
) -> GenerateStream {
    let stream = stream! {
        let mut pending: HashMap<usize, PendingBlock> = HashMap::new();
        let mut usage_accum = UsageAccum::default();
        let mut stop_reason: Option<String> = None;
        let mut terminated_with_error = false;

        // ── 诊断计数器：逐 chunk 不打日志，流终止时一次性汇报 ──
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
        let mut malformed_event_count: u64 = 0;

        futures::pin_mut!(event_source);

        while let Some(event_result) = event_source.next().await {
            let (fallback_name, data) = match event_result {
                Ok(SseEvent::Open) => continue,
                Ok(SseEvent::Message(msg)) => (msg.event, msg.data),
                Err(provider_err) => {
                    terminated_with_error = true;
                    tracing::warn!(
                        target: "model_provider::deepseek",
                        request_id = %request_id,
                        model = %model,
                        error = %provider_err,
                        event_count,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "anthropic SSE stream transport error; aborting"
                    );
                    yield Err(provider_err);
                    break;
                }
            };

            event_count += 1;
            if data.trim().is_empty() {
                continue;
            }

            let event: Value = match serde_json::from_str(&data) {
                Ok(v) => v,
                Err(e) => {
                    terminated_with_error = true;
                    tracing::warn!(
                        target: "model_provider::deepseek",
                        request_id = %request_id,
                        model = %model,
                        error = %e,
                        event_count,
                        "failed to parse anthropic SSE event; aborting"
                    );
                    yield Err(ProviderError::Stream(format!("解析 anthropic SSE 事件失败: {e}")));
                    break;
                }
            };
            let event_type = event_type(&event, &fallback_name);

            match event_type.as_str() {
                "message_start" => {
                    if let Some(fragment) = event
                        .get("message")
                        .and_then(|m| m.get("usage"))
                        .filter(|u| u.is_object())
                    {
                        usage_accum.merge(fragment);
                    }
                }
                "content_block_start" => {
                    let Some(index) = event.get("index").and_then(Value::as_u64).map(|i| i as usize)
                    else {
                        malformed_event_count += 1;
                        continue;
                    };
                    let Some(cb) = event.get("content_block") else {
                        malformed_event_count += 1;
                        continue;
                    };
                    let Some(cb_type) = cb.get("type").and_then(Value::as_str) else {
                        malformed_event_count += 1;
                        continue;
                    };
                    match cb_type {
                        "text" => {
                            pending.insert(index, PendingBlock::Text(String::new()));
                            yield Ok(StreamChunk::BlockStart { index, block_type: BlockType::Text });
                        }
                        "thinking" => {
                            pending.insert(index, PendingBlock::Thinking(String::new()));
                            yield Ok(StreamChunk::BlockStart { index, block_type: BlockType::Reasoning });
                        }
                        "tool_use" => {
                            let call_id = cb.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                            let name = cb.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                            tool_call_count += 1;
                            first_chunk_at.get_or_insert_with(std::time::Instant::now);
                            pending.insert(index, PendingBlock::ToolUse {
                                call_id: call_id.clone(),
                                name: name.clone(),
                                arguments: String::new(),
                            });
                            yield Ok(StreamChunk::BlockStart { index, block_type: BlockType::ToolCall });
                            yield Ok(StreamChunk::ToolCallDelta {
                                index,
                                call_id,
                                name: Some(name),
                                arguments: Value::String(String::new()),
                            });
                        }
                        "server_tool_use" => {
                            let call_id = cb.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                            let name = cb.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                            first_chunk_at.get_or_insert_with(std::time::Instant::now);
                            pending.insert(index, PendingBlock::ServerToolUse {
                                call_id,
                                name,
                                input: String::new(),
                            });
                            yield Ok(StreamChunk::BlockStart { index, block_type: BlockType::ServerToolUse });
                        }
                        "web_search_tool_result" => {
                            let call_id = cb.get("tool_use_id").and_then(Value::as_str).unwrap_or("").to_string();
                            let content = cb.get("content").cloned().unwrap_or(Value::Null);
                            pending.insert(index, PendingBlock::ServerToolResult { call_id, content });
                            yield Ok(StreamChunk::BlockStart { index, block_type: BlockType::ServerToolResult });
                        }
                        other => {
                            unknown_item_count += 1;
                            if !unknown_item_types.contains(other) {
                                unknown_item_types.insert(other.to_string());
                            }
                            pending.insert(index, PendingBlock::Unknown);
                        }
                    }
                }
                "content_block_delta" => {
                    let Some(index) = event.get("index").and_then(Value::as_u64).map(|i| i as usize)
                    else {
                        malformed_event_count += 1;
                        continue;
                    };
                    let Some(delta) = event.get("delta") else {
                        malformed_event_count += 1;
                        continue;
                    };
                    let Some(delta_type) = delta.get("type").and_then(Value::as_str) else {
                        malformed_event_count += 1;
                        continue;
                    };
                    match delta_type {
                        "text_delta" => {
                            let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                            if !text.is_empty() {
                                text_bytes_total += text.len();
                                first_chunk_at.get_or_insert_with(std::time::Instant::now);
                                if let Some(PendingBlock::Text(buf)) = pending.get_mut(&index) {
                                    buf.push_str(text);
                                }
                                yield Ok(StreamChunk::TextDelta { index, delta: text.to_string() });
                            }
                        }
                        "thinking_delta" => {
                            let text = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                            if !text.is_empty() {
                                reasoning_bytes_total += text.len();
                                first_chunk_at.get_or_insert_with(std::time::Instant::now);
                                if let Some(PendingBlock::Thinking(buf)) = pending.get_mut(&index) {
                                    buf.push_str(text);
                                }
                                yield Ok(StreamChunk::ReasoningDelta { index, delta: text.to_string() });
                            }
                        }
                        "input_json_delta" => {
                            let partial = delta.get("partial_json").and_then(Value::as_str).unwrap_or("");
                            if partial.is_empty() {
                                continue;
                            }
                            let mut client_call_id: Option<String> = None;
                            if let Some(block) = pending.get_mut(&index) {
                                match block {
                                    PendingBlock::ToolUse { call_id, arguments, .. } => {
                                        arguments.push_str(partial);
                                        client_call_id = Some(call_id.clone());
                                    }
                                    PendingBlock::ServerToolUse { input, .. } => input.push_str(partial),
                                    _ => {}
                                }
                            }
                            if let Some(call_id) = client_call_id {
                                yield Ok(StreamChunk::ToolCallDelta {
                                    index,
                                    call_id,
                                    name: None,
                                    arguments: Value::String(partial.to_string()),
                                });
                            }
                        }
                        // thinking 签名不参与回放，忽略。
                        "signature_delta" => {}
                        other => {
                            unknown_event_count += 1;
                            if !unknown_event_types.contains(other) {
                                unknown_event_types.insert(other.to_string());
                            }
                        }
                    }
                }
                "content_block_stop" => {
                    let Some(index) = event.get("index").and_then(Value::as_u64).map(|i| i as usize)
                    else {
                        malformed_event_count += 1;
                        continue;
                    };
                    if let Some(block) = pending.remove(&index).and_then(PendingBlock::into_block) {
                        yield Ok(StreamChunk::BlockEnd { index, block });
                    }
                }
                "message_delta" => {
                    if let Some(fragment) = event.get("usage").filter(|u| u.is_object()) {
                        usage_accum.merge(fragment);
                    }
                    if let Some(reason) = event
                        .get("delta")
                        .and_then(|d| d.get("stop_reason"))
                        .and_then(Value::as_str)
                    {
                        stop_reason = Some(reason.to_string());
                    }
                }
                "message_stop" => {
                    // 语义终止事件：立即收敛，不依赖 SSE EOF。
                    break;
                }
                "ping" => {}
                "error" => {
                    let body = event
                        .get("error")
                        .map(Value::to_string)
                        .unwrap_or_else(|| "anthropic stream error".to_string());
                    tracing::warn!(
                        target: "model_provider::deepseek",
                        request_id = %request_id,
                        model = %model,
                        event_count,
                        "anthropic API returned stream error"
                    );
                    terminated_with_error = true;
                    yield Err(ProviderError::Api { status: 500, body });
                    break;
                }
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

        // ── 流结束：未闭合块收敛（content_block_stop 正常总会到达，这里是安全网）──
        let truncated = matches!(stop_reason.as_deref(), Some("max_tokens"));
        let mut open_indices: Vec<usize> = pending.keys().copied().collect();
        open_indices.sort();
        for index in open_indices {
            let Some(block) = pending.remove(&index) else {
                continue;
            };
            match block {
                PendingBlock::Text(..) | PendingBlock::Thinking(..) | PendingBlock::ServerToolResult { .. } => {
                    if let Some(block) = block.into_block() {
                        yield Ok(StreamChunk::BlockEnd { index, block });
                    }
                }
                // 截断时的半截工具块不回放：与 responses 适配器丢弃语义一致。
                PendingBlock::ToolUse { .. } | PendingBlock::ServerToolUse { .. } if truncated => {}
                other => {
                    if let Some(block) = other.into_block() {
                        yield Ok(StreamChunk::BlockEnd { index, block });
                    }
                }
            }
        }

        let usage = usage_accum.to_usage();
        let reason = stop_reason_to_finish(stop_reason.as_deref());

        tracing::debug!(
            target: "model_provider::deepseek",
            request_id = %request_id,
            model = %model,
            event_count,
            text_bytes = text_bytes_total,
            reasoning_bytes = reasoning_bytes_total,
            tool_call_count,
            web_search_requests = usage_accum.web_search_requests,
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
            "anthropic SSE stream finished"
        );
        if !unknown_event_types.is_empty() || !unknown_item_types.is_empty() {
            tracing::trace!(
                target: "model_provider::deepseek",
                request_id = %request_id,
                unknown_event_types = ?unknown_event_types,
                unknown_item_types = ?unknown_item_types,
                "unhandled event/item types in anthropic stream"
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
impl ModelProvider for DeepSeekAnthropicAdapter {
    fn name(&self) -> &str {
        "deepseek-anthropic"
    }

    async fn generate_full(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateResult, ProviderError> {
        let request_id = logging::next_request_id();
        let endpoint = self.messages_endpoint();
        let span = tracing::info_span!(
            "deepseek_anthropic_generate_full",
            provider = "deepseek-anthropic",
            endpoint = %endpoint,
            model = %request.model,
            request_id = %request_id,
        );

        async move {
            let started = std::time::Instant::now();
            let body = build_request_body(request, false, self.strict_feature_validation)?;
            let input = logging::summarize_input(&request.input);

            tracing::debug!(
                target: "model_provider::deepseek",
                request_id = %request_id,
                model = %request.model,
                endpoint = %endpoint,
                input_items = request.input.len(),
                messages = input.messages,
                function_calls = input.function_calls,
                function_call_outputs = input.function_call_outputs,
                reasoning_items = input.reasoning,
                images = input.images,
                tools = request.tools.len(),
                server_web_search = request
                    .tools
                    .iter()
                    .any(|t| t.name == SERVER_TOOL_WEB_SEARCH),
                instructions_chars = request
                    .instructions
                    .as_deref()
                    .map(|s| s.chars().count())
                    .unwrap_or(0),
                body_bytes = body.len(),
                stream = false,
                "sending anthropic generate request"
            );
            tracing::trace!(
                target: "model_provider::deepseek",
                request_id = %request_id,
                body = %logging::truncate_data_uris(&String::from_utf8_lossy(&body)),
                "anthropic request body (full)"
            );

            let response = self
                .http_client
                .post(&endpoint)
                .header("Authorization", self.auth_header())
                .header("Content-Type", "application/json")
                .header("anthropic-version", ANTHROPIC_VERSION)
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
                tracing::warn!(
                    target: "model_provider::deepseek",
                    request_id = %request_id,
                    provider_request_id = provider_request_id.as_deref().unwrap_or("-"),
                    status = status.as_u16(),
                    latency_ms,
                    body = %body_str,
                    "deepseek anthropic API returned error status"
                );
                return Err(ProviderError::Api {
                    status: status.as_u16(),
                    body: body_str,
                });
            }

            tracing::trace!(
                target: "model_provider::deepseek",
                request_id = %request_id,
                body = %String::from_utf8_lossy(&response_body),
                "anthropic response body (full)"
            );

            let result = parse_non_stream(&response_body, &request_id)?;
            tracing::debug!(
                target: "model_provider::deepseek",
                request_id = %request_id,
                provider_request_id = provider_request_id.as_deref().unwrap_or("-"),
                latency_ms,
                status = ?result.status,
                blocks = result.output.len(),
                input_tokens = result.usage.input_tokens,
                output_tokens = result.usage.output_tokens,
                total_tokens = result.usage.total_tokens,
                "anthropic generate done"
            );
            Ok(result)
        }
        .instrument(span)
        .await
    }

    async fn generate_stream(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateStream, ProviderError> {
        let body = build_request_body(request, true, self.strict_feature_validation)?;
        let endpoint = self.messages_endpoint();
        let model = request.model.clone();
        let request_id = logging::next_request_id();
        let input = logging::summarize_input(&request.input);

        tracing::debug!(
            target: "model_provider::deepseek",
            request_id = %request_id,
            model = %model,
            endpoint = %endpoint,
            input_items = request.input.len(),
            messages = input.messages,
            function_calls = input.function_calls,
            function_call_outputs = input.function_call_outputs,
            reasoning_items = input.reasoning,
            images = input.images,
            tools = request.tools.len(),
            server_web_search = request
                .tools
                .iter()
                .any(|t| t.name == SERVER_TOOL_WEB_SEARCH),
            instructions_chars = request
                .instructions
                .as_deref()
                .map(|s| s.chars().count())
                .unwrap_or(0),
            body_bytes = body.len(),
            stream = true,
            "sending anthropic streaming generate request"
        );
        tracing::trace!(
            target: "model_provider::deepseek",
            request_id = %request_id,
            body = %logging::truncate_data_uris(&String::from_utf8_lossy(&body)),
            "anthropic streaming request body (full)"
        );

        let span = tracing::info_span!(
            "deepseek_anthropic_stream",
            provider = "deepseek-anthropic",
            endpoint = %endpoint,
            model = %model,
            request_id = %request_id,
        );

        let event_source = StreamingEventSource::with_extra_headers(
            self.http_client.clone(),
            endpoint.clone(),
            body,
            self.auth_header(),
            vec![(
                http::HeaderName::from_static("anthropic-version"),
                http::HeaderValue::from_static(ANTHROPIC_VERSION),
            )],
        );

        Ok(process_anthropic_sse_stream(
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
    use crate::response::{Content, ContentPart, ReasoningConfig};

    fn make_request() -> GenerateRequest {
        GenerateRequest {
            model: "deepseek-v4-flash".to_string(),
            instructions: Some("你是一个助手".to_string()),
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

    fn build_body(request: &GenerateRequest, stream: bool) -> Value {
        let body = build_request_body(request, stream, false).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    // ── 端点 ──

    #[test]
    fn test_endpoint_strips_v1_suffix() {
        let client = DeepSeekAnthropicAdapter::new("sk-test")
            .unwrap()
            .with_base_url("https://api.deepseek.com/v1");
        assert_eq!(
            client.messages_endpoint(),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
    }

    #[test]
    fn test_endpoint_keeps_base_without_v1() {
        let client = DeepSeekAnthropicAdapter::new("sk-test")
            .unwrap()
            .with_base_url("http://localhost:3000");
        assert_eq!(
            client.messages_endpoint(),
            "http://localhost:3000/anthropic/v1/messages"
        );
    }

    #[test]
    fn test_name_reports_deepseek_anthropic() {
        let client = DeepSeekAnthropicAdapter::new("sk-test").unwrap();
        assert_eq!(client.name(), "deepseek-anthropic");
    }

    // ── 请求体 ──

    #[test]
    fn test_build_request_body_basic() {
        let json = build_body(&make_request(), false);
        assert_eq!(json["model"], "deepseek-v4-flash");
        assert_eq!(json["system"], "你是一个助手");
        assert!(json.get("stream").is_none());
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "你好");
    }

    #[test]
    fn test_system_hoists_history_after_instructions() {
        let mut request = make_request();
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::System,
                content: "历史系统提示".into(),
            }),
            Arc::new(InputItem::Message {
                role: Role::Developer,
                content: "开发者提示".into(),
            }),
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "hi".into(),
            }),
        ]);
        let json = build_body(&request, false);
        // instructions 前置，历史 System/Developer 依次追加，\n\n 连接。
        assert_eq!(json["system"], "你是一个助手\n\n历史系统提示\n\n开发者提示");
        assert_eq!(json["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_grouping_reasoning_text_and_tool_call_into_one_assistant_message() {
        let mut request = make_request();
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "北京天气".into(),
            }),
            Arc::new(InputItem::Reasoning {
                content: "先查天气".into(),
            }),
            Arc::new(InputItem::Message {
                role: Role::Assistant,
                content: "我来查询".into(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "call_1".to_string(),
                name: "get_weather".to_string(),
                arguments: r#"{"city":"beijing"}"#.to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "call_1".to_string(),
                output: "晴 25C".into(),
            }),
        ]);
        let json = build_body(&request, false);
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);

        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "北京天气");

        // 收束为单条 assistant：thinking → text → tool_use。
        assert_eq!(messages[1]["role"], "assistant");
        let content = messages[1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "先查天气");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "我来查询");
        assert_eq!(content[2]["type"], "tool_use");
        assert_eq!(content[2]["id"], "call_1");
        assert_eq!(content[2]["name"], "get_weather");
        assert_eq!(content[2]["input"]["city"], "beijing");

        assert_eq!(messages[2]["role"], "user");
        let results = messages[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["type"], "tool_result");
        assert_eq!(results[0]["tool_use_id"], "call_1");
        assert_eq!(results[0]["content"], "晴 25C");
    }

    #[test]
    fn test_consecutive_tool_outputs_merge_into_single_user_message() {
        let mut request = make_request();
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "查两个城市".into(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "f".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "c2".to_string(),
                name: "f".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: "r1".into(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c2".to_string(),
                output: "r2".into(),
            }),
        ]);
        let json = build_body(&request, false);
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);

        // assistant 一条含两个 tool_use。
        let content = messages[1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["id"], "c1");
        assert_eq!(content[1]["id"], "c2");

        // 两个结果合并为一条 user（角色交替约束）。
        assert_eq!(messages[2]["role"], "user");
        let results = messages[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["tool_use_id"], "c1");
        assert_eq!(results[1]["tool_use_id"], "c2");
    }

    #[test]
    fn test_consecutive_user_messages_merge() {
        let mut request = make_request();
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "第一条".into(),
            }),
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "第二条".into(),
            }),
        ]);
        let json = build_body(&request, false);
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        let blocks = messages[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["text"], "第一条");
        assert_eq!(blocks[1]["text"], "第二条");
    }

    #[test]
    fn test_unpaired_tool_call_and_orphan_output_dropped() {
        let mut request = make_request();
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "hi".into(),
            }),
            // 无 output 的 tool_use：整块丢弃，assistant 消息不再产出。
            Arc::new(InputItem::FunctionCall {
                call_id: "lost".to_string(),
                name: "f".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "第二句".into(),
            }),
        ]);
        let json = build_body(&request, false);
        let messages = json["messages"].as_array().unwrap();
        // 两条 user 合并为一条；无 tool_use 消息。
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");

        // 孤儿 output：无配对 tool_use → 丢弃。
        let mut request = make_request();
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "hi".into(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "orphan".to_string(),
                output: "r".into(),
            }),
        ]);
        let json = build_body(&request, false);
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "hi");
    }

    #[test]
    fn test_tools_partition_marker_vs_client_functions() {
        let mut request = make_request();
        request.tools = vec![
            crate::ToolDefinition {
                name: SERVER_TOOL_WEB_SEARCH.to_string(),
                description: "marker".to_string(),
                parameters: serde_json::json!({}),
            },
            crate::ToolDefinition {
                name: "get_time".to_string(),
                description: "获取时间".to_string(),
                parameters: serde_json::json!({ "type": "object" }),
            },
        ];
        let json = build_body(&request, false);
        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["type"], "web_search_20250305");
        assert_eq!(tools[0]["name"], "web_search");
        assert_eq!(tools[0]["max_uses"], 5);
        assert_eq!(tools[1]["name"], "get_time");
        assert_eq!(tools[1]["description"], "获取时间");
        assert_eq!(tools[1]["input_schema"]["type"], "object");
        // 标记名绝不出现在线格式里。
        assert!(!json.to_string().contains(SERVER_TOOL_WEB_SEARCH));
    }

    #[test]
    fn test_empty_tools_omitted() {
        let json = build_body(&make_request(), false);
        assert!(json.get("tools").is_none());
    }

    #[test]
    fn test_thinking_mapping() {
        let mut request = make_request();
        request.reasoning = Some(ReasoningConfig {
            enabled: true,
            effort: None,
        });
        let json = build_body(&request, false);
        assert_eq!(json["thinking"]["type"], "enabled");

        request.reasoning = Some(ReasoningConfig {
            enabled: false,
            effort: None,
        });
        let json = build_body(&request, false);
        assert_eq!(json["thinking"]["type"], "disabled");

        request.reasoning = None;
        let json = build_body(&request, false);
        assert!(json.get("thinking").is_none());
    }

    #[test]
    fn test_sampling_and_stream_flags() {
        let mut request = make_request();
        request.temperature = Some(0.7);
        request.top_p = Some(0.9);
        request.max_output_tokens = Some(16384);
        let json = build_body(&request, true);
        assert_eq!(json["temperature"], 0.7);
        assert_eq!(json["top_p"], 0.9);
        assert_eq!(json["max_tokens"], 16384);
        assert_eq!(json["stream"], true);
    }

    #[test]
    fn test_invalid_tool_arguments_fallback_to_object() {
        let mut request = make_request();
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "hi".into(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "f".to_string(),
                arguments: "not json".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: "r".into(),
            }),
        ]);
        let json = build_body(&request, false);
        let messages = json["messages"].as_array().unwrap();
        let content = messages[1]["content"].as_array().unwrap();
        assert_eq!(content[0]["input"], serde_json::json!({}));

        // 非对象 JSON（数组）同样回退 {}（协议要求 input 为对象）。
        let mut request = make_request();
        request.input = Arc::from([
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "hi".into(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "f".to_string(),
                arguments: "[1,2]".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: "r".into(),
            }),
        ]);
        let json = build_body(&request, false);
        let messages = json["messages"].as_array().unwrap();
        let content = messages[1]["content"].as_array().unwrap();
        assert_eq!(content[0]["input"], serde_json::json!({}));
    }

    #[test]
    fn test_image_parts_stripped_leniently() {
        let mut request = make_request();
        request.input = Arc::from([Arc::new(InputItem::Message {
            role: Role::User,
            content: Content::Parts(vec![
                ContentPart::Text {
                    text: "看这张图".to_string(),
                },
                ContentPart::Image {
                    url: "https://example.com/a.png".to_string(),
                    detail: None,
                },
            ]),
        })]);
        let json = build_body(&request, false);
        assert_eq!(json["messages"][0]["content"], "看这张图");

        // 严格模式直接报错。
        assert!(build_request_body(&request, false, true).is_err());
    }

    // ── 非流式解析 ──

    fn parse(body: &str) -> GenerateResult {
        parse_non_stream(body.as_bytes(), "req-test").unwrap()
    }

    #[test]
    fn test_parse_maps_all_block_types() {
        let result = parse(
            r#"{
                "id": "msg_1",
                "content": [
                    {"type":"thinking","thinking":"先想","signature":"sig"},
                    {"type":"text","text":"答案"},
                    {"type":"tool_use","id":"call_1","name":"f","input":{"a":1}},
                    {"type":"server_tool_use","id":"srv_1","name":"web_search","input":{"query":"rust"},"caller":{"type":"direct"}},
                    {"type":"web_search_tool_result","tool_use_id":"srv_1","content":[{"type":"web_search_result","title":"T","url":"https://e.com"}]},
                    {"type":"redacted_thinking","data":"opaque"}
                ],
                "stop_reason": "end_turn",
                "usage": {"input_tokens":10,"output_tokens":5}
            }"#,
        );
        assert_eq!(result.output.len(), 5);
        assert!(matches!(&result.output[0], ContentBlock::Reasoning { text } if text == "先想"));
        assert!(matches!(&result.output[1], ContentBlock::Text { text } if text == "答案"));
        assert!(
            matches!(&result.output[2], ContentBlock::ToolCall { call_id, name, arguments }
            if call_id == "call_1" && name == "f" && arguments == r#"{"a":1}"#)
        );
        assert!(
            matches!(&result.output[3], ContentBlock::ServerToolUse { call_id, name, input }
            if call_id == "srv_1" && name == "web_search" && input.contains("\"query\":\"rust\""))
        );
        assert!(
            matches!(&result.output[4], ContentBlock::ServerToolResult { call_id, name, content }
            if call_id == "srv_1" && name == "web_search" && content.contains("web_search_result"))
        );

        assert_eq!(result.status, ResponseStatus::Completed);
        assert_eq!(result.finish_reason, Some(FinishReason::Stop));
        assert_eq!(
            result.usage,
            Usage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            }
        );
    }

    #[test]
    fn test_parse_usage_sums_cache_tokens() {
        let result = parse(
            r#"{
                "id":"msg_1","content":[{"type":"text","text":"hi"}],
                "stop_reason":"end_turn",
                "usage":{"input_tokens":10,"output_tokens":3,
                         "cache_creation_input_tokens":2,"cache_read_input_tokens":4,
                         "server_tool_use":{"web_search_requests":2}}
            }"#,
        );
        // input 并入缓存段，与其它适配器「全量提示词」口径一致。
        assert_eq!(result.usage.input_tokens, 16);
        assert_eq!(result.usage.total_tokens, 19);
    }

    #[test]
    fn test_parse_stop_reason_table() {
        let cases = [
            (
                "end_turn",
                ResponseStatus::Completed,
                Some(FinishReason::Stop),
            ),
            (
                "stop_sequence",
                ResponseStatus::Completed,
                Some(FinishReason::Stop),
            ),
            (
                "tool_use",
                ResponseStatus::Completed,
                Some(FinishReason::ToolCalls),
            ),
            (
                "max_tokens",
                ResponseStatus::Incomplete,
                Some(FinishReason::MaxTokens),
            ),
            ("refusal", ResponseStatus::Failed, Some(FinishReason::Error)),
        ];
        for (stop, status, finish) in cases {
            let body = format!(
                r#"{{"id":"m","content":[],"stop_reason":"{stop}","usage":{{"input_tokens":1,"output_tokens":1}}}}"#
            );
            let result = parse(&body);
            assert_eq!(result.status, status, "stop_reason={stop}");
            assert_eq!(result.finish_reason, finish, "stop_reason={stop}");
        }
        // 未知 stop_reason → Failed + 携带失败文案。
        let result = parse(
            r#"{"id":"m","content":[],"stop_reason":"weird","usage":{"input_tokens":1,"output_tokens":1}}"#,
        );
        assert_eq!(result.status, ResponseStatus::Failed);
        let err = result.error.expect("未知 stop_reason 应携带错误");
        assert!(err.message.contains("weird"));
    }

    #[test]
    fn test_parse_top_level_error_maps_to_api_error() {
        let err = parse_non_stream(
            br#"{"error":{"type":"invalid_request_error","message":"bad model"}}"#,
            "req-test",
        )
        .unwrap_err();
        assert!(matches!(err, ProviderError::Api { status: 500, .. }));
    }

    // ── 流式（本地一次性 SSE 服务） ──

    fn sse_events(datas: &[&str]) -> String {
        datas
            .iter()
            .map(|d| format!("data: {d}\n\n"))
            .collect::<String>()
    }

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

    async fn collect_stream(
        client: &DeepSeekAnthropicAdapter,
        request: &GenerateRequest,
    ) -> Vec<Result<StreamChunk, ProviderError>> {
        let mut stream = client.generate_stream(request).await.unwrap();
        let mut chunks = Vec::new();
        while let Some(chunk) = stream.next().await {
            chunks.push(chunk);
        }
        chunks
    }

    /// 全事件链：thinking → text → message_delta(stop_reason) → message_stop 收敛。
    #[tokio::test]
    async fn test_stream_text_and_reasoning_assembly() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"message_start","message":{"usage":{"input_tokens":6}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"先想"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"一下"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"你好"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}"#,
            r#"{"type":"message_stop"}"#,
        ]))
        .await;
        let client = DeepSeekAnthropicAdapter::new("sk-test")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(
            matches!(
                &chunks[..],
                [
                    Ok(StreamChunk::BlockStart { index: 0, block_type: BlockType::Reasoning }),
                    Ok(StreamChunk::ReasoningDelta { index: 0, delta }),
                    Ok(StreamChunk::ReasoningDelta { index: 0, .. }),
                    Ok(StreamChunk::BlockEnd { index: 0, block: ContentBlock::Reasoning { text: thinking } }),
                    Ok(StreamChunk::BlockStart { index: 1, block_type: BlockType::Text }),
                    Ok(StreamChunk::TextDelta { index: 1, delta: text_delta }),
                    Ok(StreamChunk::BlockEnd { index: 1, block: ContentBlock::Text { text } }),
                    Ok(StreamChunk::Usage { usage }),
                    Ok(StreamChunk::Finish { reason: FinishReason::Stop }),
                ]
                if delta == "先想"
                    && thinking == "先想一下"
                    && text == "你好"
                    && text_delta == "你好"
                    && usage.input_tokens == 6
                    && usage.output_tokens == 4
                    && usage.total_tokens == 10
            ),
            "unexpected chunks: {chunks:#?}"
        );
    }

    /// 客户端 tool_use：BlockStart + 名称公告 delta + 参数分片 delta + 完整 BlockEnd。
    #[tokio::test]
    async fn test_stream_client_tool_use_emits_tool_call_deltas() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"message_start","message":{"usage":{"input_tokens":8}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_1","name":"get_weather"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"city\":"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"bj\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}"#,
            r#"{"type":"message_stop"}"#,
        ]))
        .await;
        let client = DeepSeekAnthropicAdapter::new("sk-test")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(
            matches!(
                &chunks[..],
                [
                    Ok(StreamChunk::BlockStart { index: 0, block_type: BlockType::ToolCall }),
                    Ok(StreamChunk::ToolCallDelta { call_id, name: Some(name), arguments, .. }),
                    Ok(StreamChunk::ToolCallDelta { name: None, arguments: arg1, .. }),
                    Ok(StreamChunk::ToolCallDelta { name: None, arguments: arg2, .. }),
                    Ok(StreamChunk::BlockEnd { block: ContentBlock::ToolCall { call_id: end_id, arguments: end_args, .. }, .. }),
                    Ok(StreamChunk::Usage { .. }),
                    Ok(StreamChunk::Finish { reason: FinishReason::ToolCalls }),
                ]
                if call_id == "call_1"
                    && name == "get_weather"
                    && arguments == &Value::String(String::new())
                    && arg1 == &Value::String("{\"city\":".to_string())
                    && arg2 == &Value::String("\"bj\"}".to_string())
                    && end_id == "call_1"
                    && end_args == r#"{"city":"bj"}"#
            ),
            "unexpected chunks: {chunks:#?}"
        );
    }

    /// 服务端工具块：经 BlockStart/BlockEnd 承载，绝不发 ToolCallDelta（不进客户端执行循环）。
    #[tokio::test]
    async fn test_stream_server_tool_blocks_never_emit_tool_call_delta() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"message_start","message":{"usage":{"input_tokens":8,"server_tool_use":{"web_search_requests":1}}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srv_1","name":"web_search"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"rust\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"web_search_tool_result","tool_use_id":"srv_1","content":[{"type":"web_search_result","title":"T","url":"https://e.com"}]}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"答案"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            r#"{"type":"message_stop"}"#,
        ]))
        .await;
        let client = DeepSeekAnthropicAdapter::new("sk-test")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        // 服务端输入增量只累积，无任何 ToolCallDelta。
        assert!(
            !chunks
                .iter()
                .any(|c| matches!(c, Ok(StreamChunk::ToolCallDelta { .. }))),
            "server tool must not emit ToolCallDelta: {chunks:#?}"
        );
        assert!(
            matches!(
                &chunks[..],
                [
                    Ok(StreamChunk::BlockStart { index: 0, block_type: BlockType::ServerToolUse }),
                    Ok(StreamChunk::BlockEnd { index: 0, block: ContentBlock::ServerToolUse { call_id, name, input } }),
                    Ok(StreamChunk::BlockStart { index: 1, block_type: BlockType::ServerToolResult }),
                    Ok(StreamChunk::BlockEnd { index: 1, block: ContentBlock::ServerToolResult { call_id: rid, name: rname, .. } }),
                    Ok(StreamChunk::BlockStart { index: 2, block_type: BlockType::Text }),
                    Ok(StreamChunk::TextDelta { index: 2, delta }),
                    Ok(StreamChunk::BlockEnd { index: 2, block: ContentBlock::Text { text } }),
                    Ok(StreamChunk::Usage { usage }),
                    Ok(StreamChunk::Finish { reason: FinishReason::Stop }),
                ]
                if call_id == "srv_1"
                    && name == "web_search"
                    && input.contains("\"query\":\"rust\"")
                    && rid == "srv_1"
                    && rname == "web_search"
                    && delta == "答案"
                    && text == "答案"
                    && usage.input_tokens == 8
            ),
            "unexpected chunks: {chunks:#?}"
        );
    }

    /// 事件类型回退：data 无 `type` 字段时取 SSE 具名事件（DeepSeek 实测形态）。
    #[tokio::test]
    async fn test_stream_event_name_fallback_when_data_lacks_type() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"message\":{\"usage\":{\"input_tokens\":2}}}\n\n",
            "event: content_block_start\n",
            "data: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
            "event: message_stop\n",
            "data: {}\n\n",
        );
        let base = spawn_sse_server(body.to_string()).await;
        let client = DeepSeekAnthropicAdapter::new("sk-test")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(
            matches!(
                &chunks[..],
                [
                    Ok(StreamChunk::BlockStart { index: 0, block_type: BlockType::Text }),
                    Ok(StreamChunk::TextDelta { delta, .. }),
                    Ok(StreamChunk::BlockEnd { block: ContentBlock::Text { text }, .. }),
                    Ok(StreamChunk::Usage { usage }),
                    Ok(StreamChunk::Finish { reason: FinishReason::Stop }),
                ]
                if delta == "hi" && text == "hi" && usage.input_tokens == 2 && usage.output_tokens == 1
            ),
            "unexpected chunks: {chunks:#?}"
        );
    }

    /// EOF 安全网：缺 message_stop / content_block_stop 时未闭合文本块仍收束。
    #[tokio::test]
    async fn test_stream_eof_safety_net_closes_unclosed_text_block() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"message_start","message":{"usage":{"input_tokens":3}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"半截"}}"#,
        ]))
        .await;
        let client = DeepSeekAnthropicAdapter::new("sk-test")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(
            matches!(
                &chunks[..],
                [
                    Ok(StreamChunk::BlockStart { .. }),
                    Ok(StreamChunk::TextDelta { delta, .. }),
                    Ok(StreamChunk::BlockEnd { block: ContentBlock::Text { text }, .. }),
                    Ok(StreamChunk::Usage { .. }),
                    Ok(StreamChunk::Finish { reason: FinishReason::Stop }),
                ]
                if delta == "半截" && text == "半截"
            ),
            "unexpected chunks: {chunks:#?}"
        );
    }

    /// 截断（max_tokens）时未闭合工具块不回放 —— 与 responses 适配器丢弃语义一致。
    #[tokio::test]
    async fn test_stream_max_tokens_drops_unclosed_tool_block() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"message_start","message":{"usage":{"input_tokens":3}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"c1","name":"f"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":100}}"#,
            r#"{"type":"message_stop"}"#,
        ]))
        .await;
        let client = DeepSeekAnthropicAdapter::new("sk-test")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;

        assert!(
            !chunks
                .iter()
                .any(|c| matches!(c, Ok(StreamChunk::BlockEnd { .. }))),
            "unclosed tool block must be dropped on max_tokens: {chunks:#?}"
        );
        assert!(matches!(
            chunks.last(),
            Some(Ok(StreamChunk::Finish {
                reason: FinishReason::MaxTokens
            }))
        ));
    }

    /// ping 忽略；error 事件上抛为 Err 且不产出 Usage/Finish。
    #[tokio::test]
    async fn test_stream_ping_ignored_and_error_surfaces() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#,
            r#"{"type":"ping"}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
            r#"{"type":"message_stop"}"#,
        ]))
        .await;
        let client = DeepSeekAnthropicAdapter::new("sk-test")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;
        // ping 不产生任何 chunk：BlockStart + TextDelta + BlockEnd + Usage + Finish = 5。
        assert_eq!(chunks.len(), 5);

        // error 事件 → Err，随后终止（无 Usage/Finish）。
        let base = spawn_sse_server(sse_events(&[
            r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"overloaded"}}"#,
        ]))
        .await;
        let client = DeepSeekAnthropicAdapter::new("sk-test")
            .unwrap()
            .with_base_url(base);
        let chunks = collect_stream(&client, &make_request()).await;
        assert_eq!(chunks.len(), 1);
        assert!(matches!(
            &chunks[0],
            Err(ProviderError::Api { status: 500, body }) if body.contains("overloaded")
        ));
    }
}
