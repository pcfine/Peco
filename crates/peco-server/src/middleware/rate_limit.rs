// ============================================================================
// API 限流中间件 — 基于 JWT user_id 的 per-user GCRA 速率限制
// ============================================================================
//
// 使用 `governor` crate (GCRA 算法) + `DashMap` 实现 per-key 限流。
// 包装为 Tower Layer，与 Axum 中间件栈无缝集成。
//
// 两层策略：
// - `rate_limit_layer()`: 普通 API，每秒 20 次，突发 100 次
// - `sse_rate_limit_layer()`: SSE 端点，每秒 1 次，突发 3 次

use std::num::NonZeroU32;
use std::sync::Arc;

use axum::http::{header::AUTHORIZATION, Request, StatusCode};
use axum::response::Response;
use governor::{DefaultKeyedRateLimiter, Quota};
use tower::{Layer, Service};

// ── KeyedRateLimitLayer ────────────────────────────────────────────────────────

/// GCRA per-key 限流 Tower Layer。
///
/// 每个 key（JWT user_id 或 "anonymous"）独立限流。
#[derive(Clone)]
pub struct KeyedRateLimitLayer {
    quota: Quota,
    jwt_secret: Arc<String>,
}

impl KeyedRateLimitLayer {
    pub fn per_second(rate: u32, burst: u32, jwt_secret: impl Into<String>) -> Self {
        let quota = Quota::per_second(NonZeroU32::new(rate).unwrap_or(NonZeroU32::MIN))
            .allow_burst(NonZeroU32::new(burst).unwrap_or(NonZeroU32::MIN));
        Self {
            quota,
            jwt_secret: Arc::new(jwt_secret.into()),
        }
    }
}

impl<S> Layer<S> for KeyedRateLimitLayer {
    type Service = KeyedRateLimit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        KeyedRateLimit {
            inner,
            limiter: Arc::new(DefaultKeyedRateLimiter::keyed(self.quota)),
            jwt_secret: self.jwt_secret.clone(),
        }
    }
}

// ── KeyedRateLimit Service ─────────────────────────────────────────────────────

/// Per-key GCRA 限流 Service。
///
/// 从请求的 Authorization header 提取 JWT user_id 作为限流 key，
/// 未认证请求使用 "anonymous" 作为 key。
#[derive(Clone)]
pub struct KeyedRateLimit<S> {
    inner: S,
    limiter: Arc<DefaultKeyedRateLimiter<String>>,
    jwt_secret: Arc<String>,
}

impl<S, B> Service<Request<B>> for KeyedRateLimit<S>
where
    S: Service<Request<B>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        // 提取限流 key
        let key = extract_rate_limit_key(&req, &self.jwt_secret);

        // 检查 GCRA 配额
        if self.limiter.check_key(&key).is_err() {
            let resp = Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .header("Retry-After", "1")
                .body(axum::body::Body::from(
                    r#"{"error":"Too many requests. Please slow down."}"#,
                ))
                .unwrap();
            return Box::pin(std::future::ready(Ok(resp)));
        }

        // 放行
        let mut inner = self.inner.clone();
        Box::pin(async move { inner.call(req).await })
    }
}

/// 从请求中提取限流 key：JWT user_id 或 "anonymous"。
fn extract_rate_limit_key<B>(req: &Request<B>, jwt_secret: &str) -> String {
    let auth_header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(header) => {
            let token = match header.strip_prefix("Bearer ") {
                Some(t) => t,
                None => return "anonymous".to_string(),
            };
            match crate::auth::jwt::verify_token(token, jwt_secret) {
                Ok(claims) => claims.sub,
                Err(_) => "anonymous".to_string(),
            }
        }
        None => "anonymous".to_string(),
    }
}

// ── 便捷构造器 ─────────────────────────────────────────────────────────────────

/// 普通 API 限流：每秒 20 次，突发 100 次。
pub fn rate_limit_layer(secret: &str) -> KeyedRateLimitLayer {
    KeyedRateLimitLayer::per_second(20, 100, secret.to_string())
}

/// SSE 端点限流：每秒 1 次，突发 3 次。
pub fn sse_rate_limit_layer(secret: &str) -> KeyedRateLimitLayer {
    KeyedRateLimitLayer::per_second(1, 3, secret.to_string())
}
