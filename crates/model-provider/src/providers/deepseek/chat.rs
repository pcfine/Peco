//! DeepSeek 模型提供商实现。
//!
//! 提供 [`DeepSeek`]，为 [DeepSeek API](https://api.deepseek.com) 实现
//! [`ModelProvider`]，使用与 OpenAI 兼容的聊天补全协议。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;

use crate::logging;
use crate::providers::chat_common::{WireMessage, input_items_to_wire_messages};
use crate::response::{
    ContentBlock, GenerateRequest, GenerateResult, InputItem, ReasoningConfig, ReasoningEffort,
    ResponseError, ResponseStatus, Role, TextFormat,
};
use crate::streaming::pipeline::{
    NormalizedChunk, NormalizedToolCall, NormalizedUsage, StreamingProfile,
    process_normalized_sse_stream_chunks,
};
use crate::streaming::sse::StreamingEventSource;
use crate::{GenerateStream, ModelProvider, ProviderError, ToolCall, ToolDefinition, Usage};

// ============================================================================
// DeepSeek 客户端
// ============================================================================

const DEEPSEEK_API_BASE_URL: &str = "https://api.deepseek.com";

/// DeepSeek API 客户端，实现 [`ModelProvider`]。
///
/// # 示例
///
/// ```ignore
/// use model_provider::{DeepSeek, ModelProvider};
///
/// let provider = DeepSeek::from_env()?;
/// assert_eq!(provider.name(), "deepseek");
/// ```
pub struct DeepSeek {
    http_client: reqwest::Client,
    api_key: String,
    base_url: String,
    /// 严格特性校验：`false` 时静默丢弃（`debug!`）chat 适配器无法承载的特性
    /// （`text.format=json_schema`、`Role::Developer`），`true` 时返回 [`ProviderError::Request`]。
    strict_feature_validation: bool,
}

/// chat completions 适配器的语义别名（保留 `DeepSeek` 名以避免涟漪）。
pub type DeepSeekChatCompletionsAdapter = DeepSeek;

impl DeepSeek {
    /// 使用给定的 API 密钥创建新的 DeepSeek 客户端。
    ///
    /// 使用默认的基础 URL（`https://api.deepseek.com`）。
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

    /// 通过读取 `DEEPSEEK_API_KEY` 环境变量创建新的 DeepSeek 客户端。
    ///
    /// 如果环境变量未设置，返回错误。
    pub fn from_env() -> Result<Self, ProviderError> {
        let api_key = std::env::var("DEEPSEEK_API_KEY")
            .map_err(|_| ProviderError::Request("DEEPSEEK_API_KEY 环境变量未设置".to_string()))?;
        Self::new(api_key)
    }

    /// 为 API 设置自定义的基础 URL。
    ///
    /// 适用于代理或自部署的 DeepSeek 兼容端点。
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 设置严格特性校验开关（默认 `false`）。
    pub fn strict_feature_validation(mut self, strict: bool) -> Self {
        self.strict_feature_validation = strict;
        self
    }

    /// 返回聊天补全的端点 URL。
    fn chat_endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// 构建 Authorization 头部的值。
    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_key)
    }
}

// ============================================================================
// 内部 API 序列化类型
// ============================================================================

/// 将 ToolDefinition 包装为 OpenAI/DeepSeek 的传输格式。
#[derive(Serialize)]
struct ApiToolDef<'a> {
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: &'a ToolDefinition,
}

/// DeepSeek API 请求体（同时用于流式和非流式）。
#[derive(Serialize)]
struct DeepSeekRequest<'a> {
    model: &'a str,
    messages: &'a [WireMessage<'a>],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ApiToolDef<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
    /// DeepSeek thinking 配置，由 provider 从 `GenerateRequest::reasoning` 构建。
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Value>,
    /// 将额外参数扁平化合并到请求体中（透传 `GenerateRequest::additional_params`）。
    #[serde(flatten)]
    extra: Option<&'a Value>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// DeepSeek API 非流式响应。
