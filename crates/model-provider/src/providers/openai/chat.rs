//! OpenAI 模型提供商实现。
//!
//! 提供 [`OpenAI`]，为 [OpenAI chat completions API](https://platform.openai.com/docs/api-reference/chat/create)
//! 实现 [`ModelProvider`]。该协议同时是各 OpenAI 兼容网关（vLLM、OpenRouter 等）
//! 的最大公约数，经 `base_url` 覆盖即可接入，无需独立的厂商类型。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;
use tracing::{debug, trace, warn};

use crate::logging;
use crate::providers::chat_common::{WireMessage, input_items_to_wire_messages};
use crate::response::{
    ContentBlock, GenerateRequest, GenerateResult, InputItem, ReasoningConfig, ReasoningEffort,
    ResponseError, ResponseStatus, Role, TextFormat, ToolChoice,
};
use crate::streaming::pipeline::{
    NormalizedChunk, NormalizedToolCall, NormalizedUsage, StreamingProfile,
    process_normalized_sse_stream_chunks,
};
use crate::streaming::sse::StreamingEventSource;
use crate::{GenerateStream, ModelProvider, ProviderError, ToolCall, ToolDefinition, Usage};

// ============================================================================
// OpenAI 客户端
// ============================================================================

/// OpenAI API 的基础 URL（chat completions 端点位于其 `v1` 路径下）。
const OPENAI_API_BASE_URL: &str = "https://api.openai.com/v1";

/// 思考模式下生成预算的建议下限提示（推理 token 计入 `max_completion_tokens`，
/// 预算过小会导致可见输出被推理挤占；仅 `debug!` 记录，不改写用户配置）。
const OPENAI_REASONING_MIN_BUDGET: u32 = 25_000;

/// OpenAI API 客户端，实现 [`ModelProvider`]。
///
/// # 示例
///
/// ```ignore
/// use model_provider::{ModelProvider, OpenAI};
///
/// let provider = OpenAI::from_env()?;
/// assert_eq!(provider.name(), "openai");
/// ```
pub struct OpenAI {
    http_client: reqwest::Client,
    api_key: String,
    base_url: String,
    /// 严格特性校验：`false` 时静默降级（`debug!`）chat wire 层无法承载的特性
    /// （`Role::Developer` 降级为 system），`true` 时返回 [`ProviderError::Request`]。
    strict_feature_validation: bool,
}

/// chat completions 适配器的语义别名（与 [`DeepSeek`](crate::DeepSeek) 的命名惯例一致）。
pub type OpenAiChatCompletionsAdapter = OpenAI;

impl OpenAI {
    /// 使用给定的 API 密钥创建新的 OpenAI 客户端。
    ///
    /// 使用默认的基础 URL（`https://api.openai.com/v1`）。接入兼容网关时用
    /// [`Self::with_base_url`] 覆盖。
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
            strict_feature_validation: false,
        })
    }

    /// 通过读取 `OPENAI_API_KEY` 环境变量创建新的 OpenAI 客户端。
    ///
    /// 如果环境变量未设置，返回错误。
    pub fn from_env() -> Result<Self, ProviderError> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| ProviderError::Request("OPENAI_API_KEY 环境变量未设置".to_string()))?;
        Self::new(api_key)
    }

    /// 为 API 设置自定义的基础 URL。
    ///
    /// 适用于 OpenAI 兼容网关（vLLM、OpenRouter 等），URL 需包含版本路径
    /// （如 `https://openrouter.ai/api/v1`）。
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

/// 将 ToolDefinition 包装为 OpenAI chat 的传输格式。
#[derive(Serialize)]
struct ApiToolDef<'a> {
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: &'a ToolDefinition,
}

/// OpenAI chat completions 请求体（同时用于流式和非流式）。
///
/// 与 DeepSeek 的差异：生成预算字段是 `max_completion_tokens`（推理 token 计入其中，
/// `max_tokens` 与推理模型不兼容，绝不发送）；思考力度是顶层 `reasoning_effort`
/// 字符串（非 `thinking` 嵌套对象）；原生支持 `top_p`、`tool_choice`、`response_format`。
#[derive(Serialize)]
struct OpenAiRequest<'a> {
    model: &'a str,
    messages: &'a [WireMessage<'a>],
    /// 流式与非流式同样携带：OpenAI 的流式工具调用是标准 ReAct 路径。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ApiToolDef<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    /// 将额外参数扁平化合并到请求体中（透传 `GenerateRequest::additional_params`）。
    #[serde(flatten)]
    extra: Option<&'a Value>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// OpenAI chat completions 非流式响应。
