//! `/api/auth/*` —— 账号 + 密码认证，以及罩住其余接口的认证闸门。
//!
//! 闸门只有一份。逐路由挂 `from_fn` 会让新增路由默认落在闸门外，「忘了挂」和
//! 「故意不挂」在代码里长得一样；因此整棵路由挂一层 [`require_auth`]，由它按路径
//! 决定放不放行。
//!
//! 前端产物必须公开：登录页本身要能在未认证时加载，闸门只罩 `/api` 与 `/uploads`。

use std::convert::Infallible;
use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api_error::ApiError;
use crate::db::{auth_sessions, users};
use crate::state::AppState;
use crate::web_auth::{SESSION_COOKIE, password, token};

/// 闸门之外的三条：注册与登录本来就发生在没有凭据的时候，状态查询要能回答
/// 「还没注册过」。
const PUBLIC_PATHS: [&str; 3] = ["/api/auth/status", "/api/auth/register", "/api/auth/login"];

pub async fn require_auth(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if !needs_credentials(request.uri().path()) {
        return next.run(request).await;
    }
    match principal(&state, request.headers()).await {
        Ok(Some(_)) => next.run(request).await,
        Ok(None) => unauthenticated(),
        Err(e) => {
            tracing::error!(error = %e, "cannot resolve the request credentials");
            coded(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    }
}

fn needs_credentials(path: &str) -> bool {
    crate::http::static_files::is_api_path(path) && !is_public_path(path)
}

fn is_public_path(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    PUBLIC_PATHS.contains(&path)
}

struct Principal {
    /// 经会话 cookie 认证时的会话 id；Bearer 令牌没有会话行。
    session_id: Option<String>,
}

/// 令牌走恒定时间比较，会话走摘要查库：前者不进库是为了让脚本与 curl 不必先登录，
/// 后者让浏览器只持有一枚 30 天滑动过期的凭据。
async fn principal(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<Principal>, sqlx::Error> {
    if let Some(candidate) = bearer_token(headers)
        && state.web_auth.token_matches(candidate)
    {
        return Ok(Some(Principal { session_id: None }));
    }

    let Some(ticket) = cookie_value(headers, SESSION_COOKIE) else {
        return Ok(None);
    };
    let Some(session) = auth_sessions::find_valid(&state.pool, &token::sha256_hex(ticket)).await?
    else {
        return Ok(None);
    };
    if session.stale {
        auth_sessions::touch(&state.pool, &session.id).await?;
    }
    Ok(Some(Principal {
        session_id: Some(session.id),
    }))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
}

/// 客户端地址。只有 `into_make_service_with_connect_info` 装路由时才有，
/// 进程内 `oneshot` 的请求没有这一层，故缺失是常态而非错误。
///
/// `pub` 是因为它进了 handler 签名：路由那一侧要能看见它。
pub struct ClientIp(Option<String>);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &AppState) -> Result<Self, Infallible> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(addr)| addr.ip().to_string()),
        ))
    }
}

#[derive(Debug, Serialize)]
pub struct StatusView {
    pub registered: bool,
    pub authenticated: bool,
    pub username: Option<String>,
}

/// 未认证时 `username` 恒为 `null`：知道用户名等于把爆破面缩到只剩密码。
pub async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<StatusView>, ApiError> {
    let account = users::account(&state.pool).await?;
    let authenticated = principal(&state, &headers).await?.is_some();
    let registered = account.as_ref().is_some_and(users::Account::is_registered);
    let username = if authenticated {
        account.map(|account| account.username)
    } else {
        None
    };
    Ok(Json(StatusView {
        registered,
        authenticated,
        username,
    }))
}

#[derive(Debug, Deserialize)]
pub struct RegisterBody {
    username: String,
    password: String,
    setup_token: String,
}

/// 首次注册。`setup_token` 与 token 文件同源 —— 拿到它的进程本来就能改这个库。
pub async fn register(
    State(state): State<AppState>,
    ClientIp(client): ClientIp,
    headers: HeaderMap,
    Json(body): Json<RegisterBody>,
) -> Response {
    if !state.web_auth.token_matches(&body.setup_token) {
        return coded(StatusCode::FORBIDDEN, "invalid_setup_token");
    }
    let username = match password::validate_username(&body.username) {
        Ok(username) => username,
        Err(details) => return invalid_input(details),
    };
    if let Err(details) = password::validate_password(&body.password) {
        return invalid_input(details);
    }
    let hash = match password::hash(&body.password).await {
        Ok(hash) => hash,
        Err(e) => return internal(&e),
    };
    match users::register(&state.pool, &username, &hash).await {
        // 判定在 UPDATE 的谓词里：并发注册只有一个能换掉空摘要。
        Ok(true) => {}
        Ok(false) => return coded(StatusCode::CONFLICT, "already_registered"),
        Err(e) => return internal(&e),
    }
    start_session(&state, &username, client.as_deref(), user_agent(&headers)).await
}

