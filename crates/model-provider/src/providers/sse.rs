//! 带有自动重连和指数退避的 SSE 事件源。
//!
//! 本模块提供 [`StreamingEventSource`]，它是一个状态机，将 `reqwest` HTTP 响应
//! 包装为 SSE 事件流，并在传输错误时进行透明重试。
//! 该实现是一个通用的 SSE 事件源简化移植，
//! 适配为使用 `reqwest::Client` 而非泛型的 `HttpClient` trait。
//!
//! ## 架构
//!
//! ```text
//! Connecting ──(200 + text/event-stream)──▶ Open ──(message)──▶ Open
//!      │                                      │
//!      └──(可重试错误)──▶ WaitingToRetry ◀──(传输错误)
//!                                          │
//!                                          └──(延迟结束)──▶ Reconnecting
//!                                                                    │
//!                                                    (成功)──────────┘
//!                                                    (重试耗尽)─────▶ Closed
//! ```

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
/// - 无限次重试
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
        _error: &ProviderError,
        last_retry: Option<(usize, Duration)>,
    ) -> Option<Duration> {
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
        _error: &ProviderError,
        last_retry: Option<(usize, Duration)>,
    ) -> Option<Duration> {
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

/// 默认重试策略：指数退避，起始 300 毫秒，每次加倍，
/// 上限 5 秒，无限次重试。
pub const DEFAULT_RETRY: ExponentialBackoff = ExponentialBackoff::new(
    Duration::from_millis(300),
    2.0,
    Some(Duration::from_secs(5)),
    None,
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
fn check_sse_response(
    response: reqwest::Response,
    allow_missing_content_type: bool,
) -> Result<reqwest::Response, ProviderError> {
    let status = response.status();
    if status != StatusCode::OK {
        return Err(ProviderError::Api {
            status: status.as_u16(),
            body: String::new(),
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
    /// 包装 `reqwest::Client` 和请求组件，通过五状态机驱动连接生命周期。
    /// 重试行为由 [`RetryPolicy`] 泛型参数控制（默认为 [`ExponentialBackoff`]）。
    ///
    /// URL、请求体和认证头分别存储，以便每次重试时可以从头构建请求。
    ///
    /// ## 示例
    ///
    /// ```ignore
    /// use model_provider::providers::sse::{StreamingEventSource, SseEvent};
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
                            match check_sse_response(response, *this.allow_missing_content_type) {
                                Ok(response) => {
                                    let event_stream = into_event_stream(response);
                                    this.state.set(SseState::Open { event_stream });
                                    return Poll::Ready(Some(Ok(SseEvent::Open)));
                                }
                                Err(err) => {
                                    this.state.set(SseState::Closed);
                                    return Poll::Ready(Some(Err(err)));
                                }
                            }
                        }
                        Poll::Ready(Err(err)) => {
                            let provider_err = ProviderError::from(err);
                            if let Some(delay) = this.retry_policy.retry(&provider_err, None) {
                                let retry_delay = futures_timer::Delay::new(delay);
                                this.state.set(SseState::WaitingToRetry {
                                    retry_delay,
                                    current_retry: (1, delay),
                                });
                            } else {
                                this.state.set(SseState::Closed);
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
                            match check_sse_response(response, *this.allow_missing_content_type) {
                                Ok(response) => {
                                    let event_stream = into_event_stream(response);
                                    this.state.set(SseState::Open { event_stream });
                                    return Poll::Ready(Some(Ok(SseEvent::Open)));
                                }
                                Err(err) => {
                                    this.state.set(SseState::Closed);
                                    return Poll::Ready(Some(Err(err)));
                                }
                            }
                        }
                        Poll::Ready(Err(err)) => {
                            let provider_err = ProviderError::from(err);
                            if let Some(delay) =
                                this.retry_policy.retry(&provider_err, Some(last_retry))
                            {
                                let retry_delay = futures_timer::Delay::new(delay);
                                this.state.set(SseState::WaitingToRetry {
                                    retry_delay,
                                    current_retry: (last_retry.0 + 1, delay),
                                });
                            } else {
                                this.state.set(SseState::Closed);
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
                            let provider_err = ProviderError::Stream(err.to_string());
                            if let Some(delay) = this.retry_policy.retry(&provider_err, None) {
                                let retry_delay = futures_timer::Delay::new(delay);
                                this.state.set(SseState::WaitingToRetry {
                                    retry_delay,
                                    current_retry: (1, delay),
                                });
                            } else {
                                this.state.set(SseState::Closed);
                            }
                        }
                        // 解析器和 UTF-8 错误是可恢复的 — 跳过并继续处理流。
                        Poll::Ready(Some(Err(
                            EventStreamError::Parser(_) | EventStreamError::Utf8(_),
                        ))) => {
                            continue;
                        }
                        Poll::Ready(None) => {
                            // 流正常结束
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

        // 已达到 5 秒上限
        let delay = policy
            .retry(&err, Some((10, Duration::from_secs(5))))
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
}