#[derive(Deserialize)]
struct OpenAiResponse {
    #[serde(default)]
    id: String,
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiApiUsage>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiApiMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiApiMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
    /// 原生 OpenAI 不返回推理文本；部分兼容网关用 `reasoning_content`（DeepSeek 系惯例）。
    #[serde(default)]
    reasoning_content: Option<String>,
    /// 部分兼容网关的推理文本字段名，`reasoning_content` 缺失时的回退。
    #[serde(default)]
    reasoning: Option<String>,
}

/// OpenAI API 用量信息。
#[derive(Deserialize)]
struct OpenAiApiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

// ============================================================================
// 内部 SSE 流式反序列化类型
// ============================================================================

/// OpenAI 流式 API 中的单个 SSE 数据块。
#[derive(Deserialize)]
struct ChatSseChunk {
    /// 个别网关会省略 `choices`（如 `include_usage` 请求的末帧仅含 usage）。
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<OpenAiApiUsage>,
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
    /// 部分兼容网关的推理增量字段名，`reasoning_content` 缺失时的回退。
    #[serde(default)]
    reasoning: Option<String>,
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

/// 将 OpenAI API 用量转换为我们的 Usage 类型。
fn convert_usage(api_usage: OpenAiApiUsage) -> Usage {
    Usage {
        input_tokens: api_usage.prompt_tokens,
        output_tokens: api_usage.completion_tokens,
        total_tokens: api_usage.total_tokens,
    }
}

/// 构建请求体并序列化为 JSON 字节。
///
/// OpenAI chat 协议没有"消息末尾必须为 user"的约束，不做末尾 user 防御。
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

    // 流式同样携带 tools：OpenAI 的流式工具调用是标准 ReAct 路径。
    let tools: Vec<ApiToolDef> = request
        .tools
        .iter()
        .map(|t| ApiToolDef {
            tool_type: "function",
            function: t,
        })
        .collect();

    let reasoning_effort = reasoning_config_to_effort(request.reasoning.as_ref());
    // 推理 token 计入 max_completion_tokens：预算过小会导致可见输出被推理挤占。
    // 仅记录提醒，不主动改写用户配置（是否调整由调用方决定）。
    if let Some(effort) = &reasoning_effort
        && effort != "none"
        && let Some(budget) = request
            .max_output_tokens
            .filter(|m| *m < OPENAI_REASONING_MIN_BUDGET)
    {
        debug!(
            target: "model_provider::openai",
            max_completion_tokens = budget,
            suggested_min = OPENAI_REASONING_MIN_BUDGET,
            "推理生效时 max_completion_tokens 低于建议下限，可见输出可能被推理挤占；保留原值不改写"
        );
    }

    let api_request = OpenAiRequest {
        model: &request.model,
        messages: &messages,
        tools,
        tool_choice: tool_choice_to_wire(request.tool_choice.as_ref()),
        temperature: request.temperature,
        top_p: request.top_p,
        max_completion_tokens: request.max_output_tokens,
        reasoning_effort,
        response_format: text_format_to_response_format(
            request.text.as_ref().and_then(|t| t.format.as_ref()),
        ),
        stream: if stream { Some(true) } else { None },
        stream_options: if stream {
            Some(StreamOptions {
                include_usage: true,
            })
        } else {
            None
        },
        extra: request.additional_params.as_ref(),
    };

    serde_json::to_vec(&api_request)
}

/// 将中立 [`ReasoningConfig`] 映射为 OpenAI 顶层 `reasoning_effort` 字符串。
///
/// - `enabled = false` → `"none"`（显式关闭推理）；
/// - effort 档位 → 同名小写（`Max` → `"xhigh"`，OpenAI 的现行档位命名）；
/// - `enabled = true` 但未指定 effort、或未配置 reasoning → 字段省略（沿用模型默认）。
///
/// 档位是否被目标模型支持由服务端判定（不支持时请求报错），不做模型预判。
fn reasoning_config_to_effort(reasoning: Option<&ReasoningConfig>) -> Option<String> {
    let config = reasoning?;
    if !config.enabled {
        return Some("none".to_string());
    }
    config
        .effort
        .map(|e| match e {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::Max => "xhigh",
        })
        .map(str::to_string)
}

/// 将中立 [`ToolChoice`] 四态映射为 OpenAI 的 `tool_choice` wire 值；未配置时省略。
fn tool_choice_to_wire(tool_choice: Option<&ToolChoice>) -> Option<Value> {
    match tool_choice? {
        ToolChoice::Auto => Some(serde_json::json!("auto")),
        ToolChoice::None => Some(serde_json::json!("none")),
        ToolChoice::Required => Some(serde_json::json!("required")),
        ToolChoice::Named { name } => Some(serde_json::json!({
            "type": "function",
            "function": { "name": name },
        })),
    }
}

