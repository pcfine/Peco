//! 使用 `model-provider` 的 Qwen（阿里云百炼 DashScope OpenAI 兼容模式）示例。
//!
//! ## 运行方式
//!
//! ```sh
//! export DASHSCOPE_API_KEY="sk-..."
//! cargo run -p model-provider --example qwen_chat
//! ```
//!
//! ## 注意事项
//!
//! - API 密钥与 base_url 的**地域必须一致**：新加坡 key 配 `dashscope-intl` 端点、
//!   北京 key 配 `dashscope` 端点，混用会返回 401 invalid_api_key。
//! - 工具调用仅在**非流式**路径下携带（Qwen 流式请求不支持 tools，本实现
//!   在流式路径静默丢弃 tools），故示例的第三条路径走 `generate_full`。
//!
//! ## 示例演示内容
//!
//! 1. 从环境变量创建 `Qwen` 提供商
//! 2. 构建包含系统指令和用户输入项的 `GenerateRequest`
//! 3. 非流式 `generate_full()` 调用
//! 4. 流式 `generate_stream()` 调用，逐条输出增量内容
//! 5. 非流式工具调用：模型返回 `ToolCall` 块
//! 6. 将提供商作为 `Box<dyn ModelProvider>` 传递

#![allow(unused_crate_dependencies)]

use std::sync::Arc;

use model_provider::providers::qwen::QWEN_PLUS;
use model_provider::{
    BlockAssembler, ContentBlock, GenerateRequest, GenerateResult, GenerateStream, InputItem,
    ModelProvider, ProviderError, Qwen, Role, StreamChunk,
};

/// 构造一条带系统指令与用户输入的请求。
fn request(
    user_content: &str,
    instructions: &str,
    tools: Vec<model_provider::ToolDefinition>,
) -> GenerateRequest {
    GenerateRequest {
        model: QWEN_PLUS.to_string(),
        instructions: Some(instructions.to_string()),
        input: vec![Arc::new(InputItem::Message {
            role: Role::User,
            content: user_content.to_string(),
        })]
        .into(),
        tools,
        tool_choice: None,
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
        "用一句话介绍 Rust 的所有权机制。",
        "你是一个乐于助人的助手。回答保持简洁。",
        vec![],
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
        "写一首关于 Rust 编程的俳句。",
        "你是一个乐于助人的助手。回答保持简洁。",
        vec![],
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

/// 工具调用生成示例（非流式 — Qwen 流式请求不携带 tools）。
async fn run_tool_generate(provider: &dyn ModelProvider) -> Result<(), ProviderError> {
    println!("=== Tool-Calling generate (non-streaming) ===");

    let weather_tool = model_provider::ToolDefinition {
        name: "get_weather".to_string(),
        description: "查询指定城市的当前天气".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "city": {
                    "type": "string",
                    "description": "城市名称"
                }
            },
            "required": ["city"]
        }),
    };

    let request = request(
        "旧金山现在天气怎么样？",
        "你是一个乐于助人的助手。",
        vec![weather_tool],
    );

    let result = provider.generate_full(&request).await?;
    print_blocks(&result);
    println!();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 从环境变量创建提供商（不打印任何 api_key 或 Authorization 头）
    let qwen = Qwen::from_env()?;
    println!("Provider: {}", qwen.name());
    println!();

    // 作为 `dyn ModelProvider` 使用 — 演示动态分发
    let provider: Box<dyn ModelProvider> = Box::new(qwen);

    // 运行示例
    run_generate(provider.as_ref()).await?;
    run_generate_stream(provider.as_ref()).await?;
    run_tool_generate(provider.as_ref()).await?;

    println!("所有示例执行完毕。");
    Ok(())
}
