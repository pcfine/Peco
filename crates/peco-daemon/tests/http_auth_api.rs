//! 认证接口与认证闸门：注册、登录、登出、改密码，以及「没有凭据就别想进门」。
//!
//! 全部走真实路由（`tower::ServiceExt::oneshot`）：闸门是中间件，绕开它测
//! handler 等于把本文件要钉的那一层空掉。

mod support;

use axum::http::{Method, header};
use peco_daemon::config::TOKEN_FILE;
use serde_json::{Value, json};
use support::{BEARER_TOKEN, HttpReply, TestEnv, call_anonymous, call_with_session, new_env};

/// 与夹具令牌不同的另一枚 64 位 hex：它必须过不了 setup_token 校验。
const WRONG_TOKEN: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

async fn status(env: &TestEnv) -> Value {
    let reply = call_anonymous(&env.router(), Method::GET, "/api/auth/status", None).await;
    assert_eq!(reply.status, 200, "状态查询必须免认证：{}", reply.body);
    reply.body
}

async fn register(env: &TestEnv, username: &str, password: &str) -> HttpReply {
    let reply = call_anonymous(
        &env.router(),
        Method::POST,
        "/api/auth/register",
        Some(json!({
            "username": username,
            "password": password,
            "setup_token": BEARER_TOKEN,
        })),
    )
    .await;
    assert_eq!(reply.status, 200, "注册失败：{}", reply.body);
    reply
}

async fn login(env: &TestEnv, username: &str, password: &str) -> HttpReply {
    call_anonymous(
        &env.router(),
        Method::POST,
        "/api/auth/login",
        Some(json!({ "username": username, "password": password })),
    )
    .await
}

/// `Set-Cookie` → `Cookie` 请求头要用的那一段（去掉属性）。
fn session_cookie(reply: &HttpReply) -> String {
    let raw = reply.set_cookie().expect("响应必须带 Set-Cookie");
    raw.split(';').next().unwrap().trim().to_string()
}

async fn spaces_with_cookie(env: &TestEnv, cookie: &str) -> u16 {
    call_with_session(&env.router(), Method::GET, "/api/spaces", None, cookie)
        .await
        .status
        .as_u16()
}

/// 未注册时的公开面：状态可查、接口全挡、setup token 就是 token 文件的内容。
#[tokio::test]
async fn a_fresh_daemon_reports_unregistered_and_keeps_every_api_behind_the_gate() {
    let env = new_env(&[]).await;

    let body = status(&env).await;
    assert_eq!(body["registered"], false);
    assert_eq!(body["authenticated"], false);
    assert_eq!(body["username"], Value::Null, "未认证时不得回用户名");

    // setup token 来自 token 文件：夹具把定值写进去，用例读回来对齐。
    assert_eq!(
        std::fs::read_to_string(env.home.join(TOKEN_FILE)).unwrap(),
        BEARER_TOKEN
    );

    for uri in ["/api/spaces", "/api/peco", "/uploads/missing.png"] {
        let reply = call_anonymous(&env.router(), Method::GET, uri, None).await;
        assert_eq!(reply.status, 401, "{uri} 必须要求认证；实际 {}", reply.body);
        assert_eq!(reply.code(), "unauthenticated", "{uri} 的错误码是契约");
    }

    // 没有 cookie 也一样，且错误体里不带任何身份线索。
    let reply = call_anonymous(&env.router(), Method::GET, "/api/tasks", None).await;
    assert_eq!(reply.status, 401);
    assert_eq!(reply.body, json!({ "code": "unauthenticated" }));
}

/// Bearer 令牌直接过闸门：脚本与运维不必先登录。
#[tokio::test]
async fn the_bearer_token_authorizes_api_requests() {
    let env = new_env(&[]).await;
    let reply = env.get("/api/spaces").await;
    assert_eq!(reply.status, 200, "{}", reply.body);

    let reply = call_anonymous(&env.router(), Method::GET, "/api/spaces", None).await;
    assert_eq!(reply.status, 401);

    // 令牌错一个字照样 401 —— 比较不认前缀，也不认大小写变体。
    let reply = call_with_session(
        &env.router(),
        Method::GET,
        "/api/spaces",
        None,
        &format!("peco_session={}", &BEARER_TOKEN[..63]),
    )
    .await;
    assert_eq!(reply.status, 401, "cookie 里放令牌不算认证");
}