/// 将中立 [`TextFormat`] 映射为 OpenAI 的 `response_format`。
///
/// `JsonSchema` 固定 `strict: true`（保证输出合规；schema 不符合结构化输出子集时
/// 请求显式报错，优于静默不合规）。`Text` 与未配置均省略字段。
fn text_format_to_response_format(format: Option<&TextFormat>) -> Option<Value> {
    match format? {
        TextFormat::Text => None,
        TextFormat::JsonObject => Some(serde_json::json!({ "type": "json_object" })),
        TextFormat::JsonSchema { name, schema } => Some(serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": name,
                "strict": true,
                "schema": schema,
            },
        })),
    }
}

/// 将 chat 协议的 `finish_reason` 映射为中立 [`ResponseStatus`]。
///
/// 与 DeepSeek/Qwen chat 适配器保持一致：`tool_calls` 表示模型正常发起工具调用
/// （非失败），`finish_reason` 缺失视为正常完成。
fn finish_reason_to_status(reason: Option<&str>) -> ResponseStatus {
    match reason {
        Some("stop") | Some("tool_calls") | None => ResponseStatus::Completed,
        Some("length") => ResponseStatus::Incomplete,
        _ => ResponseStatus::Failed,
    }
}

/// 将 OpenAI chat 响应转换为中立 [`GenerateResult`]。
///
/// 块顺序：推理内容→`Reasoning`（在前，思考先于回答产生）、`content`→`Text`、
/// `tool_calls`→`ToolCall`。`content` 为空字符串时视同缺失，不产出空文本块。
/// 推理文本双字段兼容解析：`reasoning_content` 优先，缺失时取 `reasoning`。
fn chat_response_to_generate_result(
    api_response: OpenAiResponse,
) -> Result<GenerateResult, ProviderError> {
    let choice = api_response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| ProviderError::Response("响应中不包含任何选项".to_string()))?;

    let finish_reason = choice.finish_reason;
    let status = finish_reason_to_status(finish_reason.as_deref());

    let mut output = Vec::new();
    let reasoning = choice
        .message
        .reasoning_content
        .or(choice.message.reasoning);
    if let Some(reasoning) = reasoning {
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
impl ModelProvider for OpenAI {
    fn name(&self) -> &str {
        "openai"
    }

    async fn generate_full(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateResult, ProviderError> {
        let request_id = logging::next_request_id();
        let endpoint = self.chat_endpoint();
        let span = tracing::info_span!(
            "openai_chat_generate_full",
            provider = "openai",
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
                "发送 chat 生成请求"
            );
            // 含用户对话原文，仅 trace 级别输出。`body` 随后被 move 进请求，故在此之前取。
            trace!(
                target: "model_provider::openai",
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
                    target: "model_provider::openai",
                    request_id = %request_id,
                    provider_request_id = provider_request_id.as_deref().unwrap_or("-"),
                    status = status.as_u16(),
                    latency_ms,
                    body = %body_str,
                    "OpenAI API 返回错误状态"
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
                "chat 响应体全文"
            );

            let api_response: OpenAiResponse = serde_json::from_slice(&response_body)?;
            let result = chat_response_to_generate_result(api_response)?;

            let blocks = logging::summarize_blocks(&result.output);
            debug!(
                target: "model_provider::openai",
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
            "发送 chat 流式生成请求"
        );
        // 含用户对话原文，仅 trace 级别输出。`body` 随后被 move 进请求，故在此之前取。
        trace!(
            target: "model_provider::openai",
            request_id = %request_id,
            body = %String::from_utf8_lossy(&body),
            "chat 流式请求体全文"
        );

        let span = tracing::info_span!(
            "openai_stream_generate",
            provider = "openai",
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
            OpenAiStreamingProfile,
            span,
            model,
            request_id,
        ))
    }
}

impl OpenAI {
    /// 校验 chat wire 层无法承载的中立特性。
    ///
    /// 宽松模式（默认）`debug!` + 静默降级；严格模式返回 [`ProviderError::Request`]。
    /// `text.format`（含 json_schema）经 `response_format` 原生映射，不属于校验范围。
    fn validate_generate_request(&self, request: &GenerateRequest) -> Result<(), ProviderError> {
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
            if has_developer {
                return Err(ProviderError::Request(
                    "chat 适配器不支持 Developer role".to_string(),
                ));
            }
        } else if has_developer {
            debug!(
                target: "model_provider::openai",
                "chat 适配器不支持 Developer role，降级为 system"
            );
        }
        Ok(())
    }
}

