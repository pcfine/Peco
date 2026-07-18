// ============================================================================
// AuthUser — JWT 认证 Extractor（实现 FromRequestParts）
// ============================================================================

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;

use crate::error::ApiError;
use crate::state::AppState;

use super::jwt;

/// 从 JWT Bearer Token 提取的用户身份。
///
/// 用于所有需要认证的路由 handler，作为 extractor 参数使用：
///
/// ```ignore
/// async fn protected_handler(AuthUser { user_id }: AuthUser) -> ... { }
/// ```
///
/// 认证失败时返回 401 Unauthorized。
#[derive(Debug, Clone)]
pub struct AuthUser {
    /// 已验证的用户 ID。
    pub user_id: String,
}

impl FromRequestParts<Arc<AppState>> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        // 1. 提取 Authorization header
        let auth_header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::Unauthorized("missing authorization header".into()))?;

        // 2. 解析 Bearer token
        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or_else(|| ApiError::Unauthorized("invalid authorization format".into()))?;

        // 3. 验证 JWT
        let claims =
            jwt::verify_token(token, &state.jwt_secret)
                .map_err(|e| {
                    tracing::warn!(error = %e, "JWT verification failed");
                    ApiError::Unauthorized(format!("invalid or expired token: {e}"))
                })?;

        // 4. 确认用户存在于数据库中
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM users WHERE id = ?",
        )
        .bind(&claims.sub)
        .fetch_one(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, user_id = %claims.sub, "Database error during auth check");
            ApiError::Internal("authentication check failed".into())
        })?;

        if exists == 0 {
            return Err(ApiError::Unauthorized("user not found".into()));
        }

        Ok(AuthUser {
            user_id: claims.sub,
        })
    }
}
