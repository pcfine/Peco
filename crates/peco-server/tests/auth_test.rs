// ============================================================================
// Auth 集成测试 — 注册、登录、获取当前用户、认证中间件
// ============================================================================

mod common;

use common::TestApp;
use serde_json::json;

// ── 注册 ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_register_success() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .post(format!("{}/api/auth/register", app.base_url))
        .json(&json!({
            "username": "newuser",
            "email": "new@example.com",
            "password": "secret123"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["token"].as_str().is_some());
    assert_eq!(body["user"]["username"], "newuser");
    assert_eq!(body["user"]["email"], "new@example.com");
}

#[tokio::test]
async fn test_register_duplicate_email() {
    let app = TestApp::new().await;

    // 同一个 email 注册两次
    let resp = app
        .client
        .post(format!("{}/api/auth/register", app.base_url))
        .json(&json!({
            "username": "dupuser",
            "email": "test@example.com",  // 与 TestApp 预注册用户相同
            "password": "secret123"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 409);
}

#[tokio::test]
async fn test_register_empty_username() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .post(format!("{}/api/auth/register", app.base_url))
        .json(&json!({
            "username": "",
            "email": "a@b.com",
            "password": "secret123"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_register_short_password() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .post(format!("{}/api/auth/register", app.base_url))
        .json(&json!({
            "username": "shortpwd",
            "email": "short@example.com",
            "password": "12345"  // < 6 字符
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

// ── 登录 ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_login_success() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .post(format!("{}/api/auth/login", app.base_url))
        .json(&json!({
            "email": "test@example.com",
            "password": "testpass123"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["token"].as_str().is_some());
    assert_eq!(body["user"]["username"], "testuser");
}

#[tokio::test]
async fn test_login_wrong_password() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .post(format!("{}/api/auth/login", app.base_url))
        .json(&json!({
            "email": "test@example.com",
            "password": "wrongpassword"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_login_nonexistent_email() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .post(format!("{}/api/auth/login", app.base_url))
        .json(&json!({
            "email": "nonexistent@example.com",
            "password": "whatever"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

// ── 获取当前用户 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_me_with_valid_token() {
    let app = TestApp::new().await;

    let resp = app.get("/api/auth/me").send().await.unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["username"], "testuser");
    assert_eq!(body["email"], "test@example.com");
}

#[tokio::test]
async fn test_me_without_token() {
    let app = TestApp::new().await;

    // 不带 Authorization header
    let resp = app
        .client
        .get(format!("{}/api/auth/me", app.base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_me_with_invalid_token() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .get(format!("{}/api/auth/me", app.base_url))
        .bearer_auth("invalid-token-that-is-not-valid")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

// ── 受保护路由 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_protected_route_no_auth() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .get(format!("{}/api/agents", app.base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_protected_route_with_auth() {
    let app = TestApp::new().await;

    let resp = app.get("/api/agents").send().await.unwrap();

    assert_eq!(resp.status(), 200);
}