#[derive(Debug, Deserialize)]
pub struct LoginBody {
    username: String,
    password: String,
}

pub async fn login(
    State(state): State<AppState>,
    ClientIp(client): ClientIp,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Response {
    let client = client.unwrap_or_else(|| "unknown".to_string());
    if let Some(retry_after) = state.web_auth.login_retry_after(&client) {
        return too_many_attempts(retry_after);
    }

    let account = match users::account(&state.pool).await {
        Ok(account) => account,
        Err(e) => return internal(&e),
    };
    let matched = match &account {
        Some(account) if account.is_registered() && account.username == body.username.trim() => {
            password::verify(&body.password, &account.password_hash).await
        }
        // 未知用户名也烧一次 bcrypt：跳过它，「答得快」就等于「用户不存在」。
        _ => {
            password::verify_against_dummy(&body.password).await;
            false
        }
    };
    if !matched {
        state.web_auth.note_login_failure(&client);
        return coded(StatusCode::UNAUTHORIZED, "invalid_credentials");
    }

    state.web_auth.note_login_success(&client);
    let username = account.map(|account| account.username).unwrap_or_default();
    start_session(&state, &username, Some(&client), user_agent(&headers)).await
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let principal = match principal(&state, &headers).await {
        Ok(principal) => principal,
        Err(e) => return internal(&e),
    };
    // 闸门已保证有身份；Bearer 令牌没有会话行，删不掉也不需要删。
    if let Some(session_id) = principal.and_then(|principal| principal.session_id)
        && let Err(e) = auth_sessions::delete(&state.pool, &session_id).await
    {
        return internal(&e);
    }
    with_cookie(
        Json(json!({ "ok": true })).into_response(),
        state.web_auth.cleared_cookie(),
    )
}

#[derive(Debug, Deserialize)]
pub struct PasswordBody {
    current_password: String,
    new_password: String,
}

/// 改密码并吊销其它会话：口令轮换的常见动机是「另一台设备上的会话不该再有效」。
pub async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PasswordBody>,
) -> Response {
    let principal = match principal(&state, &headers).await {
        Ok(principal) => principal,
        Err(e) => return internal(&e),
    };
    let account = match users::account(&state.pool).await {
        Ok(account) => account,
        Err(e) => return internal(&e),
    };
    let Some(account) = account.filter(users::Account::is_registered) else {
        return coded(StatusCode::UNAUTHORIZED, "invalid_credentials");
    };
    if !password::verify(&body.current_password, &account.password_hash).await {
        return coded(StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
    if let Err(details) = password::validate_password(&body.new_password) {
        return invalid_input(details);
    }
    let hash = match password::hash(&body.new_password).await {
        Ok(hash) => hash,
        Err(e) => return internal(&e),
    };
    match users::set_password_hash(&state.pool, &hash).await {
        Ok(true) => {}
        // 只有种子行缺失才会走到这里（注册过的库不可能没有这一行）。
        Ok(false) => return internal(&"the local user row is missing"),
        Err(e) => return internal(&e),
    }

    let keep = principal.and_then(|principal| principal.session_id);
    match auth_sessions::delete_others(&state.pool, crate::LOCAL_USER_ID, keep.as_deref()).await {
        Ok(revoked) => tracing::info!(revoked, "password changed; other sessions revoked"),
        Err(e) => return internal(&e),
    }
    Json(json!({ "ok": true })).into_response()
}

/// 建一条会话并回 `{"username"}` + Set-Cookie。
async fn start_session(
    state: &AppState,
    username: &str,
    client: Option<&str>,
    user_agent: Option<&str>,
) -> Response {
    if let Err(e) = auth_sessions::purge_expired(&state.pool).await {
        tracing::warn!(error = %e, "cannot purge the expired sessions");
    }

    let ticket = token::new_ticket();
    let session_id = token::new_ticket();
    let token_hash = token::sha256_hex(&ticket);
    let session = auth_sessions::NewSession {
        id: &session_id,
        token_hash: &token_hash,
        user_agent,
        ip: client,
    };
    if let Err(e) = auth_sessions::insert(&state.pool, &session).await {
        return internal(&e);
    }
    with_cookie(
        Json(json!({ "username": username })).into_response(),
        state.web_auth.session_cookie(&ticket),
    )
}

fn user_agent(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
}

fn with_cookie(mut response: Response, cookie: String) -> Response {
    // 值全是 hex 与固定属性，构造失败只可能是本文件写错了字面量。
    match HeaderValue::from_str(&cookie) {
        Ok(value) => {
            response.headers_mut().insert(header::SET_COOKIE, value);
            response
        }
        Err(e) => {
            tracing::error!(error = %e, "cannot build the session cookie header");
            coded(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    }
}

fn unauthenticated() -> Response {
    coded(StatusCode::UNAUTHORIZED, "unauthenticated")
}

fn invalid_input(details: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "code": "invalid_input", "details": details.into() })),
    )
        .into_response()
}

