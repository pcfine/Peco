//! 带有自动重连和指数退避的 SSE 事件源。
//!
//! 本模块提供 [`StreamingEventSource`]，它是一个状态机，将 `reqwest` HTTP 响应
//! 包装为 SSE 事件流，并在传输错误时进行透明重试。
//! 该实现是一个通用的 SSE 事件源简化移植，
//! 适配为使用 `reqwest::Client` 而非泛型的 `HttpClient` trait。
//!
//! ## 架构
//!
//! 六状态机，转移如下（`重试 →` 表示由 [`RetryPolicy`] 决定：允许则 `WaitingToRetry`，
//! 否则产出最后一次错误并进入 `Closed`）：
//!
//! ```text
//! Connecting          收到响应              → ValidatingResponse
//!                     连接失败              → 重试 → / 放弃（产出 ProviderError）
//! Reconnecting        收到响应              → ValidatingResponse
//!                     连接失败              → 重试 → / 放弃（产出 ProviderError）
//! ValidatingResponse  200 + event-stream    → Open（产出 SseEvent::Open）
//!                     非 200 / 内容类型不符  → 瞬时类重试 → / 永久类放弃（产出 ProviderError）
//! Open                收到 message          → Open（产出 SseEvent::Message）
//!                     解析 / UTF-8 错误      → Open（跳过该事件）
//!                     传输错误              → 放弃（产出 ProviderError，由 looper 整轮重发）
//!                     流正常结束            → Closed
//! WaitingToRetry      延迟结束              → Reconnecting
//! Closed              —                     → 终止，不再产出任何事件
//! ```
//!
//! 重试预算（`max_retries`）在连接失败与校验失败之间**共享**：计数经
//! `Reconnecting → ValidatingResponse` 的 `prior_retry` 穿透，不因进入校验而重置。
//!
//! 非 200 响应按 [`ProviderError::classify`] 分类处置：瞬时类（429/5xx）走退避重试，
//! 永久类（401/400/额度…）立即产出 [`ProviderError`]。已建立流的传输中断不透明重连
//! （LLM SSE 不支持 Last-Event-Id 恢复，重连 = 全新生成），直接上抛给 looper。

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use eventsource_stream::{EventStreamError, Eventsource};
use futures::{Stream, StreamExt};
use http::{StatusCode, header};
use pin_project_lite::pin_project;

use crate::ProviderError;

// ============================================================================
// 重试策略
// ============================================================================

/// 决定 SSE 连接失败后是否重试以及等待多长时间。
pub trait RetryPolicy: Send {
    /// 当传输错误发生时调用。
    ///
    /// 首次重试时 `last_retry` 为 `None`（全新连接或从已打开的流中断开），
    /// 后续重试时为 `Some((重试次数, 上次延迟))`。
    ///
    /// 返回 `Some(delay)` 表示在 `delay` 之后安排重试，返回 `None`
    /// 表示放弃并关闭事件源。
    fn retry(
        &self,
        error: &ProviderError,
        last_retry: Option<(usize, Duration)>,
    ) -> Option<Duration>;

    /// 当服务器发送 SSE `retry:` 字段时调用，
    /// 允许策略调整其重连时间。
    fn set_reconnection_time(&mut self, duration: Duration);
}

/// 指数退避重试策略。
///
/// 默认配置（通过 [`DEFAULT_RETRY`]）：
/// - 起始延迟：300 毫秒
/// - 退避因子：每次 2×
/// - 上限：5 秒
/// - 最多 5 次
///
/// 所有内置策略都做**分类门控**：`error.is_transient()` 为 false
/// （401/400/额度/过滤等永久类）时直接返回 `None`，不浪费重试窗口。
pub struct ExponentialBackoff {
    pub start: Duration,
    pub factor: f64,
    pub max_duration: Option<Duration>,
    pub max_retries: Option<usize>,
}

impl ExponentialBackoff {
    pub const fn new(
        start: Duration,
        factor: f64,
        max_duration: Option<Duration>,
        max_retries: Option<usize>,
    ) -> Self {
        Self {
            start,
            factor,
            max_duration,
            max_retries,
        }
    }
}

impl Default for ExponentialBackoff {
    fn default() -> Self {
        DEFAULT_RETRY
    }
}

