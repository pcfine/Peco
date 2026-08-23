use serde::{Deserialize, Serialize};

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