fn too_many_attempts(retry_after: std::time::Duration) -> Response {
    // 向上取整：回 0 等于「现在就能试」，与 429 自相矛盾。
    let secs = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({ "code": "too_many_attempts", "retry_after": secs.max(1) })),
    )
        .into_response()
}

fn coded(status: StatusCode, code: &'static str) -> Response {
    (status, Json(json!({ "code": code }))).into_response()
}

fn internal(error: &dyn std::fmt::Display) -> Response {
    tracing::error!(error = %error, "auth request failed");
    coded(StatusCode::INTERNAL_SERVER_ERROR, "internal")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三条公开端点、两个受保护前缀；尾斜杠与大小写变形都得归到同一侧。
    ///
    /// 变异方法：把 `needs_credentials` 改成整棵路由放行 ⇒ 受保护路径的断言变红。
    #[test]
    fn only_the_three_public_endpoints_stay_open() {
        for path in ["/api/auth/status", "/api/auth/register", "/api/auth/login"] {
            assert!(!needs_credentials(path), "{path} 必须在闸门外");
            assert!(!needs_credentials(&format!("{path}/")), "{path}/ 同一条");
        }
        for path in [
            "/api/auth/logout",
            "/api/auth/password",
            "/api/spaces",
            "/api/peco",
            "/uploads/a.png",
            // 变形写法与静态层同一套判定。
            "/API/spaces",
            "/%61pi/spaces",
        ] {
            assert!(needs_credentials(path), "{path} 必须走闸门");
        }
        for path in ["/", "/index.html", "/spaces/ws_peco"] {
            assert!(!needs_credentials(path), "{path} 是前端产物，必须公开");
        }
    }

    #[test]
    fn bearer_tokens_are_read_from_the_authorization_header() {
        let mut headers = HeaderMap::new();
        assert_eq!(bearer_token(&headers), None);
        headers.insert(header::AUTHORIZATION, "Basic abc".parse().unwrap());
        assert_eq!(bearer_token(&headers), None, "只认 Bearer");
        headers.insert(header::AUTHORIZATION, "Bearer  deadbeef ".parse().unwrap());
        assert_eq!(bearer_token(&headers), Some("deadbeef"));
        headers.insert(header::AUTHORIZATION, "Bearer  ".parse().unwrap());
        assert_eq!(bearer_token(&headers), None);
    }

    #[test]
    fn cookies_are_matched_by_name_among_several() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "a=1; peco_session=abc123; b=2".parse().unwrap(),
        );
        assert_eq!(cookie_value(&headers, SESSION_COOKIE), Some("abc123"));
        assert_eq!(cookie_value(&headers, "missing"), None);
        headers.insert(header::COOKIE, "peco_session=".parse().unwrap());
        assert_eq!(cookie_value(&headers, SESSION_COOKIE), None, "空值不算凭据");
    }

    /// 变异方法：去掉向上取整 ⇒ 999ms 的等待回成 0，断言变红。
    #[tokio::test]
    async fn retry_after_rounds_up_to_positive_whole_seconds() {
        let read = |response: Response| async move {
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
        };

        let response = too_many_attempts(std::time::Duration::from_millis(999));
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = read(response).await;
        assert_eq!(body["code"], "too_many_attempts");
        assert_eq!(body["retry_after"], 1);

        let response = too_many_attempts(std::time::Duration::ZERO);
        let zero = read(response).await;
        assert_eq!(zero["retry_after"], 1, "零等待也要回 1s");
    }
}
