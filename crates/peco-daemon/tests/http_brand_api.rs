//! `/api/brand` —— 左栏品牌图片的读写与 URL 校验。

mod support;

use axum::http::Method;
use peco_daemon::db::get_server_config;
use serde_json::json;
use support::new_env;

async fn put(env: &support::TestEnv, image: &str) -> support::HttpReply {
    support::call(
        &env.router(),
        Method::PUT,
        "/api/brand",
        Some(json!({ "image": image })),
    )
    .await
}

/// 没写过就是「未设置」：空串而不是 404，左栏据此退回默认标记。
#[tokio::test]
async fn unset_brand_reads_as_empty_image() {
    let env = new_env(&[]).await;

    let reply = env.get("/api/brand").await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body, json!({ "image": "" }));
}

/// 合法 URL 落进 `server_config`，回读与 GET 都拿到它。
#[tokio::test]
async fn put_persists_uploaded_brand_image() {
    let env = new_env(&[]).await;
    let url = "/uploads/brand/7f3c.png";

    let reply = put(&env, url).await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body, json!({ "image": url }));
    let stored = get_server_config(&env.pool, "brand").await.unwrap();
    assert_eq!(
        stored
            .as_deref()
            .map(|raw| serde_json::from_str::<serde_json::Value>(raw).unwrap()),
        Some(json!({ "image": url })),
        "值必须是 {{\"image\": …}} 的 JSON"
    );
    assert_eq!(env.get("/api/brand").await.body, json!({ "image": url }));
}

/// 空串 = 恢复默认：写进去也读得回来，不算错。
#[tokio::test]
async fn put_empty_image_clears_the_brand() {
    let env = new_env(&[]).await;
    assert_eq!(put(&env, "/uploads/brand/a.png").await.status, 200);

    let reply = put(&env, "").await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(env.get("/api/brand").await.body, json!({ "image": "" }));
}

/// 只认品牌图目录：团队图标的 URL 不能拿来当品牌图。
#[tokio::test]
async fn put_rejects_urls_outside_the_brand_directory() {
    let env = new_env(&[]).await;

    for bad in [
        "/uploads/teams/x.png",
        "/uploads/brand",
        "https://example.com/x.png",
        "/uploads/brand/../../teams/x.png",
        "/uploads/brand/a\tb.png",
    ] {
        let reply = put(&env, bad).await;
        assert_eq!(reply.status, 400, "应拒绝 {bad:?}，body={}", reply.body);
    }
    assert_eq!(
        get_server_config(&env.pool, "brand").await.unwrap(),
        None,
        "被拒绝的请求不得留下任何一行"
    );
}

/// 超长 URL 拒绝：`server_config` 不该被塞进整段 data URI。
#[tokio::test]
async fn put_rejects_overlong_urls() {
    let env = new_env(&[]).await;
    let long = format!("/uploads/brand/{}.png", "a".repeat(1024));

    let reply = put(&env, &long).await;

    assert_eq!(reply.status, 400, "body={}", reply.body);
}
