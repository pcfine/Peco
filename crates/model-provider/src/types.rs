use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// 聊天对话中的一条消息，遵循 OpenAI/DeepSeek 的传输格式。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    /// 系统指令消息。
    System { content: String },
    /// 用户消息。
    User { content: String },
    /// 助手回复消息，可能包含工具调用。
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
    },
    /// 工具执行结果消息。
    #[serde(rename = "tool")]
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl Message {
    /// 创建新的系统消息。
    pub fn system(content: impl Into<String>) -> Self {
        Self::System {
            content: content.into(),
        }
    }

    /// 创建新的用户消息。
    pub fn user(content: impl Into<String>) -> Self {
        Self::User {
            content: content.into(),
        }
    }

    /// 创建包含文本内容的新助手消息。
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::Assistant {
            content: Some(content.into()),
            tool_calls: None,
            reasoning_content: None,
        }
    }

    /// 创建包含工具调用的新助手消息。
    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self::Assistant {
            content: Some(content.into()),
            tool_calls: Some(tool_calls),
            reasoning_content: None,
        }
    }

    /// 创建新的工具结果消息。
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
        }
    }

    /// 返回此消息的角色名称（"system"、"user"、"assistant"、"tool"）。
    pub fn role_name(&self) -> &'static str {
        match self {
            Message::System { .. } => "system",
            Message::User { .. } => "user",
            Message::Assistant { .. } => "assistant",
            Message::Tool { .. } => "tool",
        }
    }

    /// 返回此消息的文本内容（如果有）。
    pub fn content(&self) -> Option<&str> {
        match self {
            Message::System { content } => Some(content.as_str()),
            Message::User { content } => Some(content.as_str()),
            Message::Assistant { content, .. } => content.as_deref(),
            Message::Tool { content, .. } => Some(content.as_str()),
        }
    }

    /// 返回此助手消息中的工具调用（如果有）。
    pub fn tool_calls(&self) -> Option<&Vec<ToolCall>> {
        match self {
            Message::Assistant { tool_calls, .. } => tool_calls.as_ref(),
            _ => None,
        }
    }

    /// 返回此助手消息的推理内容（如果有）。
    pub fn reasoning_content(&self) -> Option<&str> {
        match self {
            Message::Assistant {
                reasoning_content, ..
            } => reasoning_content.as_deref(),
            _ => None,
        }
    }

    /// 返回此工具消息的 tool_call_id（如适用）。
    pub fn tool_call_id(&self) -> Option<&str> {
        match self {
            Message::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        }
    }
}

/// 助手发起的工具调用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    /// 此工具调用的唯一标识符。
    pub id: String,
    /// 工具调用类型（通常为 "function"）。
    #[serde(rename = "type")]
    pub call_type: String,
    /// 被调用的函数。
    pub function: ToolCallFunction,
}

impl ToolCall {
    /// 为某个函数创建新的工具调用。
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }
}

/// 工具调用中的函数详情。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallFunction {
    /// 要调用的函数名称。
    pub name: String,
    /// 函数的 JSON 编码参数。
    pub arguments: String,
}

/// 可传递给模型的工具定义。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    /// 工具名称。
    pub name: String,
    /// 对该工具功能的人类可读描述。
    pub description: String,
    /// 描述该工具参数的 JSON Schema。
    pub parameters: serde_json::Value,
}

/// 提供商返回的 token 用量信息。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    /// 输入/提示的 token 数量。
    pub input_tokens: u32,
    /// 输出/补全的 token 数量。
    pub output_tokens: u32,
    /// 使用的 token 总数。
    pub total_tokens: u32,
}

/// 聊天补全请求。
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    /// 要使用的模型标识符。
    pub model: String,
    /// 对话消息列表。
    pub messages: Vec<Arc<Message>>,
    /// 可选的工具定义，用于函数调用。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    /// 可选的温度参数，控制回复的随机性。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// 可选的生成 token 最大数量。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// 推理力度 `"low"`, `"high"`, `"max"`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// 提供商标定的额外参数
    #[serde(skip_serializing_if = "Option::is_none", flatten)]
    pub additional_params: Option<serde_json::Value>,
}

/// 聊天补全响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    /// 助手的回复消息。
    pub message: Message,
    /// token 用量信息。
    pub usage: Usage,
    /// 模型停止生成的原因（例如 "stop"、"length"、"tool_calls"）。
    pub finish_reason: Option<String>,
}
