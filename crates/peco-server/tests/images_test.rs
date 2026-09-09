// ============================================================================
// 会话图片上传集成测试 — 上传、静态直出、归属校验与用户隔离
// ============================================================================
//
// 端点：`POST /api/chat/{agentId}/conversations/{id}/images`（multipart `file`）。
// 存储按用户目录隔离（`uploads/chat/{user_id}/`），发送时 `resolve_image_parts`
// 再按目录隔离解析 —— 引用他人图片一律 404。静态服务经 `/uploads/...` 直出。

mod common;

use common::TestApp;
use reqwest::multipart::Part;
use serde_json::json;

/// 最小 PNG 载荷：8 字节签名 + IHDR 头（非完整 PNG 文件；服务端不校验
/// magic bytes，仅检查声明的 MIME、空体与大小）。
const MINIMAL_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
];

async fn create_conversation(app: &TestApp, title: &str) -> String {
    let resp = app
        .post("/api/conversations")
        .json(&json!({ "title": title }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    body["id"].as_str().unwrap().to_string()
}

async fn upload_image(app: &TestApp, conv_id: &str, token: Option<&str>) -> reqwest::Response {
    let part = Part::bytes(MINIMAL_PNG)
        .file_name("a.png")
        .mime_str("image/png")
        .unwrap();
    let form = reqwest::multipart::Form::new().part("file", part);
    // reqwest 的 header/bearer_auth 是 append 语义，两次调用会产生两个
    // Authorization 头 —— 必须二选一设置。
    let req = app.client.post(format!(
        "{}/api/chat/any-agent/conversations/{conv_id}/images",
        app.base_url
    ));
    let req = match token {
        Some(t) => req.bearer_auth(t),
        None => req.bearer_auth(&app.user_token),
    };
    req.multipart(form).send().await.unwrap()
}

#[tokio::test]
async fn test_upload_image_returns_id_and_url() {
    let app = TestApp::new().await;
    let conv_id = create_conversation(&app, "图片对话").await;

    let resp = upload_image(&app, &conv_id, None).await;
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();

    let id = body["id"].as_str().expect("id 应为文件名");
    let url = body["url"].as_str().expect("url 应为访问路径");
    assert!(id.ends_with(".png"));
    assert!(url.starts_with(&format!("/uploads/chat/{}/", app.user_id)));

    // 上传的文件真实落盘且经 /uploads 静态直出。
    let stored = app
        .client
        .get(format!("{}{url}", app.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(stored.status(), 200);
    assert_eq!(stored.bytes().await.unwrap().as_ref(), MINIMAL_PNG);
}

#[tokio::test]
async fn test_upload_image_requires_owned_conversation() {
    let app = TestApp::new().await;
    let conv_id = create_conversation(&app, "我的对话").await;

    // 未认证请求被 JWT 中间件拒绝。
    let anon = app
        .client
        .post(format!(
            "{}/api/chat/any-agent/conversations/{conv_id}/images",
            app.base_url
        ))
        .multipart(
            reqwest::multipart::Form::new().part(
                "file",
                Part::bytes(MINIMAL_PNG)
                    .file_name("a.png")
                    .mime_str("image/png")
                    .unwrap(),
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status(), 401);

    // 第二个用户向他人对话上传 → 404（归属校验，不泄露会话存在性）。
    let (user2_id, user2_token) = app.register_user2().await;
    assert_ne!(user2_id, app.user_id);
    let resp = upload_image(&app, &conv_id, Some(&user2_token)).await;
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_upload_image_rejects_unsupported_mime_and_empty_body() {
    let app = TestApp::new().await;
    let conv_id = create_conversation(&app, "校验对话").await;

    let text_part = Part::bytes(b"not an image".as_slice())
        .file_name("a.txt")
        .mime_str("text/plain")
        .unwrap();
    let resp = app
        .client
        .post(format!(
            "{}/api/chat/any-agent/conversations/{conv_id}/images",
            app.base_url
        ))
        .bearer_auth(&app.user_token)
        .multipart(reqwest::multipart::Form::new().part("file", text_part))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let empty_part = Part::bytes(Vec::new())
        .file_name("a.png")
        .mime_str("image/png")
        .unwrap();
    let resp = app
        .client
        .post(format!(
            "{}/api/chat/any-agent/conversations/{conv_id}/images",
            app.base_url
        ))
        .bearer_auth(&app.user_token)
        .multipart(reqwest::multipart::Form::new().part("file", empty_part))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_upload_image_rejects_oversized_payload() {
    let app = TestApp::new().await;
    let conv_id = create_conversation(&app, "超限对话").await;

    // 10 MB 上限 + 1 字节 → 400，命中 handler 自身的大小检查（路由层
    // DefaultBodyLimit 已抬高到 11 MB，不会先被 multipart 默认限制拦截）。
    let oversized = vec![0u8; 10 * 1024 * 1024 + 1];
    let part = Part::bytes(oversized)
        .file_name("big.png")
        .mime_str("image/png")
        .unwrap();
    let resp = app
        .client
        .post(format!(
            "{}/api/chat/any-agent/conversations/{conv_id}/images",
            app.base_url
        ))
        .bearer_auth(&app.user_token)
        .multipart(reqwest::multipart::Form::new().part("file", part))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["details"]
            .as_str()
            .unwrap()
            .starts_with("file too large"),
        "unexpected details: {body}"
    );
}