/// 注册的三条拒绝路径 + 成功路径，以及「第二次注册 409」。
#[tokio::test]
async fn registration_needs_the_setup_token_the_input_and_a_free_slot() {
    let env = new_env(&[]).await;

    let reply = call_anonymous(
        &env.router(),
        Method::POST,
        "/api/auth/register",
        Some(
            json!({ "username": "peco", "password": "correct horse", "setup_token": WRONG_TOKEN }),
        ),
    )
    .await;
    assert_eq!(reply.status, 403);
    assert_eq!(reply.code(), "invalid_setup_token");
    assert_eq!(
        status(&env).await["registered"],
        false,
        "被拒的注册不得改库"
    );

    let reply = call_anonymous(
        &env.router(),
        Method::POST,
        "/api/auth/register",
        Some(
            json!({ "username": "   ", "password": "correct horse", "setup_token": BEARER_TOKEN }),
        ),
    )
    .await;
    assert_eq!(reply.status, 400);
    assert_eq!(reply.code(), "invalid_input");
    assert!(reply.body["details"].is_string(), "400 要说明哪里不对");

    let reply = call_anonymous(
        &env.router(),
        Method::POST,
        "/api/auth/register",
        Some(json!({ "username": "peco", "password": "short", "setup_token": BEARER_TOKEN })),
    )
    .await;
    assert_eq!(reply.status, 400, "口令太短必须拒绝");
    assert_eq!(reply.code(), "invalid_input");

    let created = register(&env, "  peco  ", "correct horse").await;
    assert_eq!(
        created.body,
        json!({ "username": "peco" }),
        "用户名要去空白"
    );
    let cookie = created.set_cookie().unwrap().to_string();
    assert!(cookie.starts_with("peco_session="), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");
    assert!(cookie.contains("Path=/"), "{cookie}");
    assert!(cookie.contains("Max-Age=2592000"), "{cookie}");
    assert!(
        !cookie.contains("Secure"),
        "明文部署下带 Secure 会让浏览器丢掉 cookie：{cookie}"
    );

    // 注册即登录：cookie 能直接用。
    assert_eq!(
        spaces_with_cookie(&env, &session_cookie(&created)).await,
        200
    );

    let registered = status(&env).await;
    assert_eq!(registered["registered"], true);
    assert_eq!(
        registered["authenticated"], false,
        "匿名查询不该被认成已认证"
    );
    assert_eq!(registered["username"], Value::Null);

    let authenticated =
        support::call_authed(&env.router(), Method::GET, "/api/auth/status", None).await;
    assert_eq!(authenticated.body["authenticated"], true);
    assert_eq!(authenticated.body["username"], "peco");

    let reply = call_anonymous(
        &env.router(),
        Method::POST,
        "/api/auth/register",
        Some(json!({ "username": "intruder", "password": "another password", "setup_token": BEARER_TOKEN })),
    )
    .await;
    assert_eq!(reply.status, 409);
    assert_eq!(reply.code(), "already_registered");
}

/// 登录：口令不对 401，对了给 cookie；cookie 能顶着闸门取数据。
#[tokio::test]
async fn login_issues_a_session_cookie_that_authorizes_later_requests() {
    let env = new_env(&[]).await;
    register(&env, "peco", "correct horse").await;

    let reply = login(&env, "peco", "wrong horse").await;
    assert_eq!(reply.status, 401);
    assert_eq!(reply.code(), "invalid_credentials");
    assert!(reply.set_cookie().is_none(), "失败的登录不得发 cookie");

    let reply = login(&env, "peco", "correct horse").await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, json!({ "username": "peco" }));
    let cookie = session_cookie(&reply);
    assert_eq!(spaces_with_cookie(&env, cookie.as_str()).await, 200);
}

/// 连续失败到第 6 次就回 429，且 `retry_after` 是正整数秒。
///
/// 变异方法：把 `backoff` 的门槛写成 `failures > FREE_ATTEMPTS` ⇒ 第 6 次仍是 401。
#[tokio::test]
async fn six_failed_logins_end_in_too_many_attempts() {
    let env = new_env(&[]).await;
    register(&env, "peco", "correct horse").await;

    for attempt in 1..=5 {
        let reply = login(&env, "peco", "wrong horse").await;
        assert_eq!(reply.status, 401, "第 {attempt} 次失败仍是凭证错误");
    }

    let blocked = login(&env, "peco", "correct horse").await;
    assert_eq!(blocked.status, 429, "第 6 次必须被限流拦下");
    assert_eq!(blocked.code(), "too_many_attempts");
    let retry_after = blocked.body["retry_after"]
        .as_u64()
        .expect("429 必须带 retry_after 秒数");
    assert!(retry_after >= 1, "retry_after 不得为 0：{retry_after}");
}

