//! Qwen 模型提供商实现。
//!
//! 提供 [`Qwen`]，为阿里云百炼（DashScope）的 OpenAI 兼容模式实现
//! [`ModelProvider`]，使用 chat completions 协议。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;
use tracing::{debug, trace, warn};

use crate::logging;
use crate::providers::chat_common::{
    WireMessage, ensure_trailing_user, input_items_to_wire_messages,
};
use crate::response::{
    ContentBlock, GenerateRequest, GenerateResult, InputItem, ReasoningConfig, ResponseError,
    ResponseStatus, Role, TextFormat,
};
use crate::streaming::pipeline::{
    NormalizedChunk, NormalizedToolCall, NormalizedUsage, StreamingProfile,
    process_normalized_sse_stream_chunks,
};
use crate::streaming::sse::StreamingEventSource;
use crate::{GenerateStream, ModelProvider, ProviderError, ToolCall, ToolDefinition, Usage};

// ============================================================================
// Qwen 客户端
// ============================================================================

/// DashScope OpenAI 兼容模式的基础 URL（chat completions 挂在其 `v1` 路径下）。
const QWEN_API_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";

/// Qwen API 客户端，实现 [`ModelProvider`]。
///
/// # 示例
///
/// ```ignore
/// use model_provider::{ModelProvider, Qwen};
///
/// let provider = Qwen::from_env()?;
/// assert_eq!(provider.name(), "qwen");
/// ```
pub struct Qwen {
    http_client: reqwest::Client,
    api_key: String,
    base_url: String,
    /// 严格特性校验：`false` 时静默丢弃（`debug!`）chat 适配器无法承载的特性
    /// （`text.format=json_schema`、`Role::Developer`），`true` 时返回 [`ProviderError::Request`]。
    strict_feature_validation: bool,
}

/// chat completions 适配器的语义别名（与 [`DeepSeek`](crate::DeepSeek) 的命名惯例一致）。
pub type QwenChatCompletionsAdapter = Qwen;

impl Qwen {
    /// 使用给定的 API 密钥创建新的 Qwen 客户端。
    ///
    /// 使用默认的基础 URL（DashScope OpenAI 兼容模式）。
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

    /// 通过读取 `DASHSCOPE_API_KEY` 环境变量创建新的 Qwen 客户端。
    ///
    /// 如果环境变量未设置，返回错误。
    pub fn from_env() -> Result<Self, ProviderError> {
        let api_key = std::env::var("DASHSCOPE_API_KEY")
            .map_err(|_| ProviderError::Request("DASHSCOPE_API_KEY 环境变量未设置".to_string()))?;
        Self::new(api_key)
    }

    /// 为 API 设置自定义的基础 URL。
    ///
    /// 适用于代理或自部署的 Qwen 兼容端点。
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

/// 将 ToolDefinition 包装为 OpenAI/Qwen 的传输格式。
#[derive(Serialize)]
struct ApiToolDef<'a> {
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: &'a ToolDefinition,
}

/// 思考模式下 `max_tokens` 的有效上限（超限网关返回 400）。
const QWEN_THINKING_MAX_TOKENS: u32 = 32768;

/// Qwen API 请求体（同时用于流式和非流式）。
///
/// 注意：Qwen 的思考参数是顶层的 `enable_thinking` bool，与 DeepSeek 的
/// `thinking` 嵌套对象形状完全不同，此处不得引入任何 `thinking` 字段。
#[derive(Serialize)]
struct QwenRequest<'a> {
    model: &'a str,
    messages: &'a [WireMessage<'a>],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ApiToolDef<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    /// 思考开关（Qwen 形式的顶层 bool，替代 DeepSeek 的 `thinking` 对象）。
    /// 未配置时省略，沿用模型默认行为。
    #[serde(skip_serializing_if = "Option::is_none")]
    enable_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
    /// 将额外参数扁平化合并到请求体中（透传 `GenerateRequest::additional_params`）。
    #[serde(flatten)]
    extra: Option<&'a Value>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// Qwen API 非流式响应。
#[derive(Deserialize)]
struct QwenResponse {
    #[serde(default)]
    id: String,
    choices: Vec<QwenChoice>,
    usage: Option<QwenApiUsage>,
}

