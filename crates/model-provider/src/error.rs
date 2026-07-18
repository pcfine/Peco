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
