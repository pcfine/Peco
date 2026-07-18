// ============================================================================
// ApiError — 统一错误类型，实现 axum::response::IntoResponse
// ============================================================================

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// 统一 API 错误类型。
///
/// 每个变体携带可读的错误描述信息，通过 [`IntoResponse`] 自动映射到
/// 对应的 HTTP 状态码和 JSON 响应体。
#[derive(Debug)]
pub enum ApiError {
    /// 400 — 请求参数不合法
    BadRequest(String),
    /// 401 — 未认证或认证失败
    Unauthorized(String),
    /// 403 — 无权限执行操作
    Forbidden(String),
    /// 404 — 资源不存在
    NotFound(String),
    /// 409 — 资源冲突（如重复注册）
    Conflict(String),
    /// 500 — 服务器内部错误
    Internal(String),
}

impl ApiError {
    /// 返回错误消息字符串（用于日志记录）。
    pub fn message(&self) -> &str {
        match self {
            ApiError::BadRequest(m)
            | ApiError::Unauthorized(m)
            | ApiError::Forbidden(m)
            | ApiError::NotFound(m)
            | ApiError::Conflict(m)
            | ApiError::Internal(m) => m.as_str(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::BadRequest(m) => write!(f, "Bad request: {m}"),
            ApiError::Unauthorized(m) => write!(f, "Unauthorized: {m}"),
            ApiError::Forbidden(m) => write!(f, "Forbidden: {m}"),
            ApiError::NotFound(m) => write!(f, "Not found: {m}"),
            ApiError::Conflict(m) => write!(f, "Conflict: {m}"),
            ApiError::Internal(m) => write!(f, "Internal error: {m}"),
        }
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error_type) = match &self {
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ApiError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ApiError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            ApiError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            ApiError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };

        let body = json!({
            "error": error_type,
            "details": self.message(),
        });

        (status, Json(body)).into_response()
    }
}

// ── From impls for common error conversions ───────────────────────────────

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error = %e, "Database error");
        match &e {
            sqlx::Error::RowNotFound => ApiError::NotFound("resource not found".into()),
            _ => {
                if let Some(db_err) = e.as_database_error() {
                    if db_err.is_unique_violation() {
                        return ApiError::Conflict("resource already exists".into());
                    }
                }
                ApiError::Internal(format!("database error: {e}"))
            }
        }
    }
}

impl From<jsonwebtoken::errors::Error> for ApiError {
    fn from(e: jsonwebtoken::errors::Error) -> Self {
        ApiError::Unauthorized(format!("invalid token: {e}"))
    }
}

impl From<bcrypt::BcryptError> for ApiError {
    fn from(e: bcrypt::BcryptError) -> Self {
        ApiError::Internal(format!("password hashing error: {e}"))
    }
}
