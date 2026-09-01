//! Qwen（阿里云百炼 DashScope）厂商实现。

mod chat;
mod responses;

pub use chat::{QWEN_FLASH, QWEN_MAX, QWEN_PLUS, Qwen, QwenChatCompletionsAdapter};
pub use responses::QwenResponsesAdapter;
