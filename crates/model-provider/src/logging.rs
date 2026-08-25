//! 日志辅助：请求/响应摘要与请求关联 id。
//!
//! 两个适配器（chat / responses）需要同一套摘要口径，集中在此以保证字段名稳定。
//!
//! ## 分级约定
//!
//! - `warn!` — HTTP 非 2xx、SSE 重连/放弃、丢弃工具调用、未闭合块降级
//! - `debug!` — 请求摘要、响应摘要、流终止摘要、状态转移、静默丢弃计数
//! - `trace!` — 完整请求体/响应体原文（含用户对话内容，显式 opt-in 才输出）
//!
//! **安全红线**：`api_key` / `Authorization` 头的值绝不能进入任何级别的日志。

use std::sync::atomic::{AtomicU64, Ordering};

use crate::response::{ContentBlock, InputItem};

/// 进程内自增的请求序号。
///
/// 只用于在单进程的交错日志里区分并发请求，因此不需要全局唯一 —— `req-42`
/// 比 UUID 可读得多，也免去一个依赖。
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 生成下一个请求关联 id（形如 `req-42`）。
pub(crate) fn next_request_id() -> String {
    let n = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("req-{n}")
}

/// 历史输入的结构摘要（按 [`InputItem`] 变体计数）。
///
/// thinking 模式的 400 多半源于 `input[]` 的排布，这些计数是判断
/// 「回放形状对不对」的第一手依据。
/// 刻意**不含**字符数合计：`chars().count()` 要对整段历史做一遍完整 UTF-8 解码，
/// 而这里的调用点在 `tracing` 宏之外（无条件执行），默认 `info` 级别下结果全被丢弃。
/// 请求规模改由同一条日志里的 `body_bytes` 表达 —— 那本来就是更准的口径。
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct InputSummary {
    pub messages: usize,
    pub function_calls: usize,
    pub function_call_outputs: usize,
    pub reasoning: usize,
}

/// 统计 [`InputItem`] 各变体数量（O(items)，不触碰文本内容）。
pub(crate) fn summarize_input(input: &[std::sync::Arc<InputItem>]) -> InputSummary {
    let mut s = InputSummary::default();
    for item in input {
        match &**item {
            InputItem::Message { .. } => s.messages += 1,
            InputItem::FunctionCall { .. } => s.function_calls += 1,
            InputItem::FunctionCallOutput { .. } => s.function_call_outputs += 1,
            InputItem::Reasoning { .. } => s.reasoning += 1,
        }
    }
    s
}

/// 输出内容块的结构摘要（按 [`ContentBlock`] 变体计数）。
///
/// 同 [`InputSummary`]，不含字符数合计 —— 输出规模由同一条日志里的 `output_tokens` 表达。
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct BlockSummary {
    pub text: usize,
    pub reasoning: usize,
    pub tool_calls: usize,
}

/// 统计 [`ContentBlock`] 各变体数量（O(blocks)，不触碰文本内容）。
pub(crate) fn summarize_blocks(blocks: &[ContentBlock]) -> BlockSummary {
    let mut s = BlockSummary::default();
    for block in blocks {
        match block {
            ContentBlock::Text { .. } => s.text += 1,
            ContentBlock::Reasoning { .. } => s.reasoning += 1,
            ContentBlock::ToolCall { .. } => s.tool_calls += 1,
        }
    }
    s
}

