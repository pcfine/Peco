use thiserror::Error;

/// 与模型提供商交互时可能发生的错误。
#[derive(Debug, Error)]
pub enum ProviderError {
    /// HTTP 传输错误。
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON 序列化或反序列化错误。
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// 提供商返回了 API 级别的错误响应。
    #[error("API error ({status}): {body}")]
    Api { status: u16, body: String },

    /// 提供商的响应无法解析或无效。
    #[error("Response error: {0}")]
    Response(String),

    /// 流式传输过程中发生错误。
    #[error("Stream error: {0}")]
    Stream(String),

    /// 配置或请求构建错误。
    #[error("Request error: {0}")]
    Request(String),
}

// ============================================================================
// 错误语义分类
// ============================================================================

/// API 错误的语义分类。决定上层重试/呈现策略。
///
/// 分类由 [`classify_api_error`] 在需要时惰性计算 — 构造 [`ProviderError::Api`]
/// 的 12 处调用点无需感知本枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApiErrorKind {
    /// 429 / body 含 rate limit 语义 — 瞬时，可退避重试
    RateLimited,
    /// 网络传输失败 / 超时 — 瞬时
    Network,
    /// 5xx — 瞬时
    Server,
    /// 401 / 403 鉴权失败 — 永久
    Auth,
    /// 404 endpoint / 模型名不存在 — 永久
    NotFound,
    /// 上下文窗口溢出（400 + context_length 语义）— 需压缩，不可盲目重试
    ContextOverflow,
    /// 额度/余额耗尽（insufficient_quota / balance / 402）— 永久（需充值）
    QuotaExhausted,
    /// 内容过滤 — 永久（重试结果相同）
    ContentFiltered,
    /// 其余 400/422 请求错误 — 永久
    InvalidRequest,
    /// 无法归类
    Unknown,
}

impl ApiErrorKind {
    /// 瞬时类 = 允许有限次退避重发。
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::RateLimited | Self::Network | Self::Server)
    }

    /// 稳定字符串表示，用于日志字段。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::Network => "network",
            Self::Server => "server",
            Self::Auth => "auth",
            Self::NotFound => "not_found",
            Self::ContextOverflow => "context_overflow",
            Self::QuotaExhausted => "quota_exhausted",
            Self::ContentFiltered => "content_filtered",
            Self::InvalidRequest => "invalid_request",
            Self::Unknown => "unknown",
        }
    }
}

/// 分类结果：语义 kind + 原始错误消息。
///
/// **分类不吞原文** — 上层日志、`TurnFailureReason` 文案、SSE error
/// 都要用原始 msg 做诊断。
#[derive(Debug, Clone)]
pub struct ClassifiedError {
    /// 语义分类
    pub kind: ApiErrorKind,
    /// 原始错误 msg：`Api` 取 body（截断到 [`MAX_MESSAGE_LEN`]），
    /// `Http`/`Stream`/`Response`/`Request` 取其 Display 文案。
    pub message: String,
    /// 解析成功时的 provider 结构化 code（`error.code`），无则 `None`。
    pub code: Option<String>,
}

/// `ClassifiedError.message` 的长度上限 — body 可能很大，只保留摘要。
const MAX_MESSAGE_LEN: usize = 500;

impl ProviderError {
    /// 本错误的语义分类（含原始 msg）。
    pub fn classify(&self) -> ClassifiedError {
        match self {
            ProviderError::Http(e) => ClassifiedError {
                kind: ApiErrorKind::Network,
                message: e.to_string(),
                code: None,
            },
            ProviderError::Stream(msg) => ClassifiedError {
                kind: ApiErrorKind::Network,
                message: truncate_message(msg),
                code: None,
            },
            ProviderError::Api { status, body } => classify_api_error(*status, body),
            ProviderError::Json(e) => ClassifiedError {
                kind: ApiErrorKind::Unknown,
                message: e.to_string(),
                code: None,
            },
            ProviderError::Response(msg) => ClassifiedError {
                kind: ApiErrorKind::Unknown,
                message: truncate_message(msg),
                code: None,
            },
            ProviderError::Request(msg) => ClassifiedError {
                kind: ApiErrorKind::InvalidRequest,
                message: truncate_message(msg),
                code: None,
            },
        }
    }

    /// 是否允许瞬时重发（退避后重试）。
    pub fn is_transient(&self) -> bool {
        self.classify().kind.is_transient()
    }
}

/// 按字符边界截断到 [`MAX_MESSAGE_LEN`]（多字节安全）。
fn truncate_message(s: &str) -> String {
    if s.chars().count() <= MAX_MESSAGE_LEN {
        return s.to_string();
    }
    s.chars().take(MAX_MESSAGE_LEN).collect()
}