#[derive(Deserialize)]
struct DeepSeekResponse {
    #[serde(default)]
    id: String,
    choices: Vec<DeepSeekChoice>,
    usage: Option<DeepSeekApiUsage>,
}

#[derive(Deserialize)]
struct DeepSeekChoice {
    message: DeepSeekApiMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct DeepSeekApiMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

/// DeepSeek API 用量信息。
#[derive(Deserialize)]
struct DeepSeekApiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

// ============================================================================
// 内部 SSE 流式反序列化类型
// ============================================================================

/// DeepSeek 流式 API 中的单个 SSE 数据块。
#[derive(Deserialize)]
struct ChatSseChunk {
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<DeepSeekApiUsage>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<StreamToolCallDelta>>,
}

#[derive(Deserialize)]
struct StreamToolCallDelta {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamToolFunctionDelta>,
}

#[derive(Deserialize)]
struct StreamToolFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 将 DeepSeek API 用量转换为我们的 Usage 类型。
fn convert_usage(api_usage: DeepSeekApiUsage) -> Usage {
    Usage {
        input_tokens: api_usage.prompt_tokens,
        output_tokens: api_usage.completion_tokens,
        total_tokens: api_usage.total_tokens,
    }
}

/// 构建请求体并序列化为 JSON 字节。
fn build_request_body(
    request: &GenerateRequest,
    stream: bool,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut messages: Vec<WireMessage> = Vec::new();
    if let Some(instructions) = &request.instructions {
        messages.push(WireMessage::System {
            content: instructions,
        });
    }
    messages.extend(input_items_to_wire_messages(&request.input));

    let tools: Vec<ApiToolDef> = request
        .tools
        .iter()
        .map(|t| ApiToolDef {
            tool_type: "function",
            function: t,
        })
        .collect();

    let stream_options = if stream {
        Some(StreamOptions {
            include_usage: true,
        })
    } else {
        None
    };

    // - "disabled" / "none" → 显式禁用 thinking
    // - 其它等级：low, high, max
    // - 未配置 → 默认启用 thinking（rely on DeepSeek 默认行为）
    let reasoning_effort = reasoning_config_to_effort(request.reasoning.as_ref());
    let thinking = match reasoning_effort {
        Some(effort) if !effort.is_empty() => {
            let effort_lower = effort.to_lowercase();
            if effort_lower == "disabled" || effort_lower == "none" {
                Some(serde_json::json!({"type": "disabled"}))
            } else {
                Some(serde_json::json!({"type": "enabled", "effort": effort_lower}))
            }
        }
        _ => {
            // DeepSeek 默认启用 thinking
            Some(serde_json::json!({"type": "enabled", "effort": "high"}))
        }
    };

    let api_request = DeepSeekRequest {
        model: &request.model,
        messages: &messages,
        tools,
        temperature: request.temperature,
        max_tokens: request.max_output_tokens,
        stream: if stream { Some(true) } else { None },
        stream_options,
        tool_choice: None,
        thinking,
        extra: request.additional_params.as_ref(),
    };

    serde_json::to_vec(&api_request)
}

/// 将中立 [`ReasoningConfig`] 映射为 chat 协议的 `reasoning_effort` 字符串。
fn reasoning_config_to_effort(reasoning: Option<&ReasoningConfig>) -> Option<String> {
    let config = reasoning?;
    if !config.enabled {
        return Some("disabled".to_string());
    }
    config
        .effort
        .map(|e| match e {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::Max => "max",
        })
        .map(str::to_string)
}

/// 将 chat 协议的 `finish_reason` 映射为中立 [`ResponseStatus`]。
fn finish_reason_to_status(reason: Option<&str>) -> ResponseStatus {
    match reason {
        Some("stop") | Some("tool_calls") | None => ResponseStatus::Completed,
        Some("length") => ResponseStatus::Incomplete,
        _ => ResponseStatus::Failed,
    }
}

/// 将 DeepSeek chat 响应转换为中立 [`GenerateResult`]。
///
/// 块顺序（与流式合成一致）：`content`→`Text`、`reasoning_content`→`Reasoning`、
/// `tool_calls`→`ToolCall`。
fn chat_response_to_generate_result(
    api_response: DeepSeekResponse,
) -> Result<GenerateResult, ProviderError> {
    let choice = api_response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| ProviderError::Response("响应中不包含任何选项".to_string()))?;

