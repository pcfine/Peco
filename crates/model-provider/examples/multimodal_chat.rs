//! 多模态输入示例：用户消息携带图片部件（OpenAI chat completions 适配器）。
//!
//! ## 运行方式
//!
//! ```sh
//! export OPENAI_API_KEY="sk-..."
//! cargo run -p model-provider --example multimodal_chat
//! ```
//!
//! ## 示例演示内容
//!
//! 1. 用户消息使用 `Content::Parts` 混排文本与图片（公网图片 URL，无需下载转 base64）
//! 2. `ImageDetail` 档位（`low` / `high` / `auto`）随部件传递
//! 3. 非流式 `generate_full()` 调用
//! 4. 流式 `generate_stream()` 调用，逐条输出增量内容
//!
//! 不支持图片输入的适配器（DeepSeek 等）会剥离图片部件并 `warn!` ——
//! 换成 `DeepSeek::from_env()?` 即可观察该行为。

#![allow(unused_crate_dependencies)]

use std::sync::Arc;

use model_provider::providers::openai::OPENAI_GPT5_2;
use model_provider::{
    BlockAssembler, Content, ContentBlock, ContentPart, GenerateRequest, GenerateStream,
    ImageDetail, InputItem, ModelProvider, OpenAI, ProviderError, Role, StreamChunk,
};

/// 示例图片：公网 URL，适配器原样透传。
const SAMPLE_IMAGE_URL: &str =
    "https://upload.wikimedia.org/wikipedia/commons/thumb/3/3a/Cat03.jpg/640px-Cat03.jpg";

/// 构造一条「文本 + 图片」混排的用户请求。
fn request(question: &str, detail: ImageDetail) -> GenerateRequest {
    GenerateRequest {
        model: OPENAI_GPT5_2.to_string(),
        instructions: Some("你是图像分析助手，用中文简要回答。".to_string()),
        input: vec![Arc::new(InputItem::Message {
            role: Role::User,
            content: Content::Parts(vec![
                ContentPart::Text {
                    text: question.to_string(),
                },
                ContentPart::Image {
                    url: SAMPLE_IMAGE_URL.to_string(),
                    detail: Some(detail),
                },
            ]),
        })]
        .into(),
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

/// 非流式多模态生成示例。
async fn run_generate(provider: &dyn ModelProvider) -> Result<(), ProviderError> {
    println!("=== Non-Streaming multimodal generate ===");

    let result = provider
        .generate_full(&request("这张图片里是什么？", ImageDetail::Low))
        .await?;

    for block in &result.output {
        if let ContentBlock::Text { text } = block {
            println!("[text] {text}");
        }
    }
    println!(
        "Usage: input={}, output={}, total={}",
        result.usage.input_tokens, result.usage.output_tokens, result.usage.total_tokens
    );
    println!();
    Ok(())
}

/// 流式多模态生成示例。
async fn run_generate_stream(provider: &dyn ModelProvider) -> Result<(), ProviderError> {
    println!("=== Streaming multimodal generate_stream ===");

    let mut stream: GenerateStream = provider
        .generate_stream(&request("这张图片里的动物有什么特征？", ImageDetail::Auto))
        .await?;
    let mut assembler = BlockAssembler::new();
    print!("Streaming: ");

    while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
        let chunk = chunk?;
        match &chunk {
            StreamChunk::TextDelta { delta, .. } => print!("{delta}"),
            StreamChunk::ReasoningDelta { delta, .. } => print!("\n[reasoning: {delta}]\n"),
            _ => {}
        }
        assembler.push(chunk);
    }
    let (blocks, usage, _status, _err) = assembler.finish();
    println!();
    for block in &blocks {
        if let ContentBlock::Text { text } = block {
            println!("[final text] {text}");
        }
    }
    println!(
        "Stream finished. Usage: input={}, output={}, total={}",
        usage.input_tokens, usage.output_tokens, usage.total_tokens
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 从环境变量创建提供商（支持 OPENAI_BASE_URL 覆盖为兼容网关）
    let openai = OpenAI::from_env()?;
    println!("Provider: {}", openai.name());
    println!("Image: {SAMPLE_IMAGE_URL}");
    println!();

    let provider: Box<dyn ModelProvider> = Box::new(openai);

    run_generate(provider.as_ref()).await?;
    run_generate_stream(provider.as_ref()).await?;

    println!("所有示例执行完毕。");
    Ok(())
}