#[derive(Deserialize)]
struct QwenChoice {
    message: QwenApiMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct QwenApiMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

/// Qwen API 用量信息。
#[derive(Deserialize)]
struct QwenApiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

// ============================================================================
// 内部 SSE 流式反序列化类型
// ============================================================================

/// Qwen 流式 API 中的单个 SSE 数据块。
#[derive(Deserialize)]
struct ChatSseChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<QwenApiUsage>,
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

/// 将 Qwen API 用量转换为我们的 Usage 类型。
fn convert_usage(api_usage: QwenApiUsage) -> Usage {
    Usage {
        input_tokens: api_usage.prompt_tokens,
        output_tokens: api_usage.completion_tokens,
        total_tokens: api_usage.total_tokens,
    }
}

/// 构建请求体并序列化为 JSON 字节。
///
/// `stream = true` 时携带 `stream: true` 与 `stream_options.include_usage = true`，
/// 且**绝不携带 tools** —— 千问官方明确 tools 与 stream=True 不可并用（D-3 红线），
/// 调用方传入的 tools 会被丢弃。
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
    // 千问网关要求最后一条消息为 user；工具轮末条是 `tool`，需追加一条空 user。
    ensure_trailing_user(&mut messages);

    let tools: Vec<ApiToolDef> = if stream {
        // 千问流式请求不携带 tools：与 stream=True 不可并用。
        if !request.tools.is_empty() {
            debug!(
                target: "model_provider::qwen",
                tools = request.tools.len(),
                "Qwen 流式请求不携带 tools（tools 与 stream=True 不可并用），忽略"
            );
        }
        Vec::new()
    } else {
        request
            .tools
            .iter()
            .map(|t| ApiToolDef {
                tool_type: "function",
                function: t,
            })
            .collect()
    };

    let enable_thinking = reasoning_config_to_enable_thinking(request.reasoning.as_ref());
    // 思考模式下 max_tokens 有效范围 [1, 32768]，超限网关返回 400。仅记录提醒，
    // 不主动改写用户配置（是否调整由调用方决定）。
    if enable_thinking == Some(true)
        && let Some(max_tokens) = request
            .max_output_tokens
            .filter(|m| *m > QWEN_THINKING_MAX_TOKENS)
    {
        debug!(
            target: "model_provider::qwen",
            max_tokens,
            limit = QWEN_THINKING_MAX_TOKENS,
            "思考模式下 max_tokens 超过上限 32768，可能被网关拒绝；保留原值不改写"
        );
    }

    let api_request = QwenRequest {
        model: &request.model,
        messages: &messages,
        tools,
        temperature: request.temperature,
        max_tokens: request.max_output_tokens,
        enable_thinking,
        stream: if stream { Some(true) } else { None },
        stream_options: if stream {
            Some(StreamOptions {
                include_usage: true,
            })
        } else {
            None
        },
        tool_choice: None,
        extra: request.additional_params.as_ref(),
    };

    serde_json::to_vec(&api_request)
}

/// 将中立 [`ReasoningConfig`] 映射为 Qwen 顶层 `enable_thinking` bool。
///
/// Qwen 的思考开关是请求体顶层的 `enable_thinking`（非 DeepSeek 的
/// `thinking{type,effort}` 嵌套对象，D-5 红线）；未配置时返回 `None`（字段省略，
/// 沿用模型默认行为）。Qwen 不支持 effort 力度概念——携带 effort 时仅 `debug!`
/// 记录后丢弃，不报错。
fn reasoning_config_to_enable_thinking(reasoning: Option<&ReasoningConfig>) -> Option<bool> {
    let config = reasoning?;
    if config.effort.is_some() {
        debug!(
            target: "model_provider::qwen",
            "Qwen 不支持 reasoning effort 力度配置，忽略"
        );
    }
    Some(config.enabled)
}

/// 将 chat 协议的 `finish_reason` 映射为中立 [`ResponseStatus`]。
///
/// 与 DeepSeek chat 适配器保持一致：`tool_calls` 表示模型正常发起工具调用
/// （非失败），`finish_reason` 缺失视为正常完成。
fn finish_reason_to_status(reason: Option<&str>) -> ResponseStatus {
    match reason {
        Some("stop") | Some("tool_calls") | None => ResponseStatus::Completed,
        Some("length") => ResponseStatus::Incomplete,
        _ => ResponseStatus::Failed,
    }
}