// ============================================================================
// OpenAI 流式配置文件
// ============================================================================

/// OpenAI SSE 数据块的提供商标定 [`StreamingProfile`]。
struct OpenAiStreamingProfile;

impl StreamingProfile for OpenAiStreamingProfile {
    fn normalize_chunk(&self, data: &str) -> Result<Option<NormalizedChunk>, ProviderError> {
        let chunk: ChatSseChunk = serde_json::from_str(data)
            .map_err(|e| ProviderError::Stream(format!("解析 SSE 数据块失败: {e}")))?;

        let usage = chunk.usage.map(|u| NormalizedUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        });

        // 末帧用量 chunk：choices 为空而 usage 非空 —— 仅提取 usage，
        // 交由管线随流尾输出（`stream_options.include_usage` 请求的正是这一帧）。
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

        // 推理增量双字段兼容解析：`reasoning_content` 优先，缺失时取 `reasoning`。
        let reasoning = choice
            .delta
            .reasoning_content
            .clone()
            .or(choice.delta.reasoning.clone());

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
            reasoning,
            tool_calls,
            finish_reason: choice.finish_reason.clone(),
            usage,
        }))
    }

    // OpenAI 的工具调用以标准增量片段流式传输（首帧 index+id+name，后续按 index
    // 追加 arguments 片段），共享管线的默认累积路径即为此设计，不需要 DeepSeek 的
    // 同索引淘汰与单帧完整工具调用特判。
}

// ============================================================================
// Builder 风格配置
// ============================================================================

impl OpenAI {
    /// 开始使用自定义配置构建 OpenAI 客户端。
    pub fn builder() -> OpenAiBuilder {
        OpenAiBuilder::default()
    }
}

/// 用于构建 [`OpenAI`] 客户端的构建器。
#[derive(Default)]
pub struct OpenAiBuilder {
    api_key: Option<String>,
    base_url: Option<String>,
}

impl OpenAiBuilder {
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

    /// 构建 [`OpenAI`] 客户端。
    ///
    /// 如果未提供 API 密钥则返回错误。
    pub fn build(self) -> Result<OpenAI, ProviderError> {
        let api_key = self
            .api_key
            .ok_or_else(|| ProviderError::Request("需要提供 API 密钥".to_string()))?;

        let mut client = OpenAI::new(api_key)?;
        if let Some(url) = self.base_url {
            client = client.with_base_url(url);
        }
        Ok(client)
    }
}

// ============================================================================
// 公共模型名称常量
// ============================================================================

/// GPT-5.2 — 最强大的模型。
pub const OPENAI_GPT5_2: &str = "gpt-5.2";

/// GPT-5.1 — 能力与成本的均衡选择。
pub const OPENAI_GPT5_1: &str = "gpt-5.1";