impl RetryPolicy for ExponentialBackoff {
    fn retry(
        &self,
        error: &ProviderError,
        last_retry: Option<(usize, Duration)>,
    ) -> Option<Duration> {
        // 分类门控：永久类（401/400/额度/过滤…）立即放弃，只有瞬时类走退避。
        if !error.is_transient() {
            return None;
        }
        let (_retry_num, delay) = match last_retry {
            Some((num, last_delay)) => {
                // 检查最大重试次数
                if let Some(max) = self.max_retries
                    && num >= max
                {
                    return None;
                }
                let next = Duration::from_secs_f64(last_delay.as_secs_f64() * self.factor);
                let capped = if let Some(max) = self.max_duration {
                    next.min(max)
                } else {
                    next
                };
                (num + 1, capped)
            }
            None => {
                if let Some(max) = self.max_retries
                    && max == 0
                {
                    return None;
                }
                (1, self.start)
            }
        };
        Some(delay)
    }

    fn set_reconnection_time(&mut self, duration: Duration) {
        self.start = duration;
        if let Some(ref mut max) = self.max_duration
            && *max < duration
        {
            *max = duration;
        }
    }
}

/// 始终等待相同时长的重试策略。
pub struct Constant {
    pub delay: Duration,
    pub max_retries: Option<usize>,
}

impl RetryPolicy for Constant {
    fn retry(
        &self,
        error: &ProviderError,
        last_retry: Option<(usize, Duration)>,
    ) -> Option<Duration> {
        // 分类门控与 ExponentialBackoff 同语义。
        if !error.is_transient() {
            return None;
        }
        let retry_num = last_retry.map(|(n, _)| n).unwrap_or(0);
        if let Some(max) = self.max_retries
            && retry_num >= max
        {
            return None;
        }
        Some(self.delay)
    }

    fn set_reconnection_time(&mut self, duration: Duration) {
        self.delay = duration;
    }
}

/// 永不重试的重试策略。
pub struct Never;

impl RetryPolicy for Never {
    fn retry(
        &self,
        _error: &ProviderError,
        _last_retry: Option<(usize, Duration)>,
    ) -> Option<Duration> {
        None
    }

    fn set_reconnection_time(&mut self, _duration: Duration) {}
}

/// 默认重试策略：指数退避，起始 300 毫秒，每次加倍，上限 5 秒，**最多 5 次**。
///
/// 有限次数是刻意的：短暂网络抖动在传输层自愈（约 9 秒窗口），持续故障必须
/// 上抛给 looper —— 那里有带回退的整轮重发与类型化失败报错。无限重试会把
/// 错误永远挡在传输层，上层永远见不到它。
pub const DEFAULT_RETRY: ExponentialBackoff = ExponentialBackoff::new(
    Duration::from_millis(300),
    2.0,
    Some(Duration::from_secs(5)),
    Some(5),
);

// ============================================================================
// SSE 事件
// ============================================================================

/// [`StreamingEventSource`] 产生的事件。
#[derive(Debug, Clone)]
pub enum SseEvent {
    /// 当连接（或重连）成功建立时发出。
    Open,
    /// 服务器发送的包含数据的事件消息。
    Message(eventsource_stream::Event),
}

// ============================================================================
// 内部类型
// ============================================================================

/// 解析为 reqwest Response 的装箱 future。
type ResponseFuture =
    Pin<Box<dyn Future<Output = Result<reqwest::Response, reqwest::Error>> + Send>>;

/// 校验 SSE 响应（含读取错误体）的装箱 future。
type CheckResponseFuture =
    Pin<Box<dyn Future<Output = Result<reqwest::Response, ProviderError>> + Send>>;

/// 解析后的 SSE 事件的装箱流。
type SseByteStream = Pin<
    Box<
        dyn Stream<Item = Result<eventsource_stream::Event, EventStreamError<std::io::Error>>>
            + Send,
    >,
>;

/// 从响应创建字节流并将其包装在 SSE 解析器中。
fn into_event_stream(response: reqwest::Response) -> SseByteStream {
    let byte_stream = response
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    Box::pin(byte_stream.eventsource())
}