/// 把 HTTP status + 错误 body 归类为语义 [`ClassifiedError`]。
///
/// 判定顺序**先 body 后 status**：流内错误 payload（`pipeline.rs` 的
/// `provider_error_from_sse_data`）用的是伪造 status 500，真实语义只在
/// body 里 — 先看 status 会把额度耗尽、上下文溢出统统误判成 Server。
///
/// 1. **body JSON 启发式**：解析 OpenAI 兼容错误体
///    `{"error":{"code","type","message"}}`（DeepSeek/Qwen/OpenAI 同构）；
/// 2. **status 兜底**：429/5xx/401/403/404/400/422；
/// 3. **文本兜底**：body 非 JSON 时按关键词扫。
pub fn classify_api_error(status: u16, body: &str) -> ClassifiedError {
    let message = truncate_message(body);

    // ── 1. body JSON 启发式 ──
    if let Some(parsed) = parse_error_payload(body) {
        let kind = classify_from_fields(status, parsed.code.clone(), parsed.type_, parsed.message);
        return ClassifiedError {
            kind,
            message,
            code: parsed.code,
        };
    }

    // ── 3. 文本兜底（body 非 JSON）──
    let kind = classify_from_text(status, body);
    ClassifiedError {
        kind,
        message,
        code: None,
    }
}

/// 解析出的 provider 结构化错误字段。
struct ErrorPayload {
    code: Option<String>,
    type_: Option<String>,
    message: Option<String>,
}

/// 解析 OpenAI 兼容错误体的 `error.{code,type,message}`。
///
/// 兼容两种形态：`{"error": {...}}` 与扁平 `{"code":..., "message":...}`。
/// 解析失败返回 `None`（走文本兜底）。
fn parse_error_payload(body: &str) -> Option<ErrorPayload> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = value.get("error").cloned().unwrap_or(value);
    if !error.is_object() {
        return None;
    }
    let get = |key: &str| {
        error
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let payload = ErrorPayload {
        code: get("code"),
        type_: get("type"),
        message: get("message"),
    };
    // 三个字段全空视为无结构
    if payload.code.is_none() && payload.type_.is_none() && payload.message.is_none() {
        return None;
    }
    Some(payload)
}

/// 按结构化字段（code/type/message）+ status 分类。
fn classify_from_fields(
    status: u16,
    code: Option<String>,
    type_: Option<String>,
    message: Option<String>,
) -> ApiErrorKind {
    // code/type 是最强信号，优先于 status（400 + context_length、
    // 429 + insufficient_quota 等组合都以语义字段为准）。
    let hay_code = join_lower(&[code.as_deref(), type_.as_deref()]);
    let hay_msg = message.as_deref().unwrap_or("").to_lowercase();

    if let Some(kind) = classify_semantics(&hay_code, &hay_msg, status) {
        return kind;
    }
    classify_from_status(status)
}

/// 语义关键词匹配。`hay_code` = code+type 小写拼接，`hay_msg` = message 小写。
fn classify_semantics(hay_code: &str, hay_msg: &str, status: u16) -> Option<ApiErrorKind> {
    // 上下文溢出
    if hay_code.contains("context_length")
        || hay_code.contains("context_window")
        || hay_msg.contains("context_length_exceeded")
        || hay_msg.contains("context window")
        || hay_msg.contains("input length and")
        || hay_msg.contains("maximum context")
        || hay_msg.contains("prompt is too long")
        || hay_msg.contains("too many tokens")
    {
        return Some(ApiErrorKind::ContextOverflow);
    }
    // 额度耗尽
    if status == 402
        || hay_code.contains("insufficient_quota")
        || hay_code.contains("quota")
        || hay_code.contains("billing")
        || hay_code.contains("balance")
        || hay_msg.contains("insufficient balance")
        || hay_msg.contains("insufficient quota")
        || hay_msg.contains("quota")
        || hay_msg.contains("balance")
        || hay_msg.contains("exceeded your current balance")
    {
        return Some(ApiErrorKind::QuotaExhausted);
    }
    // 内容过滤
    if hay_code.contains("content_filter")
        || hay_code.contains("content_policy")
        || hay_code.contains("moderation")
        || hay_msg.contains("content filter")
        || hay_msg.contains("content policy")
        || hay_msg.contains("violated our community guidelines")
    {
        return Some(ApiErrorKind::ContentFiltered);
    }
    // 限流
    if hay_code.contains("rate_limit")
        || hay_code.contains("too_many_requests")
        || hay_msg.contains("rate limit")
        || hay_msg.contains("too many requests")
    {
        return Some(ApiErrorKind::RateLimited);
    }
    None
}

/// status 兜底分类。
fn classify_from_status(status: u16) -> ApiErrorKind {
    match status {
        429 => ApiErrorKind::RateLimited,
        500..=599 => ApiErrorKind::Server,
        401 | 403 => ApiErrorKind::Auth,
        404 => ApiErrorKind::NotFound,
        400 | 422 => ApiErrorKind::InvalidRequest,
        _ => ApiErrorKind::Unknown,
    }
}

/// body 非 JSON 时的文本关键词兜底 + status。
fn classify_from_text(status: u16, body: &str) -> ApiErrorKind {
    let lower = body.to_lowercase();
    // 复用语义匹配（非 JSON body 也能命中 DeepSeek "Insufficient Balance" 等）
    if let Some(kind) = classify_semantics(&lower, &lower, status) {
        return kind;
    }
    classify_from_status(status)
}

