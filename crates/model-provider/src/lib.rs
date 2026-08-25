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
//! use std::sync::Arc;
//!
//! use model_provider::{DeepSeek, GenerateRequest, InputItem, ModelProvider, Role};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let provider = DeepSeek::from_env()?;
//!
//!     let request = GenerateRequest {
//!         model: "deepseek-v4-pro".to_string(),
//!         instructions: Some("你是一个乐于助人的助手。".to_string()),
//!         input: vec![Arc::new(InputItem::Message {
//!             role: Role::User,
//!             content: "你好！".to_string(),
//!         })]
//!         .into(),
//!         tools: vec![],
//!         tool_choice: None,
//!         temperature: None,
//!         top_p: None,
//!         max_output_tokens: None,
//!         reasoning: None,
//!         text: None,
//!         additional_params: None,
//!     };
//!
//!     let result = provider.generate_full(&request).await?;
//!     println!("{:?}", result.status);
//!     Ok(())
//! }
//! ```

mod error;
mod generate_stream;
mod logging;
pub mod providers;
mod response;
mod streaming;
mod types;

use async_trait::async_trait;

pub use error::ProviderError;
pub use generate_stream::GenerateStream;
pub use providers::deepseek::{DeepSeek, DeepSeekChatCompletionsAdapter, DeepSeekResponsesAdapter};
pub use response::{
    BlockAssembler, BlockType, ContentBlock, FinishReason, GenerateRequest, GenerateResult,
    InputItem, ReasoningConfig, ReasoningEffort, ResponseError, ResponseStatus, Role, StreamChunk,
    TextConfig, TextFormat, ToolChoice,
};
pub use types::{ToolCall, ToolCallFunction, ToolDefinition, Usage};

/// 支持中立生成与流式传输的模型提供商。
///
/// 此 trait 使用 `#[async_trait]`，支持 `dyn ModelProvider` 用法：
///
/// ```ignore
/// let provider: Box<dyn ModelProvider> = Box::new(DeepSeek::from_env()?);
/// let result = provider.generate_full(&request).await?;
/// ```
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// 返回提供商标识（例如 `"deepseek"`、`"openai"`）。
    fn name(&self) -> &str;

    /// 发送中立非流式生成请求。
    ///
    /// 返回有序 [`ContentBlock`] 列表 + 状态 + 用量。
    async fn generate_full(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateResult, ProviderError>;

    /// 发送中立流式生成请求。
    ///
    /// 返回一个 [`GenerateStream`]，产出 [`StreamChunk`]，由 [`BlockAssembler`] 折叠。
    async fn generate_stream(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateStream, ProviderError>;
}

#[cfg(test)]
mod tests {
    use tokio as _;
}