/// 验证响应是否为正确的 SSE 连接。
async fn check_sse_response(
    response: reqwest::Response,
    allow_missing_content_type: bool,
) -> Result<reqwest::Response, ProviderError> {
    let status = response.status();
    if status != StatusCode::OK {
        // 读取错误响应体（通常为 JSON 错误详情），截断以避免异常大的响应体。
        let body = read_error_body(response).await;
        return Err(ProviderError::Api {
            status: status.as_u16(),
            body,
        });
    }

    // 验证 text/event-stream 内容类型
    let content_type_valid = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|ct| ct.to_str().ok())
        .and_then(|s| s.parse::<mime_guess::mime::Mime>().ok())
        .map(|m| {
            m.type_() == mime_guess::mime::TEXT && m.subtype() == mime_guess::mime::EVENT_STREAM
        })
        .unwrap_or(false);

    if content_type_valid || allow_missing_content_type {
        Ok(response)
    } else {
        Err(ProviderError::Response(
            "SSE 的内容类型无效：期望 text/event-stream".into(),
        ))
    }
}

/// 读取非 200 响应的错误体，最多读入 `MAX_BODY_BYTES` 字节以便日志诊断。
async fn read_error_body(response: reqwest::Response) -> String {
    const MAX_BODY_BYTES: usize = 8192;

    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    let mut truncated = false;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(e) => return format!("<failed to read error body: {e}>"),
        };
        // 上限已读满却还收到后续块 —— 此时才真的有内容被丢弃。恰好读满上限的响应体
        // 会在这之前自然结束循环，不会被误标为截断。
        if bytes.len() >= MAX_BODY_BYTES {
            truncated = true;
            break;
        }
        let remaining = MAX_BODY_BYTES - bytes.len();
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }

    let mut text = String::from_utf8_lossy(&bytes).to_string();
    if text.len() > MAX_BODY_BYTES {
        // `from_utf8_lossy` 可能把末尾不完整的多字节序列替换为 U+FFFD，使字节数略增；
        // 截断前退回到最近的字符边界，避免在字符中间 panic。
        text.truncate(text.floor_char_boundary(MAX_BODY_BYTES));
    }
    if truncated {
        text.push_str("…(truncated)");
    }
    text
}

// ============================================================================
// 状态机
// ============================================================================

pin_project! {
    #[project = SseStateProjection]
    enum SseState {
        /// 初始连接尝试正在进行中。
        Connecting {
            #[pin]
            response_future: ResponseFuture,
        },
        /// 连接已建立，正在消费 SSE 事件。
        Open {
            #[pin]
            event_stream: SseByteStream,
        },
        /// 等待重试延迟结束。
        WaitingToRetry {
            #[pin]
            retry_delay: futures_timer::Delay,
            current_retry: (usize, Duration),
        },
        /// 校验已收到的响应（含读取非 200 错误体）后进入 Open 或失败。
        ///
        /// `prior_retry` 是进入校验前已发生的重试（`Reconnecting` 携带；`Connecting`
        /// 首连为 `None`）。校验失败与连接失败共用同一份重试预算 —— 丢掉它，
        /// `max_retries` 在校验路径上永不生效，持续 429/503 会无限重发。
        ValidatingResponse {
            #[pin]
            check_future: CheckResponseFuture,
            // 见变体文档：校验失败的重试在此基础上累积，不可重置。
            prior_retry: Option<(usize, Duration)>,
        },
        /// 在传输错误后重新连接。
        Reconnecting {
            #[pin]
            response_future: ResponseFuture,
            last_retry: (usize, Duration),
        },
        /// 终止状态：不再产生任何事件。
        Closed,
    }
}

// ============================================================================
// StreamingEventSource
// ============================================================================