/// 拼接多个 Option<&str> 为小写字符串。
fn join_lower(parts: &[Option<&str>]) -> String {
    parts
        .iter()
        .flatten()
        .map(|s| s.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(status: u16, body: &str) -> ApiErrorKind {
        classify_api_error(status, body).kind
    }

    #[test]
    fn classify_context_overflow_from_json_code() {
        let body = r#"{"error":{"message":"This model's maximum context length is 8192 tokens","type":"invalid_request_error","code":"context_length_exceeded"}}"#;
        assert_eq!(kind(400, body), ApiErrorKind::ContextOverflow);
    }

    #[test]
    fn classify_context_overflow_from_message_only() {
        let body = r#"{"error":{"message":"input length and max_tokens exceed context length"}}"#;
        assert_eq!(kind(400, body), ApiErrorKind::ContextOverflow);
    }

    #[test]
    fn classify_quota_from_openai_code() {
        let body = r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_api_quota"}}"#;
        // OpenAI 用 429 报额度耗尽 — 语义字段优先于 status
        assert_eq!(kind(429, body), ApiErrorKind::QuotaExhausted);
    }

    #[test]
    fn classify_quota_from_deepseek_balance_text() {
        // DeepSeek 非 JSON / 简单文本形态
        assert_eq!(
            kind(402, "Insufficient Balance"),
            ApiErrorKind::QuotaExhausted
        );
        assert_eq!(
            kind(
                400,
                r#"{"error":{"message":"You don't have enough balance"}}"#
            ),
            ApiErrorKind::QuotaExhausted
        );
    }

    #[test]
    fn classify_content_filter() {
        let body = r#"{"error":{"message":"The response was filtered","type":"content_filter","code":null}}"#;
        assert_eq!(kind(400, body), ApiErrorKind::ContentFiltered);
    }

    #[test]
    fn classify_rate_limit_from_body_beats_500_status() {
        // 流内错误 payload 伪造 500，语义在 body
        let body = r#"{"error":{"message":"Rate limit reached","code":"rate_limit_exceeded"}}"#;
        assert_eq!(kind(500, body), ApiErrorKind::RateLimited);
    }

    #[test]
    fn classify_status_fallbacks() {
        assert_eq!(kind(429, "slow down"), ApiErrorKind::RateLimited);
        assert_eq!(kind(503, "unavailable"), ApiErrorKind::Server);
        assert_eq!(kind(500, ""), ApiErrorKind::Server);
        assert_eq!(kind(401, "{}"), ApiErrorKind::Auth);
        assert_eq!(kind(403, "forbidden"), ApiErrorKind::Auth);
        assert_eq!(kind(404, "not found"), ApiErrorKind::NotFound);
        assert_eq!(kind(400, "bad request"), ApiErrorKind::InvalidRequest);
        assert_eq!(kind(418, "teapot"), ApiErrorKind::Unknown);
    }

    #[test]
    fn transient_classification() {
        assert!(ApiErrorKind::RateLimited.is_transient());
        assert!(ApiErrorKind::Network.is_transient());
        assert!(ApiErrorKind::Server.is_transient());
        assert!(!ApiErrorKind::Auth.is_transient());
        assert!(!ApiErrorKind::ContextOverflow.is_transient());
        assert!(!ApiErrorKind::QuotaExhausted.is_transient());
        assert!(!ApiErrorKind::ContentFiltered.is_transient());
        assert!(!ApiErrorKind::InvalidRequest.is_transient());
    }

    #[test]
    fn classify_provider_error_variants() {
        let api = ProviderError::Api {
            status: 429,
            body: "rate limited".into(),
        };
        assert!(api.is_transient());
        assert_eq!(api.classify().kind, ApiErrorKind::RateLimited);

        let stream = ProviderError::Stream("connection reset".into());
        assert!(stream.is_transient());
        assert_eq!(stream.classify().kind, ApiErrorKind::Network);

        let auth = ProviderError::Api {
            status: 401,
            body: r#"{"error":{"message":"invalid api key"}}"#.into(),
        };
        assert!(!auth.is_transient());
        assert_eq!(auth.classify().kind, ApiErrorKind::Auth);
    }

    #[test]
    fn classify_preserves_original_message() {
        let body = r#"{"error":{"message":"You exceeded your current quota","code":"insufficient_api_quota"}}"#;
        let classified = classify_api_error(429, body);
        assert_eq!(classified.kind, ApiErrorKind::QuotaExhausted);
        assert_eq!(classified.code.as_deref(), Some("insufficient_api_quota"));
        assert!(
            classified.message.contains("exceeded your current quota"),
            "原始 msg 必须保留，实际 {}",
            classified.message
        );
    }

    #[test]
    fn classify_truncates_long_body() {
        let long = "x".repeat(10_000);
        let classified = classify_api_error(500, &long);
        assert_eq!(classified.message.chars().count(), MAX_MESSAGE_LEN);
    }

    #[test]
    fn request_error_is_invalid_request() {
        let err = ProviderError::Request("missing model".into());
        assert!(!err.is_transient());
        assert_eq!(err.classify().kind, ApiErrorKind::InvalidRequest);
    }
}
