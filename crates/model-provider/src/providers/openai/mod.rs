//! OpenAI 提供商实现（chat completions 协议与原生 Responses API，兼容网关经
//! `base_url` 覆盖接入）。

mod chat;
mod responses;

pub use chat::{
    OPENAI_GPT5_1, OPENAI_GPT5_2, OPENAI_GPT5_MINI, OpenAI, OpenAiChatCompletionsAdapter,
};
pub use responses::OpenAiResponsesAdapter;