    let finish_reason = choice.finish_reason;
    let status = finish_reason_to_status(finish_reason.as_deref());

    let mut output = Vec::new();
    if let Some(content) = choice.message.content {
        output.push(ContentBlock::Text { text: content });
    }
    if let Some(reasoning) = choice.message.reasoning_content {
        output.push(ContentBlock::Reasoning { text: reasoning });
    }
    if let Some(tool_calls) = choice.message.tool_calls {
        for tc in tool_calls {
            output.push(ContentBlock::ToolCall {
                call_id: tc.id,
                name: tc.function.name,
                arguments: tc.function.arguments,
            });
        }
    }

    let error = if status == ResponseStatus::Failed {
        Some(ResponseError {
            code: None,
            message: finish_reason.unwrap_or_else(|| "unknown".to_string()),
        })
    } else {
        None
    };

    Ok(GenerateResult {
        id: api_response.id,
        output,
        usage: api_response.usage.map(convert_usage).unwrap_or_default(),
        status,
        error,
    })
}

// ============================================================================
// ModelProvider 实现
// ============================================================================

#[async_trait]
impl ModelProvider for DeepSeek {
    fn name(&self) -> &str {
        "deepseek"
    }

    async fn generate_full(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateResult, ProviderError> {
        let request_id = logging::next_request_id();
        let endpoint = self.chat_endpoint();
        let span = tracing::info_span!(
            "deepseek_chat_generate_full",
            provider = "deepseek",
            endpoint = %endpoint,
            model = %request.model,
            request_id = %request_id,
        );

        async move {
            self.validate_generate_request(request)?;

            let started = std::time::Instant::now();
            let body = build_request_body(request, false)?;
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
                tools = request.tools.len(),
                instructions_chars = request
                    .instructions
                    .as_deref()
                    .map(|s| s.chars().count())
                    .unwrap_or(0),
                body_bytes = body.len(),
                stream = false,
                "发送 chat 生成请求"
            );
            // 含用户对话原文，仅 trace 级别输出。`body` 随后被 move 进请求，故在此之前取。
            tracing::trace!(
                target: "model_provider::deepseek",
                request_id = %request_id,
                body = %String::from_utf8_lossy(&body),
                "chat 请求体全文"
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
                tracing::warn!(
                    target: "model_provider::deepseek",
                    request_id = %request_id,
                    provider_request_id = provider_request_id.as_deref().unwrap_or("-"),
                    status = status.as_u16(),
                    latency_ms,
                    body = %body_str,
                    "DeepSeek API 返回错误状态"
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
                "chat 响应体全文"
            );

            let api_response: DeepSeekResponse = serde_json::from_slice(&response_body)?;
            let result = chat_response_to_generate_result(api_response)?;

            let blocks = logging::summarize_blocks(&result.output);
            tracing::debug!(
                target: "model_provider::deepseek",
                request_id = %request_id,
                provider_request_id = provider_request_id.as_deref().unwrap_or("-"),
                latency_ms,
                status = ?result.status,
                blocks = result.output.len(),
                text_blocks = blocks.text,
                reasoning_blocks = blocks.reasoning,
                tool_call_blocks = blocks.tool_calls,
                input_tokens = result.usage.input_tokens,
                output_tokens = result.usage.output_tokens,
                total_tokens = result.usage.total_tokens,
                "chat 生成完成"
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
        self.validate_generate_request(request)?;

        let body = build_request_body(request, true)?;

        let endpoint = self.chat_endpoint();
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
            tools = request.tools.len(),
            instructions_chars = request
                .instructions
                .as_deref()
                .map(|s| s.chars().count())
                .unwrap_or(0),
            body_bytes = body.len(),
            stream = true,
            "发送 chat 流式生成请求"
        );
        tracing::trace!(
            target: "model_provider::deepseek",
            request_id = %request_id,
            body = %String::from_utf8_lossy(&body),
            "chat 流式请求体全文"
        );

        let span = tracing::info_span!(
            "deepseek_stream_generate",
            provider = "deepseek",
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

        Ok(process_normalized_sse_stream_chunks(
            event_source,
            DeepSeekStreamingProfile,
            span,
            model,
            request_id,
        ))
    }
}

impl DeepSeek {
    /// 校验 chat 适配器无法承载的中立特性。
    ///
    /// 宽松模式（默认）`debug!` + 静默丢弃；严格模式返回 [`ProviderError::Request`]。
    fn validate_generate_request(&self, request: &GenerateRequest) -> Result<(), ProviderError> {
        let has_json_schema = request
            .text
            .as_ref()
            .and_then(|t| t.format.as_ref())
            .is_some_and(|f| matches!(f, TextFormat::JsonSchema { .. }));
        let has_developer = request.input.iter().any(|i| {
            matches!(
                &**i,
                InputItem::Message {
                    role: Role::Developer,
                    ..
                }
            )
        });

        if self.strict_feature_validation {
            if has_json_schema {
                return Err(ProviderError::Request(
                    "chat 适配器不支持 text.format=json_schema".to_string(),
                ));
            }
            if has_developer {
                return Err(ProviderError::Request(
                    "chat 适配器不支持 Developer role".to_string(),
                ));
            }
        } else {
            if has_json_schema {
                tracing::debug!(
                    target: "model_provider::deepseek",
                    "chat 适配器不支持 text.format=json_schema，忽略"
                );
            }
            if has_developer {
                tracing::debug!(
                    target: "model_provider::deepseek",
                    "chat 适配器不支持 Developer role，降级为 system"
                );
            }
        }
        Ok(())
    }
}

// ============================================================================
// DeepSeek 流式配置文件
// ============================================================================

/// DeepSeek SSE 数据块的提供商标定 [`StreamingProfile`]。
struct DeepSeekStreamingProfile;

impl StreamingProfile for DeepSeekStreamingProfile {
    fn normalize_chunk(&self, data: &str) -> Result<Option<NormalizedChunk>, ProviderError> {
        let chunk: ChatSseChunk = serde_json::from_str(data)
            .map_err(|e| ProviderError::Stream(format!("解析 SSE 数据块失败: {e}")))?;

        // 提取第一个选项（聊天补全的标准做法）
        let choice = match chunk.choices.first() {
            Some(c) => c,
            None => return Ok(None),
        };

        let text = choice.delta.content.clone();
        let reasoning = choice.delta.reasoning_content.clone();

        let tool_calls: Vec<NormalizedToolCall> = choice
            .delta
            .tool_calls
            .as_ref()
            .map(|tcs| {
                tcs.iter()
                    .map(|tc| NormalizedToolCall {
                        index: tc.index,
                        id: tc.id.clone(),
                        name: tc.function.as_ref().and_then(|f| f.name.clone()),
                        arguments: tc.function.as_ref().and_then(|f| f.arguments.clone()),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let usage = chunk.usage.map(|u| NormalizedUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        });

        Ok(Some(NormalizedChunk {
            text,
            reasoning,
            tool_calls,
            finish_reason: choice.finish_reason.clone(),
            usage,
        }))
    }

    fn uses_distinct_tool_call_eviction(&self) -> bool {
        true
    }

    fn emits_complete_single_chunk_tool_calls(&self) -> bool {
        true
    }
}

// ============================================================================
// Builder 风格配置
// ============================================================================

impl DeepSeek {
    /// 开始使用自定义配置构建 DeepSeek 客户端。
    pub fn builder() -> DeepSeekBuilder {
        DeepSeekBuilder::default()
    }
}

/// 用于构建 [`DeepSeek`] 客户端的构建器。
#[derive(Default)]
pub struct DeepSeekBuilder {
    api_key: Option<String>,
    base_url: Option<String>,
}

impl DeepSeekBuilder {
    /// 设置 API 密钥。
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// 设置自定义基础 URL。
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// 构建 [`DeepSeek`] 客户端。
    ///
    /// 如果未提供 API 密钥则返回错误。
    pub fn build(self) -> Result<DeepSeek, ProviderError> {
        let api_key = self
            .api_key
            .ok_or_else(|| ProviderError::Request("需要提供 API 密钥".to_string()))?;

        let mut client = DeepSeek::new(api_key)?;
        if let Some(url) = self.base_url {
            client = client.with_base_url(url);
        }
        Ok(client)
    }
}

// ============================================================================
// 公共模型名称常量
// ============================================================================

/// DeepSeek V4 Flash — 快速且经济。
pub const DEEPSEEK_V4_FLASH: &str = "deepseek-v4-flash";

/// DeepSeek V4 Pro — 最强大的模型。
pub const DEEPSEEK_V4_PRO: &str = "deepseek-v4-pro";

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_deepseek_response_deserialization() {
        let json = r#"{
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello, how can I help?"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 15,
                "completion_tokens": 7,
                "total_tokens": 22
            }
        }"#;
        let response: DeepSeekResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.choices.len(), 1);
        let msg = &response.choices[0].message;
        assert_eq!(msg.content.as_deref(), Some("Hello, how can I help?"));
        let usage = response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 15);
        assert_eq!(usage.completion_tokens, 7);
        assert_eq!(usage.total_tokens, 22);
    }

    #[test]
    fn test_stream_chunk_deserialization_text_delta() {
        let json = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {"content": "Hello"},
                "finish_reason": null
            }]
        }"#;
        let chunk: ChatSseChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
        assert_eq!(chunk.choices[0].delta.reasoning_content, None);
    }

    #[test]
    fn test_stream_chunk_deserialization_with_usage() {
        let json = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 15,
                "completion_tokens": 7,
                "total_tokens": 22
            }
        }"#;
        let chunk: ChatSseChunk = serde_json::from_str(json).unwrap();
        assert!(chunk.usage.is_some());
        let usage = chunk.usage.unwrap();
        assert_eq!(usage.total_tokens, 22);
    }

    #[test]
    fn test_stream_chunk_deserialization_tool_call() {
        let json = r#"{
            "id": "chatcmpl-123",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\":\"SF\"}"
                        }
                    }]
                }
            }]
        }"#;
        let chunk: ChatSseChunk = serde_json::from_str(json).unwrap();
        let tool_calls = chunk.choices[0].delta.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id.as_deref(), Some("call_abc"));
        let func = tool_calls[0].function.as_ref().unwrap();
        assert_eq!(func.name.as_deref(), Some("get_weather"));
    }

    #[test]
    fn test_deepseek_with_base_url() {
        let client = DeepSeek::new("sk-test-key")
            .unwrap()
            .with_base_url("https://custom-proxy.example.com");
        assert_eq!(client.name(), "deepseek");
        assert_eq!(client.base_url, "https://custom-proxy.example.com");
        assert_eq!(
            client.chat_endpoint(),
            "https://custom-proxy.example.com/chat/completions"
        );
    }

    #[test]
    fn test_build_request_body() {
        // 非流式请求
        let request = GenerateRequest {
            model: "deepseek-v4-pro".to_string(),
            instructions: Some("You are helpful.".to_string()),
            input: vec![Arc::new(InputItem::Message {
                role: Role::User,
                content: "Hello".to_string(),
            })]
            .into(),
            tools: vec![],
            tool_choice: None,
            temperature: Some(0.7),
            top_p: None,
            max_output_tokens: None,
            reasoning: None,
            text: None,
            additional_params: None,
        };
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["model"], "deepseek-v4-pro");
        assert_eq!(json["temperature"], 0.7);
        assert!(json.get("stream").is_none());
        assert_eq!(json["messages"].as_array().unwrap().len(), 2);

        // 流式请求
        let body = build_request_body(&request, true).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["stream"], true);
        assert_eq!(json["stream_options"]["include_usage"], true);
    }

    #[test]
    fn test_build_request_body_with_tools() {
        let tool = ToolDefinition {
            name: "get_weather".to_string(),
            description: "获取天气信息".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        };
        let request = GenerateRequest {
            model: "deepseek-v4-pro".to_string(),
            instructions: None,
            input: vec![Arc::new(InputItem::Message {
                role: Role::User,
                content: "天气怎么样？".to_string(),
            })]
            .into(),
            tools: vec![tool],
            tool_choice: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            reasoning: None,
            text: None,
            additional_params: None,
        };
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
    }

    /// 借用版 `WireToolCall` 的 JSON 形状必须与 owned [`ToolCall`] 逐字段一致。
    #[test]
    fn test_wire_tool_call_serialization_shape() {
        let request = GenerateRequest {
            model: "deepseek-v4-pro".to_string(),
            instructions: None,
            input: vec![
                Arc::new(InputItem::FunctionCall {
                    call_id: "c1".to_string(),
                    name: "get_weather".to_string(),
                    arguments: r#"{"city":"SF"}"#.to_string(),
                }),
                Arc::new(InputItem::FunctionCallOutput {
                    call_id: "c1".to_string(),
                    output: "72F".to_string(),
                }),
            ]
            .into(),
            tools: vec![],
            tool_choice: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            reasoning: None,
            text: None,
            additional_params: Some(serde_json::json!({"top_k": 5})),
        };
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        let assistant = &json["messages"][0];
        assert_eq!(assistant["role"], "assistant");
        let tc = &assistant["tool_calls"][0];
        assert_eq!(
            *tc,
            serde_json::to_value(ToolCall::new("c1", "get_weather", r#"{"city":"SF"}"#)).unwrap()
        );
        // assistant 无文本时不得序列化出 `content` 字段
        assert!(assistant.get("content").is_none());

        assert_eq!(json["messages"][1]["role"], "tool");
        assert_eq!(json["messages"][1]["tool_call_id"], "c1");
        assert_eq!(json["messages"][1]["content"], "72F");
        // additional_params 仍被扁平化透传
        assert_eq!(json["top_k"], 5);
    }

    #[test]
    fn test_input_items_to_wire_messages_merge() {
        let items = vec![
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "hi".to_string(),
            }),
            Arc::new(InputItem::Reasoning {
                content: "step 1".to_string(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "get_weather".to_string(),
                arguments: r#"{"city":"SF"}"#.to_string(),
            }),
            Arc::new(InputItem::Message {
                role: Role::Assistant,
                content: "done".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: "72F".to_string(),
            }),
        ];
        let msgs = input_items_to_wire_messages(&items);
        // user + assistant（reasoning+tool_call+text 合并）+ tool = 3 条
        assert_eq!(msgs.len(), 3);
        assert!(matches!(msgs[0], WireMessage::User { .. }));
        match &msgs[1] {
            WireMessage::Assistant {
                content,
                tool_calls,
                reasoning_content,
            } => {
                assert_eq!(content.as_deref(), Some("done"));
                assert_eq!(reasoning_content.as_deref(), Some("step 1"));
                assert_eq!(tool_calls.as_ref().map(|t| t.len()), Some(1));
            }
            other => panic!("expected assistant, got {other:?}"),
        }
        assert!(matches!(msgs[2], WireMessage::Tool { .. }));
    }
}
