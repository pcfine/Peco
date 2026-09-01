//! 各厂商的 [`ModelProvider`] 实现，按厂商分目录组织。
//!
//! 跨厂商共享的 SSE/流式基础设施位于 crate 顶层 `streaming` 模块，
//! 新增厂商时在此处添加对应子模块即可。

mod chat_common;

pub mod deepseek;
pub mod qwen;

pub use deepseek::{DeepSeek, DeepSeekChatCompletionsAdapter, DeepSeekResponsesAdapter};
pub use qwen::{
    QWEN_FLASH, QWEN_MAX, QWEN_PLUS, Qwen, QwenChatCompletionsAdapter, QwenResponsesAdapter,
};
