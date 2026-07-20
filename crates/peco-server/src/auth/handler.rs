// ============================================================================
// Auth Handlers — 注册、登录、获取当前用户
// ============================================================================

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

use super::jwt;
use super::middleware::AuthUser;

// ── Request / Response 类型 ─────────────────────────────────────────────────

/// 注册请求体。
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub email: String,
    pub password: String,
}

/// 登录请求体。
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// 用户信息响应（不含密码）。
#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: String,
    pub username: String,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    pub created_at: String,
}

/// 认证成功响应（注册/登录共用）。
#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub user: UserResponse,
    pub token: String,
}

// ── 数据库行类型 ────────────────────────────────────────────────────────────

/// users 表完整行（含 password_hash）。
#[derive(Debug, sqlx::FromRow)]
struct UserRow {
    id: String,
    username: String,
    email: String,
    password_hash: String,
    avatar: Option<String>,
    created_at: String,
}

/// users 表公开行（不含 password_hash）。
#[derive(Debug, sqlx::FromRow)]
struct UserPublicRow {
    id: String,
    username: String,
    email: String,
    avatar: Option<String>,
    created_at: String,
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `POST /api/auth/register`
///
/// 创建新用户并返回 JWT Token。
///
/// # 验证
/// - username 不能为空
/// - email 必须包含 @
/// - password 至少 6 位
/// - username 和 email 均不可重复
pub async fn register(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<AuthResponse>), ApiError> {
    // ── 输入验证 ──────────────────────────────────────────────────────────
    let username = req.username.trim();
    let email = req.email.trim();

    if username.is_empty() {
        return Err(ApiError::BadRequest("username is required".into()));
    }
    if email.is_empty() || !email.contains('@') {
        return Err(ApiError::BadRequest("valid email is required".into()));
    }
    if req.password.len() < 6 {
        return Err(ApiError::BadRequest(
            "password must be at least 6 characters".into(),
        ));
    }

    // ── 检查重复 ──────────────────────────────────────────────────────────
    let existing =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE email = ? OR username = ?")
            .bind(email)
            .bind(username)
            .fetch_one(&state.db)
            .await?;

    if existing > 0 {
        return Err(ApiError::Conflict(
            "email or username already registered".into(),
        ));
    }

    // ── 密码哈希（CPU 密集型，放入 spawn_blocking）───────────────────────
    let password_hash = {
        let pwd = req.password.clone();
        tokio::task::spawn_blocking(move || bcrypt::hash(pwd, 12))
            .await
            .map_err(|e| ApiError::Internal(format!("bcrypt task panicked: {e}")))?
            .map_err(|e| ApiError::Internal(format!("password hashing failed: {e}")))?
    };

    // ── 插入用户 ──────────────────────────────────────────────────────────
    let user_id = Uuid::new_v4().to_string();

    sqlx::query("INSERT INTO users (id, username, email, password_hash) VALUES (?, ?, ?, ?)")
        .bind(&user_id)
        .bind(username)
        .bind(email)
        .bind(&password_hash)
        .execute(&state.db)
        .await?;

    // ── 签发 JWT ──────────────────────────────────────────────────────────
    let token = jwt::create_token(&user_id, &state.jwt_secret)
        .map_err(|e| ApiError::Internal(format!("token generation failed: {e}")))?;

    // ── 读取用户完整信息（含 created_at）─────────────────────────────────
    let user = sqlx::query_as::<_, UserPublicRow>(
        "SELECT id, username, email, avatar, created_at FROM users WHERE id = ?",
    )
    .bind(&user_id)
    .fetch_one(&state.db)
    .await?;

    let user = UserResponse {
        id: user.id,
        username: user.username,
        email: user.email,
        avatar: user.avatar,
        created_at: user.created_at,
    };

    tracing::info!(
        user_id = %user.id,
        username = %user.username,
        "User registered successfully"
    );

    Ok((StatusCode::CREATED, Json(AuthResponse { user, token })))
}

/// `POST /api/auth/login`
///
/// 验证邮箱和密码，返回 JWT Token。
pub async fn login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    let email = req.email.trim();

    if email.is_empty() || req.password.is_empty() {
        return Err(ApiError::BadRequest(
            "email and password are required".into(),
        ));
    }

    // ── 查找用户 ──────────────────────────────────────────────────────────
    let user = sqlx::query_as::<_, UserRow>(
        "SELECT id, username, email, password_hash, avatar, created_at FROM users WHERE email = ?",
    )
    .bind(email)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::Unauthorized("invalid email or password".into()))?;

    // ── 验证密码（CPU 密集型，放入 spawn_blocking）───────────────────────
    let is_valid = {
        let password = req.password.clone();
        let hash = user.password_hash.clone();
        tokio::task::spawn_blocking(move || bcrypt::verify(password, &hash).unwrap_or(false))
            .await
            .map_err(|e| ApiError::Internal(format!("bcrypt task panicked: {e}")))?
    };

    if !is_valid {
        return Err(ApiError::Unauthorized("invalid email or password".into()));
    }

    // ── 签发 JWT ──────────────────────────────────────────────────────────
    let token = jwt::create_token(&user.id, &state.jwt_secret)
        .map_err(|e| ApiError::Internal(format!("token generation failed: {e}")))?;

    tracing::info!(
        user_id = %user.id,
        username = %user.username,
        "User logged in successfully"
    );

    Ok(Json(AuthResponse {
        user: UserResponse {
            id: user.id,
            username: user.username,
            email: user.email,
            avatar: user.avatar,
            created_at: user.created_at,
        },
        token,
    }))
}

/// `GET /api/auth/me`
///
/// 返回当前认证用户的信息。
pub async fn me(
    AuthUser { user_id }: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<UserResponse>, ApiError> {
    let user = sqlx::query_as::<_, UserPublicRow>(
        "SELECT id, username, email, avatar, created_at FROM users WHERE id = ?",
    )
    .bind(&user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::NotFound("user not found".into()))?;

    Ok(Json(UserResponse {
        id: user.id,
        username: user.username,
        email: user.email,
        avatar: user.avatar,
        created_at: user.created_at,
    }))
}