pin_project! {
    /// 一个 SSE 事件源，在传输错误时自动重连并使用指数退避策略。
    ///
    /// 包装 `reqwest::Client` 和请求组件，通过六状态机驱动连接生命周期
    /// （`Connecting` / `ValidatingResponse` / `Open` / `WaitingToRetry` /
    /// `Reconnecting` / `Closed`，见模块级文档的状态图）。
    /// 重试行为由 [`RetryPolicy`] 泛型参数控制（默认为 [`ExponentialBackoff`]）。
    ///
    /// URL、请求体和认证头分别存储，以便每次重试时可以从头构建请求。
    ///
    /// ## 示例
    ///
    /// ```ignore
    /// use crate::streaming::sse::{StreamingEventSource, SseEvent};
    ///
    /// let mut source = StreamingEventSource::new(
    ///     client,
    ///     "https://api.example.com/stream".into(),
    ///     body_bytes,
    ///     "Bearer sk-...".into(),
    /// );
    /// while let Some(Ok(event)) = source.next().await {
    ///     match event {
    ///         SseEvent::Open => println!("已连接！"),
    ///         SseEvent::Message(msg) => println!("data: {}", msg.data),
    ///     }
    /// }
    /// ```
    #[project = StreamingEventSourceProjection]
    pub struct StreamingEventSource<R = ExponentialBackoff> {
        client: reqwest::Client,
        url: String,
        body: Vec<u8>,
        auth_header: String,
        retry_policy: R,
        last_event_id: Option<String>,
        allow_missing_content_type: bool,
        // 被跳过的 SSE 解析 / UTF-8 错误计数。这些错误逐条可恢复（跳过该事件继续读流），
        // 但逐条打日志会在畸形流上刷屏，因此只累加，在流终止（EOF / 重试放弃）时一次性汇报。
        // 非零即说明上游发过畸形帧。
        skipped_parse_errors: u64,
        #[pin]
        state: SseState,
    }
}

impl StreamingEventSource {
    /// 使用默认重试策略（[`ExponentialBackoff`]）创建新的事件源。
    ///
    /// 向 `url` 发送 POST 请求，请求体为 `body`，
    /// `auth_header` 作为 `Authorization` 头部的值。
    pub fn new(client: reqwest::Client, url: String, body: Vec<u8>, auth_header: String) -> Self {
        Self::with_retry_policy(client, url, body, auth_header, DEFAULT_RETRY)
    }
}

impl<R: RetryPolicy> StreamingEventSource<R> {
    /// 使用自定义 [`RetryPolicy`] 创建新的事件源。
    pub fn with_retry_policy(
        client: reqwest::Client,
        url: String,
        body: Vec<u8>,
        auth_header: String,
        retry_policy: R,
    ) -> Self {
        let response_future = create_response_future(&client, &url, &body, &auth_header, None);

        Self {
            client,
            url,
            body,
            auth_header,
            retry_policy,
            last_event_id: None,
            allow_missing_content_type: false,
            skipped_parse_errors: 0,
            state: SseState::Connecting { response_future },
        }
    }

    /// 跳过 HTTP 响应的 `text/event-stream` 内容类型检查。
    ///
    /// 当连接到没有设置正确 Content-Type 头的端点时使用。
    pub fn allow_missing_content_type(mut self) -> Self {
        self.allow_missing_content_type = true;
        self
    }

    /// 立即关闭事件源。
    ///
    /// 下一次 poll 将返回 `None`。
    pub fn close(&mut self) {
        self.state = SseState::Closed;
    }

    /// 返回最后收到的事件 ID（如果有）。
    ///
    /// 用于重连时的 `Last-Event-Id` 头部。
    pub fn last_event_id(&self) -> Option<&str> {
        self.last_event_id.as_deref()
    }
}

/// 从存储的请求组件构建 response future。
///
/// 从零构建 `reqwest::Request`，并可为重连尝试
/// 选择性设置 `Last-Event-Id` 头部。
fn create_response_future(
    client: &reqwest::Client,
    url: &str,
    body: &[u8],
    auth_header: &str,
    last_event_id: Option<&str>,
) -> ResponseFuture {
    let mut request_builder = client
        .post(url)
        .header("Authorization", auth_header)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .body(body.to_vec());

    // 为重连设置 Last-Event-Id
    if let Some(id) = last_event_id
        && let Ok(val) = http::HeaderValue::from_str(id)
    {
        request_builder =
            request_builder.header(http::HeaderName::from_static("last-event-id"), val);
    }

    let request = match request_builder.build() {
        Ok(req) => req,
        Err(e) => return Box::pin(async move { Err(e) }),
    };

    Box::pin(client.execute(request))
}

