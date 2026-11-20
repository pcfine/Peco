//! `POST /api/upload` 与 `/uploads` 静态服务：只收图片、URL 可直出。

mod support;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use support::new_env;
use tower::ServiceExt;

fn multipart_body(
    boundary: &str,
    field: &str,
    filename: &str,
    content_type: &str,
    data: &[u8],
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// `file` 在前、`purpose` 在后的 multipart：字段顺序不能影响结果。
fn multipart_body_with_purpose(boundary: &str, purpose: &str, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"brand.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(
        format!(
            "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"purpose\"\r\n\r\n{purpose}\r\n--{boundary}--\r\n"
        )
        .as_bytes(),
    );
    body
}

async fn upload_with_purpose(
    env: &support::TestEnv,
    purpose: &str,
    data: &[u8],
) -> (StatusCode, serde_json::Value) {
    let boundary = "peco-test-boundary";
    let request = support::bearer(Request::builder())
        .method(Method::POST)
        .uri("/api/upload")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(multipart_body_with_purpose(
            boundary, purpose, data,
        )))
        .unwrap();

    let response = env.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn upload(
    env: &support::TestEnv,
    field: &str,
    content_type: &str,
    data: &[u8],
) -> (StatusCode, serde_json::Value) {
    let boundary = "peco-test-boundary";
    let body = multipart_body(boundary, field, "icon.png", content_type, data);
    let request = support::bearer(Request::builder())
        .method(Method::POST)
        .uri("/api/upload")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();

    let response = env.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

/// 合法 PNG：返回 `/uploads/teams/...` URL，文件真的落盘且静态服务能取回。
#[tokio::test]
async fn uploading_a_png_returns_a_served_url() {
    let env = new_env(&[]).await;

    let (status, body) = upload(&env, "file", "image/png", b"\x89PNG\r\n\x1a\n").await;

    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let url = body["url"].as_str().expect("url");
    assert!(url.starts_with("/uploads/teams/"), "url={url}");
    let path = env
        .home
        .join("uploads")
        .join(url.trim_start_matches("/uploads/"));
    assert!(
        path.is_file(),
        "文件必须落在 $PECO_HOME/uploads/teams/，实际路径 {}",
        path.display()
    );

    let served = env.get(url).await;
    assert_eq!(served.status, StatusCode::OK, "静态服务应能取回 {url}");
}

/// SVG 不在白名单内：防止把可执行脚本当图片直出。
#[tokio::test]
async fn uploading_svg_is_rejected() {
    let env = new_env(&[]).await;

    let (status, body) = upload(&env, "file", "image/svg+xml", b"<svg/>").await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
}

/// 空文件与缺失 `file` 字段都是 400。
#[tokio::test]
async fn empty_file_and_missing_field_are_rejected() {
    let env = new_env(&[]).await;

    let (empty, _) = upload(&env, "file", "image/png", b"").await;
    assert_eq!(empty, StatusCode::BAD_REQUEST);

    let (missing, _) = upload(&env, "other", "image/png", b"x").await;
    assert_eq!(missing, StatusCode::BAD_REQUEST);
}

/// `purpose=brand` 落 `uploads/brand/`：目录由服务端白名单决定，URL 前缀随之改变。
#[tokio::test]
async fn brand_purpose_uploads_into_the_brand_directory() {
    let env = new_env(&[]).await;

    let (status, body) = upload_with_purpose(&env, "brand", b"\x89PNG\r\n\x1a\n").await;

    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let url = body["url"].as_str().expect("url");
    assert!(url.starts_with("/uploads/brand/"), "url={url}");
    let path = env
        .home
        .join("uploads")
        .join(url.trim_start_matches("/uploads/"));
    assert!(
        path.is_file(),
        "文件必须落在 $PECO_HOME/uploads/brand/，实际路径 {}",
        path.display()
    );
    assert!(
        !env.home.join("uploads").join("teams").exists(),
        "指定 brand 后不该再顺手建出 teams 目录"
    );
    assert_eq!(env.get(url).await.status, StatusCode::OK);
}

/// 显式写 `purpose=teams` 与缺省同义。
#[tokio::test]
async fn explicit_teams_purpose_keeps_the_existing_directory() {
    let env = new_env(&[]).await;

    let (status, body) = upload_with_purpose(&env, "teams", b"\x89PNG\r\n\x1a\n").await;

    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert!(
        body["url"]
            .as_str()
            .unwrap_or_default()
            .starts_with("/uploads/teams/"),
        "body={body}"
    );
}

/// 白名单之外的用途一律 400，且不落盘 —— 目录名不接受调用方拼。
#[tokio::test]
async fn unknown_purpose_is_rejected_without_writing_anything() {
    let env = new_env(&[]).await;

    for purpose in ["nope", "", "../teams", "brand/../teams"] {
        let (status, body) = upload_with_purpose(&env, purpose, b"\x89PNG\r\n\x1a\n").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "purpose={purpose:?} body={body}"
        );
    }
    assert!(
        !env.home.join("uploads").exists(),
        "被拒绝的上传不得建出目录"
    );
}
