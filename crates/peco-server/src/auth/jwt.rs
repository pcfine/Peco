// ============================================================================
// JWT 签发与验证
// ============================================================================

use chrono::Utc;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

/// JWT Claims 负载。
#[derive(Debug, Serialize, Deserialize)]
pub struct JwtClaims {
    /// Subject — 用户 ID。
    pub sub: String,
    /// 过期时间（Unix 时间戳，秒）。
    pub exp: usize,
    /// 签发时间（Unix 时间戳，秒）。
    pub iat: usize,
}

/// 为用户签发 JWT Token。
///
/// Token 有效期 7 天，使用 HS256 算法签名。
pub fn create_token(user_id: &str, secret: &str) -> Result<String, jsonwebtoken::errors::Error> {
    let now = Utc::now();
    let claims = JwtClaims {
        sub: user_id.to_string(),
        iat: now.timestamp() as usize,
        exp: (now.timestamp() + 7 * 24 * 3600) as usize, // 7 days
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

/// 验证 JWT Token 并返回 Claims。
///
/// 验证签名和过期时间，不验证 audience/issuer。
pub fn verify_token(token: &str, secret: &str) -> Result<JwtClaims, jsonwebtoken::errors::Error> {
    let token_data = decode::<JwtClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )?;
    Ok(token_data.claims)
}