impl<R: RetryPolicy> Stream for StreamingEventSource<R> {
    type Item = Result<SseEvent, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            match this.state.as_mut().project() {
                // ---- 连接中 ----
                SseStateProjection::Connecting { response_future } => {
                    match response_future.poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Ok(response)) => {
                            let check_future =
                                check_sse_response(response, *this.allow_missing_content_type);
                            this.state.set(SseState::ValidatingResponse {
                                check_future: Box::pin(check_future),
                                prior_retry: None,
                            });
                            continue;
                        }
                        Poll::Ready(Err(err)) => {
                            let provider_err = ProviderError::from(err);
                            match this.retry_policy.retry(&provider_err, None) {
                                Some(delay) => {
                                    tracing::warn!(
                                        target: "model_provider::streaming",
                                        url = %this.url,
                                        error = %provider_err,
                                        attempt = 1,
                                        delay_ms = delay.as_millis() as u64,
                                        "SSE connect failed; scheduling retry"
                                    );
                                    let retry_delay = futures_timer::Delay::new(delay);
                                    this.state.set(SseState::WaitingToRetry {
                                        retry_delay,
                                        current_retry: (1, delay),
                                    });
                                }
                                None => {
                                    // 放弃必须带最后一次错误离开：静默 Closed → EOF 会被
                                    // pipeline 当成正常结束，产出 Finish{Stop} —— 一个
                                    // 「成功的空回复」。上抛后 pipeline 走 terminated_with_error，
                                    // looper 才能分类、退避重发。
                                    tracing::warn!(
                                        target: "model_provider::streaming",
                                        url = %this.url,
                                        error = %provider_err,
                                        "SSE connect failed; retry policy gave up; surfacing error"
                                    );
                                    this.state.set(SseState::Closed);
                                    return Poll::Ready(Some(Err(provider_err)));
                                }
                            }
                        }
                    }
                }

                // ---- 重连中 ----
                SseStateProjection::Reconnecting {
                    response_future,
                    last_retry,
                } => {
                    let last_retry = *last_retry;
                    match response_future.poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Ok(response)) => {
                            let check_future =
                                check_sse_response(response, *this.allow_missing_content_type);
                            this.state.set(SseState::ValidatingResponse {
                                check_future: Box::pin(check_future),
                                // 带上重试计数：校验失败与连接失败共用同一份预算，
                                // 在这里丢弃会让 max_retries 永不生效（见状态定义注释）。
                                prior_retry: Some(last_retry),
                            });
                            continue;
                        }
                        Poll::Ready(Err(err)) => {
                            let provider_err = ProviderError::from(err);
                            match this.retry_policy.retry(&provider_err, Some(last_retry)) {
                                Some(delay) => {
                                    tracing::warn!(
                                        target: "model_provider::streaming",
                                        url = %this.url,
                                        error = %provider_err,
                                        attempt = last_retry.0 + 1,
                                        delay_ms = delay.as_millis() as u64,
                                        "SSE reconnect failed; retrying"
                                    );
                                    let retry_delay = futures_timer::Delay::new(delay);
                                    this.state.set(SseState::WaitingToRetry {
                                        retry_delay,
                                        current_retry: (last_retry.0 + 1, delay),
                                    });
                                }
                                None => {
                                    // 同 Connecting：放弃必须上抛最后一次错误，不能静默 EOF。
                                    tracing::warn!(
                                        target: "model_provider::streaming",
                                        url = %this.url,
                                        error = %provider_err,
                                        attempts = last_retry.0,
                                        "SSE reconnect failed; retry policy gave up; surfacing error"
                                    );
                                    this.state.set(SseState::Closed);
                                    return Poll::Ready(Some(Err(provider_err)));
                                }
                            }
                        }
                    }
                }

                // ---- 校验响应（含读取非 200 错误体）----
                SseStateProjection::ValidatingResponse {
                    check_future,
                    prior_retry,
                } => {
                    // 进入校验前已发生的重试次数/延迟（首连为 None）。校验失败的
                    // 重试必须在此基础上累积，否则每次失败都重置为「第 1 次」，
                    // max_retries 永不生效，持续瞬时错误无限循环。
                    let prior_retry = *prior_retry;
                    match check_future.poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Ok(response)) => {
                            let event_stream = into_event_stream(response);
                            this.state.set(SseState::Open { event_stream });
                            return Poll::Ready(Some(Ok(SseEvent::Open)));
                        }
                        Poll::Ready(Err(err)) => {
                            // 非 200 与 content-type 不符都走这里。由策略按错误分类决定：
                            // 瞬时类（429/5xx）退避重试，永久类（401/400/额度…）立即失败。
                            let kind = err.classify().kind;
                            match this.retry_policy.retry(&err, prior_retry) {
                                Some(delay) => {
                                    let attempt = prior_retry.map_or(1, |(n, _)| n + 1);
                                    tracing::warn!(
                                        target: "model_provider::streaming",
                                        url = %this.url,
                                        error = %err,
                                        kind = kind.as_str(),
                                        attempt,
                                        delay_ms = delay.as_millis() as u64,
                                        "SSE response validation failed with transient error; scheduling retry"
                                    );
                                    let retry_delay = futures_timer::Delay::new(delay);
                                    this.state.set(SseState::WaitingToRetry {
                                        retry_delay,
                                        current_retry: (attempt, delay),
                                    });
                                    continue;
                                }
                                None => {
                                    // 永久类分类即放弃，瞬时类则为预算耗尽 —— 两者都
                                    // 上抛，让 looper 分类报错/整轮重发。
                                    tracing::warn!(
                                        target: "model_provider::streaming",
                                        url = %this.url,
                                        error = %err,
                                        kind = kind.as_str(),
                                        prior_attempts = prior_retry.map(|(n, _)| n).unwrap_or(0),
                                        "SSE response validation failed; retry policy gave up; surfacing error"
                                    );
                                    this.state.set(SseState::Closed);
                                    return Poll::Ready(Some(Err(err)));
                                }
                            }
                        }
                    }
                }

                // ---- 已打开（消费 SSE 事件）----
                SseStateProjection::Open { mut event_stream } => {
                    match event_stream.as_mut().poll_next(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Some(Ok(event))) => {
                            // 跟踪最后的事件 ID 以便重连
                            if !event.id.is_empty() {
                                *this.last_event_id = Some(event.id.clone());
                            }
                            // 应用服务器发送的重试提示
                            if let Some(retry_dur) = event.retry {
                                this.retry_policy.set_reconnection_time(retry_dur);
                            }
                            return Poll::Ready(Some(Ok(SseEvent::Message(event))));
                        }
                        Poll::Ready(Some(Err(EventStreamError::Transport(err)))) => {
                            // established-stream 中断**不透明重连**：LLM provider 的
                            // SSE 不支持 Last-Event-Id 恢复（无 id: 字段，重连 = 重新
                            // POST = 全新生成 → 增量重复拼接）。中断上抛给 looper，
                            // 由它回退 staging 后整轮重发 —— 那才是去重安全的重试点。
                            let provider_err = ProviderError::Stream(err.to_string());
                            tracing::warn!(
                                target: "model_provider::streaming",
                                url = %this.url,
                                error = %provider_err,
                                skipped_parse_errors = *this.skipped_parse_errors,
                                "established SSE stream interrupted; surfacing to caller"
                            );
                            this.state.set(SseState::Closed);
                            return Poll::Ready(Some(Err(provider_err)));
                        }
                        // 解析器和 UTF-8 错误是可恢复的 — 跳过并继续处理流。
                        Poll::Ready(Some(Err(
                            EventStreamError::Parser(_) | EventStreamError::Utf8(_),
                        ))) => {
                            // 逐条打会在畸形流上刷屏，只累加，流终止时统一汇报。
                            *this.skipped_parse_errors += 1;
                            continue;
                        }
                        Poll::Ready(None) => {
                            // 流正常结束。此处不再逐流打「正常结束」日志 —— 上层终止摘要已覆盖。
                            if *this.skipped_parse_errors > 0 {
                                tracing::warn!(
                                    target: "model_provider::streaming",
                                    url = %this.url,
                                    skipped_parse_errors = *this.skipped_parse_errors,
                                    "SSE stream ended after skipping malformed events"
                                );
                            }
                            this.state.set(SseState::Closed);
                            return Poll::Ready(None);
                        }
                    }
                }

                // ---- 等待重试 ----
                SseStateProjection::WaitingToRetry {
                    retry_delay,
                    current_retry,
                } => {
                    let current_retry = *current_retry;
                    match retry_delay.poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(()) => {
                            tracing::debug!(
                                target: "model_provider::streaming",
                                url = %this.url,
                                attempt = current_retry.0,
                                last_event_id = this.last_event_id.as_deref().unwrap_or("-"),
                                "starting SSE reconnect"
                            );
                            let response_future = create_response_future(
                                this.client,
                                this.url,
                                this.body,
                                this.auth_header,
                                this.last_event_id.as_deref(),
                            );
                            this.state.set(SseState::Reconnecting {
                                response_future,
                                last_retry: current_retry,
                            });
                            // 循环回去以 poll 新的 response future
                            continue;
                        }
                    }
                }

                // ---- 已关闭（终止状态）----
                SseStateProjection::Closed => {
                    return Poll::Ready(None);
                }
            }
        }
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exponential_backoff_first_retry() {
        let policy = DEFAULT_RETRY;
        let err = ProviderError::Stream("test".into());
        let delay = policy.retry(&err, None);
        assert!(delay.is_some());
        assert_eq!(delay.unwrap(), Duration::from_millis(300));
    }

    #[test]
    fn test_exponential_backoff_sequence() {
        let policy = DEFAULT_RETRY;
        let err = ProviderError::Stream("test".into());

        let d1 = policy.retry(&err, None).unwrap();
        assert_eq!(d1, Duration::from_millis(300));

        let d2 = policy.retry(&err, Some((1, d1))).unwrap();
        assert_eq!(d2, Duration::from_millis(600));

        let d3 = policy.retry(&err, Some((2, d2))).unwrap();
        assert_eq!(d3, Duration::from_millis(1200));
    }

    #[test]
    fn test_exponential_backoff_cap() {
        let policy = DEFAULT_RETRY;
        let err = ProviderError::Stream("test".into());

        // 第 4 次重试（在 max_retries=5 窗口内）：延迟已到 5 秒上限
        let delay = policy
            .retry(&err, Some((4, Duration::from_secs(5))))
            .unwrap();
        assert_eq!(delay, Duration::from_secs(5));
    }

    #[test]
    fn test_never_policy() {
        let policy = Never;
        let err = ProviderError::Stream("test".into());
        assert!(policy.retry(&err, None).is_none());
        assert!(
            policy
                .retry(&err, Some((1, Duration::from_millis(100))))
                .is_none()
        );
    }

    #[test]
    fn test_constant_policy() {
        let policy = Constant {
            delay: Duration::from_secs(2),
            max_retries: Some(3),
        };
        let err = ProviderError::Stream("test".into());
        // 第一次重试（None = 之前没有重试过）
        assert_eq!(policy.retry(&err, None).unwrap(), Duration::from_secs(2));
        // 第二次重试
        assert_eq!(
            policy
                .retry(&err, Some((1, Duration::from_secs(2))))
                .unwrap(),
            Duration::from_secs(2)
        );
        // 第三次重试
        assert_eq!(
            policy
                .retry(&err, Some((2, Duration::from_secs(2))))
                .unwrap(),
            Duration::from_secs(2)
        );
        // 第四次重试应该被拒绝（max_retries=3 表示总共允许 3 次）
        assert!(
            policy
                .retry(&err, Some((3, Duration::from_secs(2))))
                .is_none()
        );
    }

    #[test]
    fn test_permanent_error_not_retried() {
        // 分类门控：401/400 等永久类即刻放弃，不进退避窗口。
        let policy = DEFAULT_RETRY;
        let auth = ProviderError::Api {
            status: 401,
            body: "invalid api key".into(),
        };
        assert!(policy.retry(&auth, None).is_none());

        let bad_request = ProviderError::Api {
            status: 400,
            body: "bad request".into(),
        };
        assert!(policy.retry(&bad_request, None).is_none());
    }

    #[test]
    fn test_transient_error_retried() {
        let policy = DEFAULT_RETRY;
        let rate = ProviderError::Api {
            status: 429,
            body: "rate limited".into(),
        };
        assert_eq!(policy.retry(&rate, None), Some(Duration::from_millis(300)));

        let server = ProviderError::Api {
            status: 503,
            body: "unavailable".into(),
        };
        assert!(policy.retry(&server, None).is_some());
    }

    #[test]
    fn test_default_retry_exhausts_after_five() {
        let policy = DEFAULT_RETRY;
        let err = ProviderError::Stream("connection reset".into());
        // 前 5 次放行，第 6 次（last_retry.0 == 5）拒绝
        let mut last: Option<(usize, Duration)> = None;
        for i in 1..=5 {
            let delay = policy
                .retry(&err, last)
                .unwrap_or_else(|| panic!("attempt {i} should be allowed"));
            last = Some((i, delay));
        }
        assert!(policy.retry(&err, last).is_none(), "5 次后必须耗尽");
    }

    #[test]
    fn test_set_reconnection_time_updates_max() {
        let mut policy = ExponentialBackoff::new(
            Duration::from_millis(100),
            2.0,
            Some(Duration::from_secs(3)),
            None,
        );
        // 服务器要求 10 秒重连时间
        policy.set_reconnection_time(Duration::from_secs(10));
        // max_duration 应至少提高到 10 秒
        assert!(policy.max_duration.unwrap() >= Duration::from_secs(10));
    }

    /// 小预算快退避策略：10ms 起步、最多 2 次重试，让耗尽路径在毫秒级完成。
    fn fast_policy() -> ExponentialBackoff {
        ExponentialBackoff::new(
            Duration::from_millis(10),
            2.0,
            Some(Duration::from_millis(50)),
            Some(2),
        )
    }

    /// 回归：校验失败（429）的重试计数必须经 `Reconnecting → ValidatingResponse`
    /// 穿透。修复前每轮都以 `retry(&err, None)` 重新计数，`max_retries` 永不
    /// 生效，持续 429 以固定间隔无限重发。
    #[tokio::test]
    async fn test_validation_retry_exhausts_and_surfaces_error() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        let hits = Arc::new(AtomicUsize::new(0));
        let hits_srv = hits.clone();

        // 永远回 429 的极简 HTTP 服务。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                hits_srv.fetch_add(1, Ordering::SeqCst);
                let body = r#"{"error":{"type":"rate_limit","message":"rate limited"}}"#;
                let resp = format!(
                    "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                use tokio::io::AsyncWriteExt;
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let mut source = StreamingEventSource::with_retry_policy(
            reqwest::Client::new(),
            format!("http://{addr}/v1/responses"),
            br#"{"model":"test"}"#.to_vec(),
            "Bearer test".into(),
            fast_policy(),
        );

        // 修复前此调用永不返回（每 10ms 重新校验一次）——超时即为回归。
        let first = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("validation retry never exhausted: infinite loop")
            .expect("retry exhaustion must not end as clean EOF");
        match first {
            Err(ProviderError::Api { status: 429, .. }) => {}
            other => panic!("expected surfaced 429, got {other:?}"),
        }

        // 初始连接 + 2 次重试 = 3 次命中；计数未被重置（重置则远超 3 次）。
        assert_eq!(hits.load(Ordering::SeqCst), 3);

        server.abort();
    }

    /// 回归：连接重试耗尽必须上抛最后一次错误。修复前给弃只置 `Closed`，
    /// 流以干净 EOF 结束，pipeline 产出 `Finish{Stop}` —— 空回复被判成功。
    #[tokio::test]
    async fn test_connect_retry_exhaustion_surfaces_error_not_eof() {
        // 先占一个端口再释放：随后的连接一律 connection refused。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let mut source = StreamingEventSource::with_retry_policy(
            reqwest::Client::new(),
            format!("http://{addr}/v1/responses"),
            br#"{"model":"test"}"#.to_vec(),
            "Bearer test".into(),
            fast_policy(),
        );

        let first = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("connect retry never exhausted: infinite loop")
            .expect(
                "retry exhaustion must not look like a clean EOF (would finish as empty success)",
            );
        assert!(
            first.is_err(),
            "expected surfaced transport error, got {first:?}"
        );
    }
}