/// 从响应头中提取厂商的请求 id，用于向厂商报障时定位单次调用。
///
/// DeepSeek 实际使用哪个头无法离线确认，因此按 OpenAI 兼容名 + 厂商候选名
/// 依次探测；都没有时返回 `None`。
///
/// **调用时机**：必须在 `Response::bytes()` 消费响应体之前调用。
pub(crate) fn request_id_header(response: &reqwest::Response) -> Option<String> {
    const CANDIDATES: [&str; 4] = [
        "x-request-id",
        "x-ds-request-id",
        "x-deepseek-request-id",
        "x-ds-trace-id",
    ];
    CANDIDATES.iter().find_map(|name| {
        response
            .headers()
            .get(*name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::fmt::MakeWriter;

    use super::*;
    use crate::response::{GenerateRequest, Role};
    use crate::{DeepSeek, ModelProvider, ProviderError};

    // ========================================================================
    // 日志管线端到端验证
    //
    // 用一个本地假端点跑通 `generate_full` 的完整路径，断言摘要/全量体/错误字段
    // 都真的产出，以及 —— 最重要的 —— API key 绝不出现在日志里。trace 级别会把
    // 整个请求体打出来，这条红线必须由测试固定，而不是靠 review 记得。
    // ========================================================================

    /// 出现在请求里的密钥；日志中一旦出现即视为泄漏。
    const SECRET_API_KEY: &str = "sk-donotleak-abcdef123456";

    /// 把 tracing 输出收集到内存里，供断言检查。
    #[derive(Clone, Default)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl CaptureWriter {
        fn contents(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
        }
    }

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// 启动一个只服务一次请求的假端点，返回其 base_url。
    ///
    /// 固定回 401 + `x-request-id` 头，用于验证错误路径的日志字段。
    async fn spawn_once_401() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // 请求很小，一次读取足以让客户端完成发送。
            let mut buf = vec![0u8; 16 * 1024];
            let _ = socket.read(&mut buf).await;

            let body = r#"{"error":{"message":"invalid api key"}}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\n\
                 Content-Type: application/json\r\n\
                 x-request-id: srv-req-9f8e7d\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });

        format!("http://{addr}")
    }

    fn sample_request(model: &str) -> GenerateRequest {
        GenerateRequest {
            model: model.to_string(),
            instructions: Some("你是一个助手。".to_string()),
            input: vec![
                Arc::new(InputItem::Message {
                    role: Role::User,
                    content: "北京天气如何".to_string(),
                }),
                Arc::new(InputItem::Reasoning {
                    content: "需要查天气".to_string(),
                }),
                Arc::new(InputItem::FunctionCall {
                    call_id: "call_1".to_string(),
                    name: "get_weather".to_string(),
                    arguments: r#"{"city":"beijing"}"#.to_string(),
                }),
                Arc::new(InputItem::FunctionCallOutput {
                    call_id: "call_1".to_string(),
                    output: "晴 25C".to_string(),
                }),
            ]
            .into(),
            tools: vec![],
            tool_choice: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            reasoning: None,
            text: None,
            additional_params: None,
        }
    }

    #[tokio::test]
    async fn generate_full_logs_request_response_and_never_leaks_key() {
        let capture = CaptureWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("model_provider=trace"))
            .with_writer(capture.clone())
            .with_ansi(false)
            .finish();
        // `#[tokio::test]` 默认单线程 runtime，thread-local 的 set_default 足够覆盖全程，
        // 且不会污染同进程的其它测试。
        let _guard = tracing::subscriber::set_default(subscriber);

        let base_url = spawn_once_401().await;
        let provider = DeepSeek::new(SECRET_API_KEY)
            .unwrap()
            .with_base_url(&base_url);

        let err = provider
            .generate_full(&sample_request("deepseek-v4-pro"))
            .await
            .expect_err("假端点固定返回 401");
        assert!(matches!(err, ProviderError::Api { status: 401, .. }));

        let logs = capture.contents();

        // ── 红线：密钥与 Authorization 头绝不进日志 ──
        assert!(
            !logs.contains(SECRET_API_KEY),
            "API key 泄漏到日志中：\n{logs}"
        );
        assert!(
            !logs.contains("Bearer"),
            "Authorization 头泄漏到日志中：\n{logs}"
        );

        // ── 请求摘要（debug）：结构计数必须逐项落地 ──
        assert!(
            logs.contains("发送 chat 生成请求"),
            "缺少请求摘要：\n{logs}"
        );
        assert!(
            logs.contains("input_items=4"),
            "缺少 input_items 计数：\n{logs}"
        );
        assert!(logs.contains("messages=1"), "缺少 messages 计数：\n{logs}");
        assert!(
            logs.contains("function_calls=1") && logs.contains("function_call_outputs=1"),
            "缺少工具调用计数：\n{logs}"
        );
        assert!(
            logs.contains("reasoning_items=1"),
            "缺少 reasoning 计数：\n{logs}"
        );
        assert!(logs.contains("stream=false"), "缺少 stream 标记：\n{logs}");

        // ── 全量请求体（trace）：排 400 时的核心依据 ──
        assert!(
            logs.contains("chat 请求体全文"),
            "缺少 trace 级请求体：\n{logs}"
        );
        assert!(
            logs.contains("get_weather"),
            "trace 请求体未包含实际内容：\n{logs}"
        );

        // ── 错误路径（warn）：status / latency / 厂商 request-id ──
        assert!(
            logs.contains("DeepSeek API 返回错误状态"),
            "缺少非 2xx warn：\n{logs}"
        );
        assert!(logs.contains("status=401"), "warn 缺少 status：\n{logs}");
        assert!(
            logs.contains("latency_ms="),
            "warn 缺少 latency_ms：\n{logs}"
        );
        assert!(
            logs.contains("srv-req-9f8e7d"),
            "warn 缺少厂商 request-id：\n{logs}"
        );

        // ── 关联 id：同一次调用的所有日志行共享一个 request_id ──
        assert!(
            logs.contains("request_id=req-"),
            "缺少请求关联 id：\n{logs}"
        );
    }

    /// 摘要字段随输入变化，且 debug 级别不得泄漏全量请求体。
    #[tokio::test]
    async fn debug_level_summarizes_without_dumping_body() {
        let capture = CaptureWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("model_provider=debug"))
            .with_writer(capture.clone())
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let base_url = spawn_once_401().await;
        let provider = DeepSeek::new(SECRET_API_KEY)
            .unwrap()
            .with_base_url(&base_url);

        let mut request = sample_request("deepseek-v4-flash");
        request.input = Vec::new().into();
        let _ = provider.generate_full(&request).await;

        let logs = capture.contents();
        assert!(logs.contains("input_items=0"), "空历史计数不对：\n{logs}");
        assert!(
            logs.contains("model=deepseek-v4-flash"),
            "缺少 model：\n{logs}"
        );
        assert!(
            !logs.contains("chat 请求体全文"),
            "debug 级别泄漏了全量请求体：\n{logs}"
        );
        assert!(!logs.contains(SECRET_API_KEY), "API key 泄漏：\n{logs}");
    }

    #[test]
    fn request_ids_are_unique_and_prefixed() {
        let a = next_request_id();
        let b = next_request_id();
        assert!(a.starts_with("req-"));
        assert_ne!(a, b);
    }

    #[test]
    fn summarize_input_counts_each_variant() {
        let input = vec![
            Arc::new(InputItem::Message {
                role: Role::User,
                content: "你好".to_string(),
            }),
            Arc::new(InputItem::Reasoning {
                content: "think".to_string(),
            }),
            Arc::new(InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "get_weather".to_string(),
                arguments: "{}".to_string(),
            }),
            Arc::new(InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: "72F".to_string(),
            }),
        ];
        let s = summarize_input(&input);
        assert_eq!(s.messages, 1);
        assert_eq!(s.reasoning, 1);
        assert_eq!(s.function_calls, 1);
        assert_eq!(s.function_call_outputs, 1);
    }

    #[test]
    fn summarize_blocks_counts_each_variant() {
        let blocks = vec![
            ContentBlock::Text {
                text: "hi".to_string(),
            },
            ContentBlock::Reasoning {
                text: "why".to_string(),
            },
            ContentBlock::ToolCall {
                call_id: "c1".to_string(),
                name: "f".to_string(),
                arguments: "{}".to_string(),
            },
        ];
        let s = summarize_blocks(&blocks);
        assert_eq!(s.text, 1);
        assert_eq!(s.reasoning, 1);
        assert_eq!(s.tool_calls, 1);
    }
}
