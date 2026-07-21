//! # model-provider
//!
//! 通过 `#[async_trait]` 支持 `dyn ModelProvider` 用法的可替代模型提供商抽象。
//!
//! `ModelProvider` 使用 `async-trait` 将 future 装箱，
//! 使得提供商可以作为 `Box<dyn ModelProvider>` 存储和传递。
//!
//! ## 用法
//!
//! ```ignore
//! use model_provider::{DeepSeek, ModelProvider, ChatRequest, Message};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let provider = DeepSeek::from_env()?;
//!
//!     let request = ChatRequest {
//!         model: "deepseek-v4-pro".to_string(),
//!         messages: vec![
//!             Message::system("你是一个乐于助人的助手。"),
//!             Message::user("你好！"),
//!         ],
//!         tools: vec![],
//!         temperature: None,
//!         max_tokens: None,
//!         reasoning_effort: None,
//!         additional_params: None,
//!     };
//!
//!     let response = provider.chat(&request).await?;
//!     println!("{}", response.message.content().unwrap_or_default());
//!     Ok(())
//! }
//! ```

mod error;
pub mod providers;
mod stream;
mod types;

use async_trait::async_trait;

pub use error::ProviderError;
pub use providers::deepseek::DeepSeek;
pub use stream::{ChatStream, StreamEvent};
pub use types::{
    ChatRequest, ChatResponse, Message, ToolCall, ToolCallFunction, ToolDefinition, Usage,
};

/// 支持聊天补全和流式传输的模型提供商。
///
/// 此 trait 使用 `#[async_trait]`，支持 `dyn ModelProvider` 用法：
///
/// ```ignore
/// let provider: Box<dyn ModelProvider> = Box::new(DeepSeek::from_env()?);
/// let response = provider.chat(&request).await?;
/// ```
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// 返回提供商标识（例如 `"deepseek"`、`"openai"`）。
    fn name(&self) -> &str;

    /// 发送非流式聊天补全请求。
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, ProviderError>;

    /// 发送流式聊天补全请求。
    ///
    /// 返回一个 [`ChatStream`]，它会随着从提供商通过 SSE 接收到的数据
    /// 逐条产出 [`StreamEvent`] 项。
    async fn stream_chat(&self, request: &ChatRequest) -> Result<ChatStream, ProviderError>;
}

#[cfg(test)]
mod tests {
    use tokio as _;
}
