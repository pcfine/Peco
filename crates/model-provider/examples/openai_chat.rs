//! 使用 `model-provider` 的 OpenAI 中立生成示例（chat completions 适配器）。
//!
//! ## 运行方式
//!
//! ```sh
//! export OPENAI_API_KEY="sk-..."
//! cargo run -p model-provider --example openai_chat
//! ```
//!
//! ## 示例演示内容
//!
//! 1. 从环境变量创建 `OpenAI` 提供商
//! 2. 构建包含系统指令和用户输入项的 `GenerateRequest`
//! 3. 非流式 `generate_full()` 调用
//! 4. 流式 `generate_stream()` 调用，逐条输出增量内容
//! 5. 工具调用路径：携带 `ToolDefinition` 与非空 `tool_choice`
//! 6. 将提供商作为 `Box<dyn ModelProvider>` 传递

#![allow(unused_crate_dependencies)]

use std::sync::Arc;

use model_provider::providers::openai::OPENAI_GPT5_2;
use model_provider::{
    BlockAssembler, ContentBlock, GenerateRequest, GenerateResult, GenerateStream, InputItem,
    ModelProvider, OpenAI, ProviderError, Role, StreamChunk, ToolChoice, ToolDefinition,
};

/// 构造一条带系统指令与用户输入的请求。
fn request(
    user_content: &str,
    instructions: &str,
    tools: Vec<ToolDefinition>,
    tool_choice: Option<ToolChoice>,
) -> GenerateRequest {
    GenerateRequest {
        model: OPENAI_GPT5_2.to_string(),
        instructions: Some(instructions.to_string()),
        input: vec![Arc::new(InputItem::Message {
            role: Role::User,
            content: user_content.to_string(),
        })]
        .into(),
        tools,
        tool_choice,
        temperature: Some(0.7),
        top_p: None,
        max_output_tokens: Some(256),
        reasoning: None,
        text: None,
        additional_params: None,
    }
}

/// 打印有序内容块。
fn print_blocks(result: &GenerateResult) {
    for block in &result.output {
        match block {
            ContentBlock::Text { text } => println!("[text] {text}"),
            ContentBlock::Reasoning { text } => println!("[reasoning] {text}"),
            ContentBlock::ToolCall {
                call_id,
                name,
                arguments,
            } => println!("[tool_call] {name} ({call_id}): {arguments}"),
            _ => println!("[other block]"),
        }
    }
    println!(
        "Usage: input={}, output={}, total={}, status={:?}",
        result.usage.input_tokens,
        result.usage.output_tokens,
        result.usage.total_tokens,
        result.status
    );
}

/// 非流式生成示例。
async fn run_generate(provider: &dyn ModelProvider) -> Result<(), ProviderError> {
    println!("=== Non-Streaming generate ===");

    let request = request(
        "What are the top 3 programming languages in 2026?",
        "You are a helpful assistant. Keep answers concise.",
        vec![],
        None,
    );

    let result = provider.generate_full(&request).await?;
    print_blocks(&result);
    println!();
    Ok(())
}

/// 流式生成示例。
async fn run_generate_stream(provider: &dyn ModelProvider) -> Result<(), ProviderError> {
    println!("=== Streaming generate_stream ===");

    let request = request(
        "Write a haiku about Rust programming.",
        "You are a helpful assistant. Keep answers concise.",
        vec![],
        None,
    );

    let mut stream: GenerateStream = provider.generate_stream(&request).await?;
    let mut assembler = BlockAssembler::new();
    print!("Streaming: ");

    while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
        let chunk = chunk?;
        match &chunk {
            StreamChunk::TextDelta { delta, .. } => print!("{delta}"),
            StreamChunk::ReasoningDelta { delta, .. } => print!("\n[reasoning: {delta}]\n"),
            StreamChunk::ToolCallDelta { call_id, name, .. } => {
                print!("\n[tool_call {call_id}] {name:?}");
            }
            _ => {}
        }
        assembler.push(chunk);
    }
    let (blocks, usage, status, _err) = assembler.finish();
    println!();
    for block in &blocks {
        if let ContentBlock::Text { text } = block {
            println!("[final text] {text}");
        }
    }
    println!(
        "Stream finished. Usage: input={}, output={}, total={}, status={:?}",
        usage.input_tokens, usage.output_tokens, usage.total_tokens, status
    );
    println!();
    Ok(())
}

/// 工具调用生成示例（携带非空 `tool_choice`，演示其 wire 映射）。
async fn run_tool_generate(provider: &dyn ModelProvider) -> Result<(), ProviderError> {
    println!("=== Tool-Calling generate ===");

    let weather_tool = ToolDefinition {
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

    let request = request(
        "What's the weather like in San Francisco?",
        "You are a helpful assistant.",
        vec![weather_tool],
        Some(ToolChoice::Auto),
    );

    let result = provider.generate_full(&request).await?;
    print_blocks(&result);
    println!();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 从环境变量创建提供商
    let openai = OpenAI::from_env()?;
    println!("Provider: {}", openai.name());
    println!();

    // 作为 `dyn ModelProvider` 使用 — 演示动态分发
    let provider: Box<dyn ModelProvider> = Box::new(openai);

    // 运行示例
    run_generate(provider.as_ref()).await?;
    run_generate_stream(provider.as_ref()).await?;
    run_tool_generate(provider.as_ref()).await?;

    println!("所有示例执行完毕。");
    Ok(())
}