/// GPT-5 Mini — 快速且经济。
pub const OPENAI_GPT5_MINI: &str = "gpt-5-mini";

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
            model: OPENAI_GPT5_2.to_string(),
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

    // ── 客户端构造 ──

    #[test]
    fn test_openai_with_base_url() {
        let client = OpenAI::new("sk-test-key")
            .unwrap()
            .with_base_url("https://openrouter.ai/api/v1");
        assert_eq!(client.name(), "openai");
        assert_eq!(client.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(
            client.chat_endpoint(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
    }

    #[test]
    fn test_default_endpoint_contains_v1() {
        let client = OpenAI::new("sk-test-key").unwrap();
        assert_eq!(
            client.chat_endpoint(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert!(client.auth_header().starts_with("Bearer sk-test-key"));
    }

    #[test]
    fn test_from_env_missing_key_errors() {
        // 仅本测试操作该变量；保存并恢复以最小化对并行测试的干扰
        let saved = std::env::var("OPENAI_API_KEY").ok();
        unsafe { std::env::remove_var("OPENAI_API_KEY") };
        let result = OpenAI::from_env();
        if let Some(key) = saved {
            unsafe { std::env::set_var("OPENAI_API_KEY", key) };
        }
        let err = result.err().expect("missing OPENAI_API_KEY must error");
        assert!(
            err.to_string().contains("OPENAI_API_KEY"),
            "unexpected message: {err}"
        );
    }

    // ── 请求体映射 ──

    #[test]
    fn test_openai_build_request_body() {
        // 非流式请求
        let request = text_request();
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["model"], "gpt-5.2");
        assert_eq!(json["temperature"], 0.7);
        assert_eq!(json["messages"][0]["role"], "system");
        assert_eq!(json["messages"][0]["content"], "You are helpful.");
        assert!(json.get("stream").is_none());
        assert!(json.get("stream_options").is_none());
        assert_eq!(json["messages"].as_array().unwrap().len(), 2);

        // 流式请求
        let body = build_request_body(&request, true).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["stream"], true);
        assert_eq!(json["stream_options"]["include_usage"], true);
    }

    #[test]
    fn test_openai_max_completion_tokens_replaces_max_tokens() {
        let request = GenerateRequest {
            max_output_tokens: Some(4096),
            ..text_request()
        };
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        // 生成预算字段是 max_completion_tokens（推理 token 计入其中）
        assert_eq!(json["max_completion_tokens"], 4096);
        // 旧字段 max_tokens 与推理模型不兼容，绝不允许出现在请求体中
        assert!(json.get("max_tokens").is_none());
    }

    #[test]
    fn test_openai_top_p_conditional_serialization() {
        let request = GenerateRequest {
            top_p: Some(0.9),
            ..text_request()
        };
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["top_p"], 0.9);

        // 未配置时字段省略
        let request = text_request();
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("top_p").is_none());
    }

    #[test]
    fn test_openai_reasoning_effort_mapping() {
        let effort_request = |enabled: bool, effort: Option<ReasoningEffort>| -> GenerateRequest {
            GenerateRequest {
                reasoning: Some(ReasoningConfig { enabled, effort }),
                ..text_request()
            }
        };

        // enabled=false → "none"（显式关闭推理）
        let body = build_request_body(&effort_request(false, None), false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["reasoning_effort"], "none");

        for (effort, expected) in [
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::Max, "xhigh"),
        ] {
            let body = build_request_body(&effort_request(true, Some(effort)), false).unwrap();
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["reasoning_effort"], expected, "effort {effort:?}");
        }

        // enabled=true 但未指定 effort → 字段省略（沿用模型默认）
        let body = build_request_body(&effort_request(true, None), false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("reasoning_effort").is_none());

        // 未配置 reasoning → 字段省略
        let body = build_request_body(&text_request(), false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("reasoning_effort").is_none());
    }

    #[test]
    fn test_openai_reasoning_min_budget_hint_keeps_value_as_is() {
        // 推理生效且预算低于建议下限：仅 debug! 提示，不改写用户配置
        let request = GenerateRequest {
            max_output_tokens: Some(4096),
            reasoning: Some(ReasoningConfig {
                enabled: true,
                effort: Some(ReasoningEffort::High),
            }),
            ..text_request()
        };
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["max_completion_tokens"], 4096);

        // enabled=false（"none"）不属于推理生效，同样保持原值
        let request = GenerateRequest {
            max_output_tokens: Some(4096),
            reasoning: Some(ReasoningConfig {
                enabled: false,
                effort: None,
            }),
            ..text_request()
        };
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["max_completion_tokens"], 4096);
    }

    #[test]
    fn test_openai_tool_choice_mapping() {
        let choice_request = |choice: Option<ToolChoice>| -> GenerateRequest {
            GenerateRequest {
                tool_choice: choice,
                ..text_request()
            }
        };

        let cases: [(ToolChoice, Value); 4] = [
            (ToolChoice::Auto, serde_json::json!("auto")),
            (ToolChoice::None, serde_json::json!("none")),
            (ToolChoice::Required, serde_json::json!("required")),
            (
                ToolChoice::Named {
                    name: "get_weather".to_string(),
                },
                serde_json::json!({"type": "function", "function": {"name": "get_weather"}}),
            ),
        ];
        for (choice, expected) in cases {
            let body = build_request_body(&choice_request(Some(choice)), false).unwrap();
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["tool_choice"], expected);
        }

        // 未配置时字段省略
        let body = build_request_body(&choice_request(None), false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("tool_choice").is_none());
    }

    #[test]
    fn test_openai_response_format_mapping() {
        let format_request = |format: Option<TextFormat>| -> GenerateRequest {
            GenerateRequest {
                text: Some(crate::response::TextConfig { format }),
                ..text_request()
            }
        };

        // JsonObject → {"type":"json_object"}
        let body =
            build_request_body(&format_request(Some(TextFormat::JsonObject)), false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["response_format"],
            serde_json::json!({"type": "json_object"})
        );

        // JsonSchema → {"type":"json_schema","json_schema":{name,strict:true,schema}}
        let body = build_request_body(
            &format_request(Some(TextFormat::JsonSchema {
                name: "result".to_string(),
                schema: serde_json::json!({"type": "object", "properties": {}}),
            })),
            false,
        )
        .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["response_format"],
            serde_json::json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "result",
                    "strict": true,
                    "schema": {"type": "object", "properties": {}},
                },
            })
        );

        // Text 与未配置 → 字段省略
        for format in [None, Some(TextFormat::Text)] {
            let body = build_request_body(&format_request(format), false).unwrap();
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert!(json.get("response_format").is_none());
        }
    }

    #[test]
    fn test_openai_build_request_body_with_tools() {
        let tool = ToolDefinition {
            name: "get_weather".to_string(),
            description: "获取天气信息".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        };
        let request = GenerateRequest {
            tools: vec![tool],
            ..text_request()
        };
        // 流式与非流式同样携带 tools（OpenAI 的流式工具调用是标准 ReAct 路径）
        for stream in [false, true] {
            let body = build_request_body(&request, stream).unwrap();
            let json: Value = serde_json::from_slice(&body).unwrap();
            let tools = json["tools"].as_array().unwrap();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0]["type"], "function");
            assert_eq!(tools[0]["function"]["name"], "get_weather");
        }
    }

    /// 借用版 `WireToolCall` 的 JSON 形状必须与 owned [`ToolCall`] 逐字段一致。
    #[test]
    fn test_openai_wire_tool_call_serialization_shape() {
        let request = GenerateRequest {
            model: OPENAI_GPT5_2.to_string(),
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
    fn test_openai_no_trailing_user_appended() {
        // OpenAI chat 无"末尾必须 user"的约束：工具轮末条是 tool 消息时保持原样，
        // 不追加空 user 消息
        let request = GenerateRequest {
            model: OPENAI_GPT5_2.to_string(),
            instructions: None,
            input: vec![
                Arc::new(InputItem::Message {
                    role: Role::User,
                    content: "天气怎么样？".to_string(),
                }),
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
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["role"], "tool");
    }

    // ── 特性校验 ──

    #[test]
    fn test_openai_developer_role_validation() {
        let request = GenerateRequest {
            model: OPENAI_GPT5_2.to_string(),
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
        };

        // 宽松模式（默认）：降级 system，不报错
        let client = OpenAI::new("sk-test-key").unwrap();
        assert!(client.validate_generate_request(&request).is_ok());

        // 严格模式：报 ProviderError::Request
        let client = OpenAI::new("sk-test-key")
            .unwrap()
            .strict_feature_validation(true);
        let err = client.validate_generate_request(&request).unwrap_err();
        assert!(matches!(err, ProviderError::Request(_)));
    }

    #[test]
    fn test_openai_json_schema_is_native_not_rejected() {
        // json_schema 经 response_format 原生映射，是受支持特性，严格模式下也不报错
        let request = GenerateRequest {
            model: OPENAI_GPT5_2.to_string(),
            instructions: None,
            input: vec![Arc::new(InputItem::Message {
                role: Role::User,
                content: "Hello".to_string(),
            })]
            .into(),
            tools: vec![],
            tool_choice: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            reasoning: None,
            text: Some(crate::response::TextConfig {
                format: Some(TextFormat::JsonSchema {
                    name: "result".to_string(),
                    schema: serde_json::json!({"type": "object", "properties": {}}),
                }),
            }),
            additional_params: None,
        };
        let client = OpenAI::new("sk-test-key")
            .unwrap()
            .strict_feature_validation(true);
        assert!(client.validate_generate_request(&request).is_ok());
    }

    // ── 响应映射 ──

    #[test]
    fn test_openai_response_deserialization() {
        let json = r#"{
            "id": "chatcmpl-123",
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
        let response: OpenAiResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.id, "chatcmpl-123");
        assert_eq!(response.choices.len(), 1);
        let msg = &response.choices[0].message;
        assert_eq!(msg.content.as_deref(), Some("Hello, how can I help?"));
        let usage = response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 15);
        assert_eq!(usage.completion_tokens, 7);
        assert_eq!(usage.total_tokens, 22);
    }

    #[test]
    fn test_openai_result_block_order_and_empty_content() {
        let api_response: OpenAiResponse = serde_json::from_str(
            r#"{
                "id": "chatcmpl-123",
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "回答",
                        "reasoning_content": "思考过程",
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "get_weather", "arguments": "{\"city\":\"SF\"}"}
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {"prompt_tokens": 9, "completion_tokens": 4, "total_tokens": 13}
            }"#,
        )
        .unwrap();

        let result = chat_response_to_generate_result(api_response).unwrap();
        // 块顺序：Reasoning 在前（思考先于回答产生）、Text、ToolCall
        assert_eq!(
            result.output,
            vec![
                ContentBlock::Reasoning {
                    text: "思考过程".to_string()
                },
                ContentBlock::Text {
                    text: "回答".to_string()
                },
                ContentBlock::ToolCall {
                    call_id: "call_1".to_string(),
                    name: "get_weather".to_string(),
                    arguments: r#"{"city":"SF"}"#.to_string(),
                },
            ]
        );
        assert_eq!(result.status, ResponseStatus::Completed);
        assert_eq!(result.usage.input_tokens, 9);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_openai_empty_content_no_empty_text_block() {
        let api_response: OpenAiResponse = serde_json::from_str(
            r#"{
                "choices": [{
                    "message": {"role": "assistant", "content": ""},
                    "finish_reason": "stop"
                }]
            }"#,
        )
        .unwrap();
        let result = chat_response_to_generate_result(api_response).unwrap();
        // 空字符串 content 视同缺失，不产出空文本块
        assert!(result.output.is_empty());
    }

    #[test]
    fn test_openai_reasoning_field_fallback() {
        // 部分兼容网关的推理文本字段名是 `reasoning`（无 `reasoning_content`）
        let api_response: OpenAiResponse = serde_json::from_str(
            r#"{
                "choices": [{
                    "message": {"role": "assistant", "content": "答", "reasoning": "思"},
                    "finish_reason": "stop"
                }]
            }"#,
        )
        .unwrap();
        let result = chat_response_to_generate_result(api_response).unwrap();
        assert_eq!(
            result.output,
            vec![
                ContentBlock::Reasoning {
                    text: "思".to_string()
                },
                ContentBlock::Text {
                    text: "答".to_string()
                },
            ]
        );

        // `reasoning_content` 优先于 `reasoning`
        let api_response: OpenAiResponse = serde_json::from_str(
            r#"{
                "choices": [{
                    "message": {"role": "assistant", "content": "答", "reasoning_content": "思一", "reasoning": "思二"},
                    "finish_reason": "stop"
                }]
            }"#,
        )
        .unwrap();
        let result = chat_response_to_generate_result(api_response).unwrap();
        assert_eq!(
            result.output[0],
            ContentBlock::Reasoning {
                text: "思一".to_string()
            }
        );
    }

    #[test]
    fn test_openai_finish_reason_mapping() {
        let with_finish = |finish_reason: &str| -> GenerateResult {
            chat_response_to_generate_result(
                serde_json::from_str(&format!(
                    r#"{{"choices":[{{"message":{{"role":"assistant","content":"x"}},"finish_reason":"{finish_reason}"}}]}}"#
                ))
                .unwrap(),
            )
            .unwrap()
        };

        assert_eq!(with_finish("stop").status, ResponseStatus::Completed);
        assert_eq!(with_finish("tool_calls").status, ResponseStatus::Completed);
        assert_eq!(with_finish("length").status, ResponseStatus::Incomplete);
        // 其它（含 content_filter）→ Failed，详情进 GenerateResult.error
        let failed = with_finish("content_filter");
        assert_eq!(failed.status, ResponseStatus::Failed);
        assert_eq!(failed.error.unwrap().message, "content_filter");
    }

    #[test]
    fn test_openai_missing_finish_reason_and_usage_defaults() {
        let api_response: OpenAiResponse = serde_json::from_str(
            r#"{
                "choices": [{"message": {"role": "assistant", "content": "x"}}]
            }"#,
        )
        .unwrap();
        let result = chat_response_to_generate_result(api_response).unwrap();
        // finish_reason 缺失视为正常完成；usage 缺失为默认零值
        assert_eq!(result.status, ResponseStatus::Completed);
        assert_eq!(result.usage, Usage::default());
        assert_eq!(result.id, "");
    }

    #[test]
    fn test_openai_no_choices_is_response_error() {
        let api_response: OpenAiResponse = serde_json::from_str(r#"{"choices": []}"#).unwrap();
        let err = chat_response_to_generate_result(api_response).unwrap_err();
        assert!(matches!(err, ProviderError::Response(ref m) if m == "响应中不包含任何选项"));
    }

    // ── SSE chunk 反序列化 ──

    #[test]
    fn test_openai_stream_chunk_deserialization_text_delta() {
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
    fn test_openai_stream_chunk_deserialization_reasoning_fallback() {
        // 网关推理增量字段名 `reasoning`（无 `reasoning_content`）
        let json = r#"{
            "choices": [{
                "index": 0,
                "delta": {"reasoning": "思考中"},
                "finish_reason": null
            }]
        }"#;
        let chunk: ChatSseChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.choices[0].delta.reasoning.as_deref(), Some("思考中"));
        assert_eq!(chunk.choices[0].delta.reasoning_content, None);
    }

    #[test]
    fn test_openai_stream_chunk_deserialization_usage_only_chunk() {
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
    fn test_openai_stream_chunk_deserialization_tool_call() {
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
    fn test_openai_normalize_chunk_maps_text_and_reasoning() {
        let data = r#"{"choices":[{"index":0,"delta":{"content":"你好","reasoning_content":"思考中"},"finish_reason":null}]}"#;
        let chunk = OpenAiStreamingProfile
            .normalize_chunk(data)
            .unwrap()
            .expect("chunk should normalize");
        assert_eq!(chunk.text.as_deref(), Some("你好"));
        assert_eq!(chunk.reasoning.as_deref(), Some("思考中"));
        assert!(chunk.tool_calls.is_empty());
        assert!(chunk.usage.is_none());
    }

    #[test]
    fn test_openai_normalize_chunk_reasoning_field_fallback() {
        let data =
            r#"{"choices":[{"index":0,"delta":{"reasoning":"思考中"},"finish_reason":null}]}"#;
        let chunk = OpenAiStreamingProfile
            .normalize_chunk(data)
            .unwrap()
            .expect("chunk should normalize");
        assert_eq!(chunk.reasoning.as_deref(), Some("思考中"));
        assert_eq!(chunk.text, None);

        // `reasoning_content` 优先于 `reasoning`
        let data = r#"{"choices":[{"index":0,"delta":{"reasoning_content":"思一","reasoning":"思二"},"finish_reason":null}]}"#;
        let chunk = OpenAiStreamingProfile
            .normalize_chunk(data)
            .unwrap()
            .expect("chunk should normalize");
        assert_eq!(chunk.reasoning.as_deref(), Some("思一"));
    }

    #[test]
    fn test_openai_normalize_chunk_usage_only_chunk_keeps_usage() {
        let data =
            r#"{"choices":[],"usage":{"prompt_tokens":6,"completion_tokens":2,"total_tokens":8}}"#;
        let chunk = OpenAiStreamingProfile
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
    fn test_openai_normalize_chunk_maps_tool_call_delta() {
        let data = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather"}}]},"finish_reason":null}]}"#;
        let chunk = OpenAiStreamingProfile
            .normalize_chunk(data)
            .unwrap()
            .expect("chunk should normalize");
        assert_eq!(chunk.tool_calls.len(), 1);
        assert_eq!(chunk.tool_calls[0].index, 0);
        assert_eq!(chunk.tool_calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(chunk.tool_calls[0].name.as_deref(), Some("get_weather"));
    }

    #[test]
    fn test_openai_normalize_chunk_skips_chunk_without_choices_and_usage() {
        let chunk = OpenAiStreamingProfile
            .normalize_chunk(r#"{"id":"chatcmpl-123"}"#)
            .unwrap();
        assert!(chunk.is_none());
    }

    #[test]
    fn test_openai_normalize_chunk_malformed_json_is_stream_error() {
        let err = OpenAiStreamingProfile
            .normalize_chunk("{not json")
            .unwrap_err();
        assert!(matches!(err, ProviderError::Stream(_)));
    }

    // ── 流式端到端（本地一次性 SSE 服务） ──

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
        client: &OpenAI,
        request: &GenerateRequest,
    ) -> Vec<Result<StreamChunk, ProviderError>> {
        let mut stream = client.generate_stream(request).await.unwrap();
        let mut chunks = Vec::new();
        while let Some(chunk) = stream.next().await {
            chunks.push(chunk);
        }
        chunks
    }

    #[tokio::test]
    async fn test_openai_stream_text_assembly_and_done() {
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
        let client = OpenAI::new("sk-test-key").unwrap().with_base_url(base);
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
    async fn test_openai_stream_tool_call_assembly() {
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
        let client = OpenAI::new("sk-test-key").unwrap().with_base_url(base);
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
    async fn test_openai_stream_error_chunk_aborts_with_api_error() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]}"#,
            r#"{"error":{"code":"invalid_request_error","message":"Invalid parameter"}}"#,
        ]))
        .await;
        let client = OpenAI::new("sk-test-key").unwrap().with_base_url(base);
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
    async fn test_openai_stream_malformed_chunk_aborts_with_stream_error() {
        let base = spawn_sse_server(sse_events(&[
            r#"{"choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]}"#,
            "{not json",
        ]))
        .await;
        let client = OpenAI::new("sk-test-key").unwrap().with_base_url(base);
        let chunks = collect_stream(&client, &text_request()).await;

        // 解析错误 → ProviderError::Stream 并中止流
        assert!(matches!(chunks.last(), Some(Err(ProviderError::Stream(_)))));
        assert!(
            !chunks
                .iter()
                .any(|c| matches!(c, Ok(StreamChunk::Finish { .. })))
        );
    }
}
