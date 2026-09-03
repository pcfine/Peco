//! 使用 `model-provider` 的 OpenAI 原生 Responses API 中立生成示例。
//!
//! ## 运行方式
//!
//! ```sh
//! export OPENAI_API_KEY="sk-..."
//! cargo run -p model-provider --example openai_responses
//! # 可用 MODEL 环境变量覆盖默认模型（gpt-5.2）
//! ```
//!
//! ## 示例演示内容
//!
//! 1. `OpenAiResponsesAdapter` 的非流式 `generate_full()` 与流式 `generate_stream()`
//! 2. 用 `BlockAssembler` 折叠流式 `StreamChunk` 为有序 `Vec<ContentBlock>`
//! 3. 工具调用路径：携带扁平 `ToolDefinition` 与 `ToolChoice::Auto`
//!
//! 注意：示例仅演示文本/工具路径，不含多模态内容部件（中立层尚未承载）。

#![allow(unused_crate_dependencies)]

use std::sync::Arc;

use futures::StreamExt;
use model_provider::providers::openai::OPENAI_GPT5_2;
use model_provider::{
    BlockAssembler, ContentBlock, GenerateRequest, InputItem, ModelProvider,
    OpenAiResponsesAdapter, Role, StreamChunk, ToolChoice, ToolDefinition,
};

fn make_request(model: &str, question: &str) -> GenerateRequest {
    GenerateRequest {
        model: model.to_string(),
        instructions: Some("You are a helpful assistant. Keep answers concise.".to_string()),
        input: Arc::from([Arc::new(InputItem::Message {
            role: Role::User,
            content: question.to_string(),
        })]),
        tools: vec![],
        tool_choice: None,
        temperature: Some(0.7),
        top_p: None,
        max_output_tokens: Some(256),
        reasoning: None,
        text: None,
        additional_params: None,
    }
}

fn print_blocks(label: &str, blocks: &[ContentBlock]) {
    println!("--- {label} ---");
    for block in blocks {
        match block {
            ContentBlock::Text { text } => println!("[text] {text}"),
            ContentBlock::Reasoning { text } => println!("[reasoning] {text}"),
            ContentBlock::ToolCall {
                call_id,
                name,
                arguments,
            } => println!("[tool_call] {name} ({call_id}): {arguments}"),
            _ => {}
        }
    }
}

/// 非流式 `generate_full()`。
async fn run_generate(
    provider: &dyn ModelProvider,
    model: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Non-Streaming generate ===");
    let result = provider
        .generate_full(&make_request(
            model,
            "What is 2 + 2? Answer in one sentence.",
        ))
        .await?;
    println!("id={} status={:?}", result.id, result.status);
    print_blocks("output", &result.output);
    Ok(())
}

/// 流式 `generate_stream()` + `BlockAssembler` 折叠。
async fn run_stream(
    provider: &dyn ModelProvider,
    model: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Streaming generate ===");
    let mut stream = provider
        .generate_stream(&make_request(model, "Count from 1 to 5."))
        .await?;

    let mut assembler = BlockAssembler::new();
    while let Some(chunk) = stream.next().await {
        match chunk? {
            StreamChunk::BlockStart { index, block_type } => {
                println!("  [start] index={index} type={block_type:?}");
                assembler.push(StreamChunk::BlockStart { index, block_type });
            }
            StreamChunk::TextDelta { delta, .. } => {
                print!("{delta}");
            }
            StreamChunk::ReasoningDelta { .. } => {}
            StreamChunk::ToolCallDelta { .. } => {}
            StreamChunk::BlockEnd { index, block } => {
                println!("\n  [end] index={index} block={block:?}");
                assembler.push(StreamChunk::BlockEnd { index, block });
            }
            StreamChunk::Usage { usage } => {
                println!("  [usage] {usage:?}");
                assembler.push(StreamChunk::Usage { usage });
            }
            StreamChunk::Finish { reason } => {
                println!("  [finish] {reason:?}");
                assembler.push(StreamChunk::Finish { reason });
            }
        }
    }

    let (blocks, usage, status, error) = assembler.finish();
    println!("\n--- assembled ---");
    println!("status={status:?} error={error:?} usage={usage:?}");
    print_blocks("assembled", &blocks);
    Ok(())
}

/// 工具调用生成示例（`ToolChoice::Auto`，演示 Responses 扁平工具定义的 wire 映射）。
async fn run_tool_generate(
    provider: &dyn ModelProvider,
    model: &str,
) -> Result<(), Box<dyn std::error::Error>> {
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

    let mut request = make_request(model, "What's the weather like in San Francisco?");
    request.tools = vec![weather_tool];
    request.tool_choice = Some(ToolChoice::Auto);

    let result = provider.generate_full(&request).await?;
    println!("id={} status={:?}", result.id, result.status);
    print_blocks("output", &result.output);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = std::env::var("MODEL").unwrap_or_else(|_| OPENAI_GPT5_2.to_string());

    let responses = OpenAiResponsesAdapter::from_env()?;
    println!("Provider: {}", responses.name());
    println!();

    run_generate(&responses, &model).await?;
    run_stream(&responses, &model).await?;
    run_tool_generate(&responses, &model).await?;

    println!("所有示例执行完毕。");
    Ok(())
}
