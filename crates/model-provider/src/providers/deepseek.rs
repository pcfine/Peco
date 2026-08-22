//! DeepSeek 模型提供商实现。
//!
//! 提供 [`DeepSeek`]，为 [DeepSeek API](https://api.deepseek.com) 实现
//! [`ModelProvider`]，使用与 OpenAI 兼容的聊天补全协议。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::providers::sse::StreamingEventSource;
use crate::providers::streaming::{
    NormalizedChunk, NormalizedToolCall, NormalizedUsage, StreamingProfile,
    process_normalized_sse_stream,
};
use crate::{
    ChatRequest, ChatResponse, ChatStream, Message, ModelProvider, ProviderError, ToolCall,
    ToolDefinition, Usage,
};

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
}

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
    messages: &'a [Arc<Message>],
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
    /// DeepSeek thinking 配置，由 provider 从 ChatRequest::reasoning_effort 构建。
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Value>,
    /// 将额外参数扁平化合并到请求体中（透传 ChatRequest::additional_params）。
    #[serde(flatten)]
    extra: Option<Value>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// DeepSeek API 非流式响应。
#[derive(Deserialize)]
struct DeepSeekResponse {
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
struct StreamChunk {
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

/// 将 DeepSeek API 消息转换为我们的公共 Message 类型。
fn convert_api_message(api_msg: DeepSeekApiMessage) -> Message {
    Message::Assistant {
        content: api_msg.content,
        tool_calls: api_msg.tool_calls,
        reasoning_content: api_msg.reasoning_content,
    }
}

/// 将 DeepSeek API 用量转换为我们的 Usage 类型。
fn convert_usage(api_usage: DeepSeekApiUsage) -> Usage {
    Usage {
        input_tokens: api_usage.prompt_tokens,
        output_tokens: api_usage.completion_tokens,
        total_tokens: api_usage.total_tokens,
    }
}

/// 构建请求体并序列化为 JSON 字节。
fn build_request_body(request: &ChatRequest, stream: bool) -> Result<Vec<u8>, serde_json::Error> {
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
    let thinking = match &request.reasoning_effort {
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
        messages: &request.messages,
        tools,
        temperature: request.temperature,
        max_tokens: request.max_tokens,
        stream: if stream { Some(true) } else { None },
        stream_options,
        tool_choice: None,
        thinking,
        extra: request.additional_params.clone(),
    };

    serde_json::to_vec(&api_request)
}

// ============================================================================
// ModelProvider 实现
// ============================================================================

#[async_trait]
impl ModelProvider for DeepSeek {
    fn name(&self) -> &str {
        "deepseek"
    }

    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let body = build_request_body(request, false)?;

        let endpoint = self.chat_endpoint();
        tracing::debug!(
            target: "model_provider::deepseek",
            "发送聊天请求到 {} (模型={})",
            endpoint,
            request.model
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
        let response_body = response.bytes().await?;

        if !status.is_success() {
            let body_str = String::from_utf8_lossy(&response_body).to_string();
            tracing::warn!(
                target: "model_provider::deepseek",
                status = status.as_u16(),
                body = %body_str,
                "DeepSeek API 返回错误状态"
            );
            return Err(ProviderError::Api {
                status: status.as_u16(),
                body: body_str,
            });
        }

        let api_response: DeepSeekResponse = serde_json::from_slice(&response_body)?;

        let choice = api_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::Response("响应中不包含任何选项".to_string()))?;

        let message = convert_api_message(choice.message);
        let usage = api_response.usage.map(convert_usage).unwrap_or_default();

        Ok(ChatResponse {
            message,
            usage,
            finish_reason: choice.finish_reason,
        })
    }

    async fn stream_chat(&self, request: &ChatRequest) -> Result<ChatStream, ProviderError> {
        let body = build_request_body(request, true)?;

        let endpoint = self.chat_endpoint();
        let model = request.model.clone();

        let span = tracing::info_span!(
            "deepseek_stream_chat",
            model = %model,
        );

        let event_source = StreamingEventSource::new(
            self.http_client.clone(),
            endpoint.clone(),
            body,
            self.auth_header(),
        );

        Ok(process_normalized_sse_stream(
            event_source,
            DeepSeekStreamingProfile,
            span,
            endpoint,
            model,
        ))
    }
}

// ============================================================================
// DeepSeek 流式配置文件
// ============================================================================

/// DeepSeek SSE 数据块的提供商标定 [`StreamingProfile`]。
struct DeepSeekStreamingProfile;

impl StreamingProfile for DeepSeekStreamingProfile {
    fn normalize_chunk(&self, data: &str) -> Result<Option<NormalizedChunk>, ProviderError> {
        let chunk: StreamChunk = serde_json::from_str(data)
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

    #[test]
    fn test_message_serde_roundtrip() {
        // 序列化 → 反序列化往返测试覆盖所有 Message 变体
        let cases = vec![
            Message::system("你是一个助手。"),
            Message::user("你好"),
            Message::assistant("你好！"),
            Message::assistant_with_tools(
                "",
                vec![ToolCall::new(
                    "call_123",
                    "get_weather",
                    r#"{"location":"NYC"}"#,
                )],
            ),
            Message::tool("call_123", "72°F，晴天"),
        ];

        for msg in &cases {
            let json = serde_json::to_string(msg).unwrap();
            let roundtripped: Message = serde_json::from_str(&json).unwrap();
            assert_eq!(&roundtripped, msg, "往返失败: {json}");
        }
    }

    #[test]
    fn test_chat_request_serialization() {
        let request = ChatRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![
                Arc::new(Message::system("You are helpful.")),
                Arc::new(Message::user("Hi")),
            ],
            tools: vec![],
            temperature: Some(0.7),
            max_tokens: Some(1024),
            reasoning_effort: None,
            additional_params: None,
        };
        let json = serde_json::to_string(&request).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["model"], "deepseek-v4-pro");
        assert_eq!(parsed["temperature"], 0.7);
        assert_eq!(parsed["max_tokens"], 1024);
        assert_eq!(parsed["messages"].as_array().unwrap().len(), 2);
    }

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
        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
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
        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
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
        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
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
        let request = ChatRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![
                Arc::new(Message::system("You are helpful.")),
                Arc::new(Message::user("Hello")),
            ],
            tools: vec![],
            temperature: Some(0.7),
            max_tokens: None,
            reasoning_effort: None,
            additional_params: None,
        };
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["model"], "deepseek-v4-pro");
        assert_eq!(json["temperature"], 0.7);
        assert!(json.get("stream").is_none());

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
        let request = ChatRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![Arc::new(Message::user("天气怎么样？"))],
            tools: vec![tool],
            temperature: None,
            max_tokens: None,
            reasoning_effort: None,
            additional_params: None,
        };
        let body = build_request_body(&request, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
    }
}
