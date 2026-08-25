//! 跨厂商共享的流式传输基础设施（crate 内部使用）。
//!
//! - [`sse`]：SSE 传输层（`StreamingEventSource`、`RetryPolicy`）。
//! - [`pipeline`]：归一化管线（`StreamingProfile`、`process_normalized_sse_stream_chunks`）。

pub(crate) mod pipeline;

// `sse` 是一个自包含的可复用 SSE 库，其完整 API 面（如 `Constant`/`Never` 重试策略、
// `close`/`last_event_id`/`allow_missing_content_type`）面向未来厂商复用，
// 当前 crate 内尚未全部用到，故关闭未使用项告警。
#[allow(dead_code)]
pub(crate) mod sse;
