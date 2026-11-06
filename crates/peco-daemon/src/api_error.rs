//! 资源模块（agent / skill / knowledge / mcp）的 HTTP 错误类型。
//!
//! 与 [`crate::error::DaemonError`] 的分工：`DaemonError` 描述空间生命周期的
//! 失败面（marker、团队绑定、打开/关闭门槛、库与 IO），本类型描述「这一条请求
//! 哪里不对」（参数非法、资源不存在、名字保留）。桥接是单向的 —— 领域错误可以
//! 变成 HTTP 错误，反之不成立，避免生命周期判定被请求校验污染。
//!
//! 响应体保持前端已消费的 `{error, details}` 形状；只有需要同一状态码下再分类
//! 的错误才追加 `code` 字段。

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::error::DaemonError;

/// 统一 API 错误类型，经 [`IntoResponse`] 映射到状态码与 JSON 响应体。
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
    /// 415 — 内容类型不支持（如二进制文件预览）
    UnsupportedMedia(String),
    /// 504 — 上游/本地处理超时
    GatewayTimeout(String),
    /// 状态码相同但类别需要区分的错误（`space_required`、`reserved_agent` 等）。
    Coded {
        status: StatusCode,
        code: &'static str,
        message: String,
    },
}

impl ApiError {
    /// 带稳定 code 的错误；调用方按 code 分支，不解析文案。
    pub fn coded(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self::Coded {
            status,
            code,
            message: message.into(),
        }
    }

    /// 可读文案（日志与响应体的 `details`）。
    pub fn message(&self) -> &str {
        match self {
            ApiError::BadRequest(m)
            | ApiError::Unauthorized(m)
            | ApiError::Forbidden(m)
            | ApiError::NotFound(m)
            | ApiError::Conflict(m)
            | ApiError::Internal(m)
            | ApiError::UnsupportedMedia(m)
            | ApiError::GatewayTimeout(m) => m.as_str(),
            ApiError::Coded { message, .. } => message.as_str(),
        }
    }

    /// 稳定 code —— 与响应体的 `code` 字段同源。
    pub fn code(&self) -> &'static str {
        match self {
            ApiError::BadRequest(_) => "bad_request",
            ApiError::Unauthorized(_) => "unauthorized",
            ApiError::Forbidden(_) => "forbidden",
            ApiError::NotFound(_) => "not_found",
            ApiError::Conflict(_) => "conflict",
            ApiError::Internal(_) => "internal",
            ApiError::UnsupportedMedia(_) => "unsupported_media",
            ApiError::GatewayTimeout(_) => "gateway_timeout",
            ApiError::Coded { code, .. } => code,
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::UnsupportedMedia(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ApiError::GatewayTimeout(_) => StatusCode::GATEWAY_TIMEOUT,
            ApiError::Coded { status, .. } => *status,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", status_label(self.status()), self.message())
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() {
            tracing::error!(code = self.code(), error = %self.message(), "request failed");
        }
        let error = status_label(status);
        let details = self.message();
        let body = match &self {
            ApiError::Coded { code, .. } => {
                json!({ "error": error, "details": details, "code": code })
            }
            _ => json!({ "error": error, "details": details }),
        };
        (status, Json(body)).into_response()
    }
}

/// 响应体的 `error` 字段是状态类别，前端按它选文案；细分类别走 `code`。
fn status_label(status: StatusCode) -> &'static str {
    match status {
        StatusCode::BAD_REQUEST => "bad_request",
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::FORBIDDEN => "forbidden",
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::CONFLICT => "conflict",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media",
        StatusCode::GATEWAY_TIMEOUT => "gateway_timeout",
        StatusCode::INTERNAL_SERVER_ERROR => "internal",
        _ => "error",
    }
}

// ── From impls ──────────────────────────────────────────────────────────────

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error = %e, "Database error");
        match &e {
            sqlx::Error::RowNotFound => ApiError::NotFound("resource not found".into()),
            _ => {
                if let Some(db_err) = e.as_database_error()
                    && db_err.is_unique_violation()
                {
                    return ApiError::Conflict("resource already exists".into());
                }
                ApiError::Internal(format!("database error: {e}"))
            }
        }
    }
}

impl From<DaemonError> for ApiError {
    /// 领域错误的状态码与 code 原样透传：daemon 的 `code` 已经是稳定契约
    /// （HTTP 文档、测试、前端分类都依赖它），在这里重新分类会造出第二套名字。
    fn from(e: DaemonError) -> Self {
        ApiError::Coded {
            status: e.http_status(),
            code: e.code(),
            message: e.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 领域错误透传：状态码与 code 都不得被 API 层改写。
    #[test]
    fn daemon_error_keeps_its_code_and_status() {
        let error: ApiError = DaemonError::SpaceNotOpen {
            ws_id: "ws_a".into(),
        }
        .into();
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.code(), "space_not_open");

        let error: ApiError = DaemonError::SpaceNotFound {
            ws_id: "ws_a".into(),
        }
        .into();
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.code(), "space_not_found");
    }

    /// `Coded` 之外既有变体的 `error` 字段不变 —— 前端按 `{error, details}` 已消费。
    #[test]
    fn plain_variants_keep_the_existing_label() {
        assert_eq!(
            status_label(ApiError::NotFound(String::new()).status()),
            "not_found"
        );
        assert_eq!(
            status_label(ApiError::UnsupportedMedia(String::new()).status()),
            "unsupported_media"
        );
    }
}