/// 未知用户名与错口令表现一致：都只回 invalid_credentials（不泄漏用户是否存在）。
#[tokio::test]
async fn an_unknown_username_looks_exactly_like_a_wrong_password() {
    let env = new_env(&[]).await;
    register(&env, "peco", "correct horse").await;

    let unknown = login(&env, "nobody", "correct horse").await;
    let wrong = login(&env, "peco", "wrong horse").await;
    assert_eq!(unknown.status, 401);
    assert_eq!(unknown.status, wrong.status);
    assert_eq!(unknown.body, wrong.body, "两条路径的响应体必须一致");
}

/// 登出删会话并让浏览器丢掉 cookie。
#[tokio::test]
async fn logout_revokes_the_session_and_clears_the_cookie() {
    let env = new_env(&[]).await;
    let created = register(&env, "peco", "correct horse").await;
    let cookie = session_cookie(&created);

    let reply = call_with_session(
        &env.router(),
        Method::POST,
        "/api/auth/logout",
        Some(json!({})),
        &cookie,
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.body, json!({ "ok": true }));
    let cleared = reply.set_cookie().expect("登出必须清 cookie");
    assert!(
        cleared.contains("Max-Age=0"),
        "登出的 cookie 要立刻过期：{cleared}"
    );

    assert_eq!(spaces_with_cookie(&env, &cookie).await, 401, "会话必须已删");
}

/// 改密码：当前会话留着，其它会话全部失效，新口令才能登录。
#[tokio::test]
async fn changing_the_password_revokes_the_other_sessions() {
    let env = new_env(&[]).await;
    let first = session_cookie(&register(&env, "peco", "correct horse").await);
    let second = session_cookie(&login(&env, "peco", "correct horse").await);
    assert_ne!(first, second, "两次会话是不同的票据");
    assert_eq!(spaces_with_cookie(&env, &first).await, 200);

    let reply = call_with_session(
        &env.router(),
        Method::POST,
        "/api/auth/password",
        Some(json!({ "current_password": "wrong horse", "new_password": "battery staple" })),
        &second,
    )
    .await;
    assert_eq!(reply.status, 401, "当前口令不对必须拒绝");
    assert_eq!(reply.code(), "invalid_credentials");

    let reply = call_with_session(
        &env.router(),
        Method::POST,
        "/api/auth/password",
        Some(json!({ "current_password": "correct horse", "new_password": "short" })),
        &second,
    )
    .await;
    assert_eq!(reply.status, 400, "新口令太短必须拒绝");
    assert_eq!(reply.code(), "invalid_input");

    let reply = call_with_session(
        &env.router(),
        Method::POST,
        "/api/auth/password",
        Some(json!({ "current_password": "correct horse", "new_password": "battery staple" })),
        &second,
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.body, json!({ "ok": true }));

    assert_eq!(
        spaces_with_cookie(&env, &first).await,
        401,
        "另一台设备上的会话必须失效"
    );
    assert_eq!(spaces_with_cookie(&env, &second).await, 200, "当前会话留着");

    assert_eq!(login(&env, "peco", "correct horse").await.status, 401);
    assert_eq!(login(&env, "peco", "battery staple").await.status, 200);
}

/// `Set-Cookie` 之外没有任何地方回传票据。
#[tokio::test]
async fn the_ticket_only_travels_in_the_set_cookie_header() {
    let env = new_env(&[]).await;
    let created = register(&env, "peco", "correct horse").await;
    let cookie = created.set_cookie().unwrap();
    let ticket = cookie
        .split_once('=')
        .and_then(|(_, rest)| rest.split(';').next())
        .unwrap()
        .to_string();
    assert_eq!(ticket.len(), 64, "票据是 64 位 hex：{ticket}");
    assert!(
        !serde_json::to_string(&created.body)
            .unwrap()
            .contains(&ticket),
        "响应体不得含票据：{}",
        created.body
    );
    assert!(
        created.headers.get(header::AUTHORIZATION).is_none(),
        "票据不走 Authorization"
    );
}