/// 将 Qwen chat 响应转换为中立 [`GenerateResult`]。
///
/// 块顺序：`reasoning_content`→`Reasoning`（在前，思考先于回答产生）、
/// `content`→`Text`、`tool_calls`→`ToolCall`。注意与 DeepSeek chat 适配器的
/// Text 在前不同。`content` 为空字符串时视同缺失，不产出空文本块。
fn chat_response_to_generate_result(
    api_response: QwenResponse,
) -> Result<GenerateResult, ProviderError> {
    let choice = api_response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| ProviderError::Response("响应中不包含任何选项".to_string()))?;

    let finish_reason = choice.finish_reason;
    let status = finish_reason_to_status(finish_reason.as_deref());

    let mut output = Vec::new();
    if let Some(reasoning) = choice.message.reasoning_content {
        output.push(ContentBlock::Reasoning { text: reasoning });
    }
    // 空字符串 content 视同缺失，避免产出空文本块。
    if let Some(content) = choice.message.content.filter(|c| !c.is_empty()) {
        output.push(ContentBlock::Text { text: content });
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
impl ModelProvider for Qwen {
    fn name(&self) -> &str {
        "qwen"
    }

    async fn generate_full(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateResult, ProviderError> {
        let request_id = logging::next_request_id();
        let endpoint = self.chat_endpoint();
        let span = tracing::info_span!(
            "qwen_chat_generate_full",
            provider = "qwen",
            endpoint = %endpoint,
            model = %request.model,
            request_id = %request_id,
        );

        async move {
            self.validate_generate_request(request)?;

            let started = std::time::Instant::now();
            let body = build_request_body(request, false)?;
            let input = logging::summarize_input(&request.input);

            debug!(
                target: "model_provider::qwen",
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
            trace!(
                target: "model_provider::qwen",
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
                warn!(
                    target: "model_provider::qwen",
                    request_id = %request_id,
                    provider_request_id = provider_request_id.as_deref().unwrap_or("-"),
                    status = status.as_u16(),
                    latency_ms,
                    body = %body_str,
                    "Qwen API 返回错误状态"
                );
                return Err(ProviderError::Api {
                    status: status.as_u16(),
                    body: body_str,
                });
            }

            trace!(
                target: "model_provider::qwen",
                request_id = %request_id,
                body = %String::from_utf8_lossy(&response_body),
                "chat 响应体全文"
            );

            let api_response: QwenResponse = serde_json::from_slice(&response_body)?;
            let result = chat_response_to_generate_result(api_response)?;

            let blocks = logging::summarize_blocks(&result.output);
            debug!(
                target: "model_provider::qwen",
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

        debug!(
            target: "model_provider::qwen",
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
        // 含用户对话原文，仅 trace 级别输出。`body` 随后被 move 进请求，故在此之前取。
        trace!(
            target: "model_provider::qwen",
            request_id = %request_id,
            body = %String::from_utf8_lossy(&body),
            "chat 流式请求体全文"
        );

        let span = tracing::info_span!(
            "qwen_stream_generate",
            provider = "qwen",
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
            QwenStreamingProfile,
            span,
            model,
            request_id,
        ))
    }
}

impl Qwen {
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
                debug!(
                    target: "model_provider::qwen",
                    "chat 适配器不支持 text.format=json_schema，忽略"
                );
            }
            if has_developer {
                debug!(
                    target: "model_provider::qwen",
                    "chat 适配器不支持 Developer role，降级为 system"
                );
            }
        }
        Ok(())
    }
}

// ============================================================================
// Qwen 流式配置文件
// ============================================================================

/// Qwen SSE 数据块的提供商标定 [`StreamingProfile`]。
struct QwenStreamingProfile;

impl StreamingProfile for QwenStreamingProfile {
    fn normalize_chunk(&self, data: &str) -> Result<Option<NormalizedChunk>, ProviderError> {
        let chunk: ChatSseChunk = serde_json::from_str(data)
            .map_err(|e| ProviderError::Stream(format!("解析 SSE 数据块失败: {e}")))?;

        let usage = chunk.usage.map(|u| NormalizedUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        });

        // 末帧用量 chunk：choices 为空而 usage 非空 —— 仅提取 usage，
        // 交由管线随流尾输出（`include_usage` 请求的正是这一帧）。
        let Some(choice) = chunk.choices.first() else {
            if usage.is_some() {
                return Ok(Some(NormalizedChunk {
                    text: None,
                    reasoning: None,
                    tool_calls: Vec::new(),
                    finish_reason: None,
                    usage,
                }));
            }
            return Ok(None);
        };

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

        Ok(Some(NormalizedChunk {
            text: choice.delta.content.clone(),
            reasoning: choice.delta.reasoning_content.clone(),
            tool_calls,
            finish_reason: choice.finish_reason.clone(),
            usage,
        }))
    }

    // Qwen（OpenAI 兼容模式）以增量片段流式传输工具调用，按 wire index 累积即可，
    // 不需要 DeepSeek 的同索引淘汰与单帧完整工具调用特判。
}

// ============================================================================
// Builder 风格配置
// ============================================================================

impl Qwen {
    /// 开始使用自定义配置构建 Qwen 客户端。
    pub fn builder() -> QwenBuilder {
        QwenBuilder::default()
    }
}

/// 用于构建 [`Qwen`] 客户端的构建器。
#[derive(Default)]
pub struct QwenBuilder {
    api_key: Option<String>,
    base_url: Option<String>,
}

impl QwenBuilder {
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

    /// 构建 [`Qwen`] 客户端。
    ///
    /// 如果未提供 API 密钥则返回错误。
    pub fn build(self) -> Result<Qwen, ProviderError> {
        let api_key = self
            .api_key
            .ok_or_else(|| ProviderError::Request("需要提供 API 密钥".to_string()))?;

        let mut client = Qwen::new(api_key)?;
        if let Some(url) = self.base_url {
            client = client.with_base_url(url);
        }
        Ok(client)
    }
}

// ============================================================================
// 公共模型名称常量
// ============================================================================

/// Qwen 3.7 Max — 最强大的模型。
pub const QWEN_MAX: &str = "qwen3.7-max";

/// Qwen 3.7 Plus — 能力与成本的均衡选择。
pub const QWEN_PLUS: &str = "qwen3.7-plus";

/// Qwen 3.8 Flash — 快速且经济。
pub const QWEN_FLASH: &str = "qwen3.8-flash";

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::{BlockType, FinishReason, StreamChunk};
    use futures::StreamExt;
    use std::sync::Arc;

    fn text_request() -> GenerateRequest {
        GenerateRequest {
            model: QWEN_PLUS.to_string(),
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
        }
    }

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
        client: &Qwen,
        request: &GenerateRequest,
    ) -> Vec<Result<StreamChunk, ProviderError>> {
        let mut stream = client.generate_stream(request).await.unwrap();
        let mut chunks = Vec::new();
        while let Some(chunk) = stream.next().await {
            chunks.push(chunk);
        }
        chunks
    }

    #[test]
    fn test_qwen_with_base_url() {
        let client = Qwen::new("sk-test-key")
            .unwrap()
            .with_base_url("https://custom-proxy.example.com/v1");
        assert_eq!(client.name(), "qwen");
        assert_eq!(client.base_url, "https://custom-proxy.example.com/v1");
        assert_eq!(
            client.chat_endpoint(),
            "https://custom-proxy.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_default_endpoint() {
        let client = Qwen::new("sk-test-key").unwrap();
        assert_eq!(
            client.chat_endpoint(),
            "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions"
        );
        assert!(client.auth_header().starts_with("Bearer sk-test-key"));
    }

    // ── Developer role 与严格校验 ──

    fn developer_request() -> GenerateRequest {
        GenerateRequest {
            model: QWEN_PLUS.to_string(),
            instructions: None,
            input: vec![
                Arc::new(InputItem::Message {
                    role: Role::Developer,
                    content: "policy".to_string(),
                }),
                Arc::new(InputItem::Message {
                    role: Role::User,
                    content: "Hello".to_string(),
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
            additional_params: None,
        }
    }

    /// 宽松模式（默认）：Developer role 降级为 system，不报错。
    #[test]
    fn test_developer_role_loose_mode_downgrades_to_system() {
        let client = Qwen::new("sk-test-key").unwrap();
        client
            .validate_generate_request(&developer_request())
            .expect("loose mode must not reject Developer role");

        let body = build_request_body(&developer_request(), false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "policy");
        assert_eq!(messages[1]["role"], "user");
    }

    /// 严格模式：Developer role 返回 [`ProviderError::Request`]。
    #[test]
    fn test_strict_validation_rejects_developer_role() {
        let client = Qwen::new("sk-test-key")
            .unwrap()
            .strict_feature_validation(true);
        let err = client
            .validate_generate_request(&developer_request())
            .unwrap_err();
        assert!(matches!(err, ProviderError::Request(ref m) if m.contains("Developer")));
    }

    /// 严格模式：`text.format=json_schema` 返回 [`ProviderError::Request`]。
    #[test]
    fn test_strict_validation_rejects_json_schema() {
        let mut request = text_request();
        request.text = Some(crate::response::TextConfig {
            format: Some(TextFormat::JsonSchema {
                name: "out".to_string(),
                schema: serde_json::json!({"type": "object"}),
            }),
        });
        // 宽松模式不报错
        Qwen::new("sk-test-key")
            .unwrap()
            .validate_generate_request(&request)
            .expect("loose mode must not reject json_schema");

        let client = Qwen::new("sk-test-key")
            .unwrap()
            .strict_feature_validation(true);
        let err = client.validate_generate_request(&request).unwrap_err();
        assert!(matches!(err, ProviderError::Request(ref m) if m.contains("json_schema")));
    }

    #[test]
    fn test_build_request_body_shape() {
        let body = build_request_body(&text_request(), false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["model"], "qwen3.7-plus");
        // instructions → 首条 system 消息
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello");

        // Option 条件序列化：已设置的字段出现，未设置的不出现
        assert_eq!(json["temperature"], 0.7);
        assert!(json.get("max_tokens").is_none());
        // 非流式请求不带 stream 字段
        assert!(json.get("stream").is_none());
        // 绝不携带 DeepSeek 形状的 thinking 嵌套对象
        assert!(json.get("thinking").is_none());
        assert!(json.get("enable_thinking").is_none());
    }

    #[test]
    fn test_build_request_body_optional_fields() {
        let mut request = text_request();
        request.temperature = None;
        request.max_output_tokens = Some(2048);
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("temperature").is_none());
        assert_eq!(json["max_tokens"], 2048);
    }

    // ── 思考模式映射（AC-6.1 / AC-6.2 / AC-6.4）──

    /// AC-6.1：`ReasoningConfig.enabled=false` → 请求体 `enable_thinking:false`。
    #[test]
    fn test_reasoning_disabled_sets_enable_thinking_false() {
        let mut request = text_request();
        request.reasoning = Some(crate::response::ReasoningConfig {
            enabled: false,
            effort: None,
        });
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["enable_thinking"], false);
        // D-5 红线：绝不出现 DeepSeek 形状的 thinking 嵌套对象
        assert!(json.get("thinking").is_none());
    }

    /// AC-6.2：effort（Low/Medium/High/Max）无 Qwen 对应概念——省略字段不报错。
    #[test]
    fn test_reasoning_effort_is_dropped_not_sent() {
        for effort in [
            crate::response::ReasoningEffort::Low,
            crate::response::ReasoningEffort::Medium,
            crate::response::ReasoningEffort::High,
            crate::response::ReasoningEffort::Max,
        ] {
            let mut request = text_request();
            request.reasoning = Some(crate::response::ReasoningConfig {
                enabled: true,
                effort: Some(effort),
            });
            let body = build_request_body(&request, false).unwrap();
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["enable_thinking"], true);
            // effort 不产生任何 wire 字段
            assert!(json.get("reasoning_effort").is_none());
            assert!(json.get("thinking_budget").is_none());
            assert!(json.get("thinking").is_none());
        }
    }

    /// 未配置 reasoning 时省略 `enable_thinking`，沿用模型默认行为。
    #[test]
    fn test_reasoning_none_omits_enable_thinking() {
        assert!(text_request().reasoning.is_none());
        let body = build_request_body(&text_request(), false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("enable_thinking").is_none());
    }

    /// 思考模式下 `max_tokens` 超过 32768 上限：仅提醒，不改写用户配置。
    #[test]
    fn test_thinking_mode_max_tokens_over_limit_kept_as_is() {
        let mut request = text_request();
        request.reasoning = Some(crate::response::ReasoningConfig {
            enabled: true,
            effort: None,
        });
        request.max_output_tokens = Some(40_000);
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["enable_thinking"], true);
        assert_eq!(json["max_tokens"], 40_000);
    }

    /// AC-6.3：`reasoning_content` 与 `content` 同时出现时，Reasoning 块在前、Text 在后。
    #[test]
    fn test_response_reasoning_block_precedes_text_block() {
        let json = r#"{
            "id": "chatcmpl-think",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "答案是 42",
                    "reasoning_content": "先分析问题再计算"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 8,
                "completion_tokens": 20,
                "total_tokens": 28
            }
        }"#;
        let api_response: QwenResponse = serde_json::from_str(json).unwrap();
        let result = chat_response_to_generate_result(api_response).unwrap();

        assert_eq!(result.output.len(), 2);
        assert_eq!(
            result.output[0],
            ContentBlock::Reasoning {
                text: "先分析问题再计算".to_string()
            }
        );
        assert_eq!(
            result.output[1],
            ContentBlock::Text {
                text: "答案是 42".to_string()
            }
        );
    }

    #[test]
    fn test_build_request_body_with_tools() {
        let mut request = text_request();
        request.tools = vec![ToolDefinition {
            name: "get_weather".to_string(),
            description: "获取天气信息".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }];
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        // wire 形状必须与 DeepSeek chat 适配器逐字节一致
        assert_eq!(
            tools[0],
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "获取天气信息",
                    "parameters": {"type": "object", "properties": {}}
                }
            })
        );
    }

    #[test]
    fn test_build_request_body_wire_shape() {
        let request = GenerateRequest {
            model: QWEN_FLASH.to_string(),
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
            reasoning: Some(crate::response::ReasoningConfig {
                enabled: true,
                effort: Some(crate::response::ReasoningEffort::High),
            }),
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
        // 思考开关映射为顶层 enable_thinking（effort 被丢弃），绝不出现 DeepSeek 形状的 thinking 对象
        assert_eq!(json["enable_thinking"], true);
        assert!(json.get("thinking").is_none());
        assert!(json.get("reasoning_effort").is_none());
    }

    /// D-4：工具轮末条是 `tool`，出口必须追加一条空 content 的 user。
    #[test]
    fn test_build_request_body_appends_trailing_user_after_tool_round() {
        let request = GenerateRequest {
            model: QWEN_FLASH.to_string(),
            instructions: Some("You are helpful.".to_string()),
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
            additional_params: None,
        };
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        let messages = json["messages"].as_array().unwrap();
        // system + assistant(tool_calls) + tool + 追加的空 user
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[3]["role"], "user");
        assert_eq!(messages[3]["content"], "");

        // 流式路径同样追加（流式虽不带 tools，但历史回放仍可能以 tool 结尾）
        let body = build_request_body(&request, true).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.last().unwrap()["role"], "user");
        assert_eq!(messages.last().unwrap()["content"], "");
    }

    /// 末条已是 user 时不得重复追加。
    #[test]
    fn test_build_request_body_keeps_existing_trailing_user() {
        let body = build_request_body(&text_request(), false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let messages = json["messages"].as_array().unwrap();
        // system + user，无追加
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello");
    }

    #[test]
    fn test_response_deserialization_and_mapping() {
        let json = r#"{
            "id": "chatcmpl-abc",
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
        let api_response: QwenResponse = serde_json::from_str(json).unwrap();
        let result = chat_response_to_generate_result(api_response).unwrap();

        assert_eq!(result.id, "chatcmpl-abc");
        assert_eq!(result.status, ResponseStatus::Completed);
        assert!(result.error.is_none());
        assert_eq!(result.output.len(), 1);
        assert_eq!(
            result.output[0],
            ContentBlock::Text {
                text: "Hello, how can I help?".to_string()
            }
        );
        assert_eq!(result.usage.input_tokens, 15);
        assert_eq!(result.usage.output_tokens, 7);
        assert_eq!(result.usage.total_tokens, 22);
    }

    #[test]
    fn test_response_empty_content_no_empty_text_block() {
        let json = r#"{
            "id": "chatcmpl-empty",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": ""},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 0,
                "total_tokens": 5
            }
        }"#;
        let api_response: QwenResponse = serde_json::from_str(json).unwrap();
        let result = chat_response_to_generate_result(api_response).unwrap();
        assert!(
            !result
                .output
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text.is_empty()))
        );
        assert!(result.output.is_empty());
    }

    #[test]
    fn test_response_tool_calls_and_reasoning() {
        let json = r#"{
            "id": "chatcmpl-tc",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "需要查天气",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"SF\"}"
                        }
                    }]
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 12,
                "total_tokens": 22
            }
        }"#;
        let api_response: QwenResponse = serde_json::from_str(json).unwrap();
        let result = chat_response_to_generate_result(api_response).unwrap();

        // 空 content 不产文本块；仅 reasoning + tool call
        assert_eq!(result.output.len(), 2);
        assert_eq!(
            result.output[0],
            ContentBlock::Reasoning {
                text: "需要查天气".to_string()
            }
        );
        match &result.output[1] {
            ContentBlock::ToolCall {
                call_id,
                name,
                arguments,
            } => {
                assert_eq!(call_id, "call_1");
                assert_eq!(name, "get_weather");
                assert_eq!(arguments, r#"{"city":"SF"}"#);
            }
            other => panic!("expected tool call, got {other:?}"),
        }
    }

    #[test]
    fn test_finish_reason_mapping() {
        assert_eq!(
            finish_reason_to_status(Some("stop")),
            ResponseStatus::Completed
        );
        // 千问正常返回 tool_calls 表示模型发起工具调用，非失败（同 DeepSeek）。
        assert_eq!(
            finish_reason_to_status(Some("tool_calls")),
            ResponseStatus::Completed
        );
        // finish_reason 缺失视为正常完成（同 DeepSeek）。
        assert_eq!(finish_reason_to_status(None), ResponseStatus::Completed);
        assert_eq!(
            finish_reason_to_status(Some("length")),
            ResponseStatus::Incomplete
        );
        assert_eq!(
            finish_reason_to_status(Some("content_filter")),
            ResponseStatus::Failed
        );
    }

    /// finish_reason="tool_calls" 的响应映射为 Completed 且产出 ToolCall 块，error 为空。
    #[test]
    fn test_response_tool_calls_finish_reason_is_completed() {
        let json = r#"{
            "id": "chatcmpl-tc-fin",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"SF\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 12,
                "total_tokens": 22
            }
        }"#;
        let api_response: QwenResponse = serde_json::from_str(json).unwrap();
        let result = chat_response_to_generate_result(api_response).unwrap();

        assert_eq!(result.status, ResponseStatus::Completed);
        assert!(result.error.is_none());
        assert_eq!(result.output.len(), 1);
        assert!(matches!(
            &result.output[0],
            ContentBlock::ToolCall { call_id, name, .. }
                if call_id == "call_1" && name == "get_weather"
        ));
    }

    #[test]
    fn test_failed_finish_reason_sets_error() {
        let json = r#"{
            "id": "chatcmpl-fail",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "partial"},
                "finish_reason": "content_filter"
            }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2
            }
        }"#;
        let api_response: QwenResponse = serde_json::from_str(json).unwrap();
        let result = chat_response_to_generate_result(api_response).unwrap();
        assert_eq!(result.status, ResponseStatus::Failed);
        assert_eq!(
            result.error.map(|e| e.message),
            Some("content_filter".to_string())
        );
    }

    #[test]
    fn test_usage_missing_defaults() {
        let json = r#"{
            "id": "chatcmpl-nousage",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hi"},
                "finish_reason": "stop"
            }]
        }"#;
        let api_response: QwenResponse = serde_json::from_str(json).unwrap();
        let result = chat_response_to_generate_result(api_response).unwrap();
        assert_eq!(result.usage, Usage::default());
    }

    #[test]
    fn test_no_choices_is_response_error() {
        let api_response: QwenResponse =
            serde_json::from_str(r#"{"id": "x", "choices": []}"#).unwrap();
        let err = chat_response_to_generate_result(api_response).unwrap_err();
        assert!(matches!(err, ProviderError::Response(_)));
    }

    // ── 流式请求体 ──

    #[test]
    fn test_build_stream_request_body_shape() {
        let body = build_request_body(&text_request(), true).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["stream"], true);
        assert_eq!(json["stream_options"]["include_usage"], true);
        // 流式请求绝不携带 tools（D-3）
        assert!(json.get("tools").is_none());
        // 绝不出现 DeepSeek 形状的 thinking 嵌套对象
        assert!(json.get("thinking").is_none());
        assert!(json.get("enable_thinking").is_none());
    }

    /// D-3 红线：流式请求体 tools 恒为 None —— 传入 tools 仅 debug! 后忽略。
    #[test]
    fn test_build_stream_request_body_never_carries_tools() {
        let mut request = text_request();
        request.tools = vec![ToolDefinition {
            name: "get_weather".to_string(),
            description: "获取天气信息".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }];
        let body = build_request_body(&request, true).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("tools").is_none());

        // 非流式路径不受影响，tools 正常携带
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["tools"].as_array().map(|t| t.len()), Some(1));
    }

    // ── SSE chunk 反序列化 ──

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
    fn test_stream_chunk_deserialization_usage_only_chunk() {
        // 末帧用量 chunk：choices 为空数组、usage 非空
        let json = r#"{
            "id": "chatcmpl-123",
            "choices": [],
            "usage": {
                "prompt_tokens": 15,
                "completion_tokens": 7,
                "total_tokens": 22
            }
        }"#;
        let chunk: ChatSseChunk = serde_json::from_str(json).unwrap();
        assert!(chunk.choices.is_empty());
        let usage = chunk.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 15);
        assert_eq!(usage.completion_tokens, 7);
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

    // ── StreamingProfile chunk 规范化 ──

    #[test]
    fn test_normalize_chunk_maps_text_and_reasoning() {
        let data = r#"{"choices":[{"index":0,"delta":{"content":"你好","reasoning_content":"思考中"},"finish_reason":null}]}"#;
        let chunk = QwenStreamingProfile
            .normalize_chunk(data)
            .unwrap()
            .expect("chunk should normalize");
        assert_eq!(chunk.text.as_deref(), Some("你好"));
        assert_eq!(chunk.reasoning.as_deref(), Some("思考中"));
        assert!(chunk.tool_calls.is_empty());
        assert!(chunk.usage.is_none());
    }

    #[test]
    fn test_normalize_chunk_usage_only_chunk_keeps_usage() {
        let data =
            r#"{"choices":[],"usage":{"prompt_tokens":6,"completion_tokens":2,"total_tokens":8}}"#;
        let chunk = QwenStreamingProfile
            .normalize_chunk(data)
            .unwrap()
            .expect("usage-only chunk should normalize");
        assert_eq!(chunk.text, None);
        assert_eq!(chunk.finish_reason, None);
        let usage = chunk.usage.expect("usage should be preserved");
        assert_eq!(usage.prompt_tokens, 6);
        assert_eq!(usage.completion_tokens, 2);
        assert_eq!(usage.total_tokens, 8);
    }

    #[test]
    fn test_normalize_chunk_skips_chunk_without_choices_and_usage() {
        let chunk = QwenStreamingProfile
            .normalize_chunk(r#"{"id":"chatcmpl-123"}"#)
            .unwrap();
        assert!(chunk.is_none());
    }

    #[test]
    fn test_normalize_chunk_malformed_json_is_stream_error() {
        let err = QwenStreamingProfile
            .normalize_chunk("{not json")
            .unwrap_err();
        assert!(matches!(err, ProviderError::Stream(_)));
    }

    // ── 流式端到端（本地一次性 SSE 服务） ──

    #[tokio::test]
    async fn test_stream_text_assembly_and_done() {
        let base = spawn_sse_server(sse_events(&[
            // 首帧 role-only：空 content 不产文本增量
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"你"},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"好"},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            // 末帧用量 chunk + [DONE] + 空行
            r#"{"choices":[],"usage":{"prompt_tokens":6,"completion_tokens":2,"total_tokens":8}}"#,
            "[DONE]",
            "",
        ]))
        .await;
        let client = Qwen::new("sk-test-key").unwrap().with_base_url(base);
        let chunks = collect_stream(&client, &text_request()).await;

        assert!(matches!(
            &chunks[..],
            [
                Ok(StreamChunk::BlockStart {
                    index: 0,
                    block_type: BlockType::Text
                }),
                Ok(StreamChunk::TextDelta { index: 0, delta }),
                Ok(StreamChunk::TextDelta { index: 0, .. }),
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Text { text },
                }),
                Ok(StreamChunk::Usage { usage }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop
                }),
            ]
            if delta == "你"
                && text == "你好"
                && *usage == (Usage {
                    input_tokens: 6,
                    output_tokens: 2,
                    total_tokens: 8,
                })
        ));
        assert_eq!(chunks.len(), 6);
    }

    #[tokio::test]
    async fn test_stream_tool_call_assembly() {
        let base = spawn_sse_server(sse_events(&[
            // 工具调用首帧：id + name
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather"}}]}}]}"#,
            // 工具调用参数片段
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":\"SF\"}"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
            "[DONE]",
        ]))
        .await;
        let client = Qwen::new("sk-test-key").unwrap().with_base_url(base);
        let chunks = collect_stream(&client, &text_request()).await;

        assert!(matches!(
            chunks[0],
            Ok(StreamChunk::BlockStart {
                block_type: BlockType::ToolCall,
                ..
            })
        ));
        assert!(matches!(
            chunks[1],
            Ok(StreamChunk::ToolCallDelta {
                name: Some(ref n),
                ..
            }) if n == "get_weather"
        ));
        assert!(matches!(
            chunks[2],
            Ok(StreamChunk::ToolCallDelta {
                name: None,
                ref arguments,
                ..
            }) if arguments == &serde_json::Value::String(r#"{"city":"SF"}"#.to_string())
        ));
        // finish_reason=tool_calls 触发 flush，产出完整工具调用块
        assert!(matches!(
            chunks[3],
            Ok(StreamChunk::BlockEnd {
                index: 2,
                block: ContentBlock::ToolCall { ref call_id, ref name, ref arguments },
            }) if call_id == "call_1"
                && name == "get_weather"
                && arguments == r#"{"city":"SF"}"#
        ));
        // 随流尾输出 Usage + Finish
        assert!(matches!(
            &chunks[4..],
            [
                Ok(StreamChunk::Usage { usage }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::ToolCalls
                }),
            ] if *usage == (Usage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            })
        ));
        assert_eq!(chunks.len(), 6);
    }

    #[tokio::test]
    async fn test_stream_error_chunk_aborts_with_api_error() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]}"#,
            r#"{"error":{"code":"InvalidParameter","message":"tools 与 stream 不可并用"}}"#,
        ]))
        .await;
        let client = Qwen::new("sk-test-key").unwrap().with_base_url(base);
        let chunks = collect_stream(&client, &text_request()).await;

        // 流内错误载荷中止流：产出错误且不再有正常收尾（Usage/Finish）
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

    #[tokio::test]
    async fn test_stream_malformed_chunk_aborts_with_stream_error() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]}"#,
            "{not json",
        ]))
        .await;
        let client = Qwen::new("sk-test-key").unwrap().with_base_url(base);
        let chunks = collect_stream(&client, &text_request()).await;

        // 解析错误 → ProviderError::Stream 并中止流
        assert!(matches!(chunks.last(), Some(Err(ProviderError::Stream(_)))));
        assert!(
            !chunks
                .iter()
                .any(|c| matches!(c, Ok(StreamChunk::Finish { .. })))
        );
    }

    #[test]
    fn test_from_env_missing_key_errors() {
        // 仅本测试操作该变量；保存并恢复以最小化对并行测试的干扰
        let saved = std::env::var("DASHSCOPE_API_KEY").ok();
        unsafe { std::env::remove_var("DASHSCOPE_API_KEY") };
        let result = Qwen::from_env();
        if let Some(key) = saved {
            unsafe { std::env::set_var("DASHSCOPE_API_KEY", key) };
        }
        let err = result.err().expect("missing DASHSCOPE_API_KEY must error");
        assert!(
            err.to_string().contains("DASHSCOPE_API_KEY"),
            "unexpected message: {err}"
        );
    }
}
