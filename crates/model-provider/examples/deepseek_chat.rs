//! 使用 `model-provider` 的 DeepSeek 聊天示例。
//!
//! ## 运行方式
//!
//! ```sh
//! export DEEPSEEK_API_KEY="sk-..."
//! cargo run -p model-provider --example deepseek_chat
//! ```
//!
//! ## 示例演示内容
//!
//! 1. 从环境变量创建 `DeepSeek` 提供商
//! 2. 构建包含系统消息和用户消息的 `ChatRequest`
//! 3. 非流式 `chat()` 调用
//! 4. 流式 `stream_chat()` 调用，逐条输出增量内容
//! 5. 将提供商作为 `Box<dyn ModelProvider>` 传递

#![allow(unused_crate_dependencies)]

use std::sync::Arc;

use model_provider::providers::deepseek::DEEPSEEK_V4_PRO;
use model_provider::{
    ChatRequest, ChatStream, DeepSeek, Message, ModelProvider, ProviderError, StreamEvent,
};

/// 辅助函数：打印用量信息。
fn print_usage(response: &model_provider::ChatResponse) {
    let content = match &response.message {
        Message::Assistant { content, .. } => content.as_deref().unwrap_or("(empty)"),
        _ => "(not an assistant message)",
    };
    println!("Response: {content}");
    println!(
        "Usage: input={}, output={}, total={}",
        response.usage.input_tokens, response.usage.output_tokens, response.usage.total_tokens
    );
}

/// 非流式聊天示例。
async fn run_chat(provider: &dyn ModelProvider) -> Result<(), ProviderError> {
    println!("=== Non-Streaming Chat ===");

    let request = ChatRequest {
        model: DEEPSEEK_V4_PRO.to_string(),
        messages: vec![
            Arc::new(Message::system(
                "You are a helpful assistant. Keep answers concise.",
            )),
            Arc::new(Message::user(
                "What are the top 3 programming languages in 2026?",
            )),
        ],
        tools: vec![],
        temperature: Some(0.7),
        max_tokens: Some(256),
        reasoning_effort: None,
        additional_params: None,
    };

    let response = provider.chat(&request).await?;
    print_usage(&response);
    println!();
    Ok(())
}

/// 流式聊天示例。
async fn run_stream_chat(provider: &dyn ModelProvider) -> Result<(), ProviderError> {
    println!("=== Streaming Chat ===");

    let request = ChatRequest {
        model: DEEPSEEK_V4_PRO.to_string(),
        messages: vec![
            Arc::new(Message::system(
                "You are a helpful assistant. Keep answers concise.",
            )),
            Arc::new(Message::user("Write a haiku about Rust programming.")),
        ],
        tools: vec![],
        temperature: Some(0.9),
        max_tokens: Some(128),
        reasoning_effort: None,
        additional_params: None,
    };

    let mut stream: ChatStream = provider.stream_chat(&request).await?;
    print!("Streaming: ");

    while let Some(event) = futures::StreamExt::next(&mut stream).await {
        match event? {
            StreamEvent::TextDelta(text) => {
                print!("{text}");
            }
            StreamEvent::ReasoningDelta(reasoning) => {
                print!("\n[reasoning: {reasoning}]\n");
            }
            StreamEvent::ToolCallDelta {
                id,
                name,
                arguments,
            } => match &arguments {
                serde_json::Value::String(s) => {
                    println!("\n[ToolCall {id}]: {name:?} (fragment: {s})");
                }
                other => {
                    println!("\n[ToolCall {id}]: {name:?} ({other})");
                }
            },
            StreamEvent::End { usage } => {
                println!();
                println!(
                    "Stream finished. Usage: input={}, output={}, total={}",
                    usage.input_tokens, usage.output_tokens, usage.total_tokens
                );
            }
            StreamEvent::ToolCallComplete(tc) => {
                println!(
                    "\n[ToolCall assembled] {}: {} ({})",
                    tc.id, tc.function.name, tc.function.arguments
                );
            }
        }
    }
    println!();
    Ok(())
}

/// 工具调用聊天示例。
async fn run_tool_chat(provider: &dyn ModelProvider) -> Result<(), ProviderError> {
    println!("=== Tool-Calling Chat ===");

    let weather_tool = model_provider::ToolDefinition {
        name: "get_weather".to_string(),
        description: "Get current weather for a city".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "city": {
                    "type": "string",
                    "description": "The city name"
                }
            },
            "required": ["city"]
        }),
    };

    let request = ChatRequest {
        model: DEEPSEEK_V4_PRO.to_string(),
        messages: vec![Arc::new(Message::user(
            "What's the weather like in San Francisco?",
        ))],
        tools: vec![weather_tool],
        temperature: Some(0.0),
        max_tokens: Some(256),
        reasoning_effort: None,
        additional_params: None,
    };

    let response = provider.chat(&request).await?;
    match &response.message {
        Message::Assistant {
            content,
            tool_calls,
            reasoning_content: _,
        } => {
            if let Some(text) = content {
                println!("Assistant text: {text}");
            }
            if let Some(calls) = tool_calls {
                for tc in calls {
                    println!(
                        "Tool call: id={}, function={}, arguments={}",
                        tc.id, tc.function.name, tc.function.arguments
                    );
                }
            }
        }
        _ => println!("Unexpected message type"),
    }
    println!(
        "Usage: input={}, output={}, total={}",
        response.usage.input_tokens, response.usage.output_tokens, response.usage.total_tokens
    );
    println!();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 从环境变量创建提供商
    let deepseek = DeepSeek::from_env()?;
    println!("Provider: {}", deepseek.name());
    println!();

    // 作为 `dyn ModelProvider` 使用 — 演示动态分发
    let provider: Box<dyn ModelProvider> = Box::new(deepseek);

    // 运行示例
    run_chat(provider.as_ref()).await?;
    run_stream_chat(provider.as_ref()).await?;
    run_tool_chat(provider.as_ref()).await?;

    println!("所有示例执行完毕。");
    Ok(())
}
