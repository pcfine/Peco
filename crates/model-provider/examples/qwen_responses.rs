//! Qwen（阿里云百炼 DashScope）的中立生成 API 冒烟示例（responses + chat 双路径）。
//!
//! ## 运行方式
//!
//! ```sh
//! export DASHSCOPE_API_KEY="sk-..."
//! cargo run -p model-provider --example qwen_responses
//! ```
//!
//! ## 示例演示内容
//!
//! 1. `QwenResponsesAdapter` 的非流式 `generate_full()` 与流式 `generate_stream()`
//! 2. chat 适配器 `Qwen` 的 `generate_full()`（复用 chat/completions）
//! 3. 用 `BlockAssembler` 折叠流式 `StreamChunk` 为有序 `Vec<ContentBlock>`
//!
//! responses 路径的输出含 `[reasoning]`（百炼以 `summary` 摘要形式返回思考内容），
//! 可用于观察百炼 responses 的默认 effort 档位、reasoning summary 形态等行为。

#![allow(unused_crate_dependencies)]

use std::sync::Arc;

use futures::StreamExt;
use model_provider::providers::qwen::QWEN_MAX;
use model_provider::{
    BlockAssembler, ContentBlock, GenerateRequest, InputItem, ModelProvider, Qwen,
    QwenResponsesAdapter, Role, StreamChunk,
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
                name, arguments, ..
            } => {
                println!("[tool_call] {name}({arguments})")
            }
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
            StreamChunk::ReasoningDelta { delta, .. } => {
                print!("\x1b[2m{delta}\x1b[0m");
            }
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = std::env::var("MODEL").unwrap_or_else(|_| QWEN_MAX.to_string());

    // 1. responses 适配器（OpenAI 兼容 /responses，保留 /v1 前缀）
    let responses = QwenResponsesAdapter::from_env()?;
    run_generate(&responses, &model).await?;
    run_stream(&responses, &model).await?;

    // 2. chat 适配器（复用 compatible-mode/v1/chat/completions）
    let chat = Qwen::from_env()?;
    run_generate(&chat, &model).await?;
    run_stream(&chat, &model).await?;

    Ok(())
}
