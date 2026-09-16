// ============================================================================
// Provider 集成测试 — 保存语义、默认 provider、真实连接测试
// ============================================================================
//
// 连接测试用本地假上游验证：探针确实发出了 HTTP 请求、把上游错误状态
// 原样带回给调用方。这覆盖了"永远返回 success: true 的假桩"这一历史缺陷。

mod common;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use common::TestApp;
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpListener;

// ── 假上游 ────────────────────────────────────────────────────────────────────

/// 假上游的行为：按配置返回 200 正常响应或指定状态码的错误。
#[derive(Clone)]
struct FakeUpstream {
    /// `None` = 返回正常 chat completion；`Some((status, body))` = 返回该错误。
    failure: Option<(u16, String)>,
}

impl FakeUpstream {
    fn ok() -> Self {
        Self { failure: None }
    }

    fn failing(status: u16, body: &str) -> Self {
        Self {
            failure: Some((status, body.to_string())),
        }
    }
}

/// 启动假上游，返回 (base_url, 已收到的请求数句柄)。
async fn start_fake_upstream(
    upstream: FakeUpstream,
) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    async fn handler(
        State((upstream, hits)): State<(FakeUpstream, Arc<std::sync::atomic::AtomicUsize>)>,
    ) -> impl IntoResponse {
        hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match upstream.failure {
            Some((status, body)) => (
                StatusCode::from_u16(status).unwrap(),
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response(),
            None => (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                json!({
                    "id": "probe-1",
                    "choices": [{
                        "message": {"role": "assistant", "content": "pong"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}
                })
                .to_string(),
            )
                .into_response(),
        }
    }

    let app = Router::new()
        .route("/chat/completions", post(handler))
        .with_state((upstream, hits.clone()));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    (format!("http://{addr}"), hits)
}

// ── 保存语义 ──────────────────────────────────────────────────────────────────

/// 保存后再保存（不带 api_key）不得清空已存凭据 —— 前端无法回读 key，
/// 全量替换语义会把用户的密钥抹掉。
#[tokio::test]
async fn test_upsert_keeps_api_key_when_omitted() {
    let app = TestApp::new().await;

    let resp = app
        .put("/api/providers")
        .json(&json!({
            "type": "deepseek",
            "api_key": "sk-first",
            "default_model": "deepseek-v4-flash"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 第二次只改 URL，不带 key / model
    let resp = app
        .put("/api/providers")
        .json(&json!({
            "type": "deepseek",
            "base_url": "https://proxy.internal/v1"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = app
        .get("/api/providers/deepseek")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["has_api_key"], true, "api_key 被清空了: {body}");
    assert_eq!(body["base_url"], "https://proxy.internal/v1");
    assert_eq!(body["default_model"], "deepseek-v4-flash");
    assert_eq!(body["is_default"], true);
}

/// 新增 provider 后列表能读到类型、模型与默认位标记。
#[tokio::test]
async fn test_list_providers_reports_fields() {
    let app = TestApp::new().await;

    for (ty, model) in [("deepseek", "deepseek-v4-flash"), ("openai", "gpt-5.2")] {
        let resp = app
            .put("/api/providers")
            .json(&json!({
                "type": ty,
                "api_key": format!("sk-{ty}"),
                "default_model": model
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    let list: Vec<serde_json::Value> = app
        .get("/api/providers")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(list.len(), 2);
    let openai = list.iter().find(|p| p["name"] == "openai").unwrap();
    assert_eq!(openai["default_model"], "gpt-5.2");
    assert_eq!(openai["has_api_key"], true);
    // 第一个写入的 provider 接管默认位，第二个不抢
    let deepseek = list.iter().find(|p| p["name"] == "deepseek").unwrap();
    assert_eq!(deepseek["is_default"], true);
    assert_eq!(openai["is_default"], false);
    // 凭据不回读
    assert!(openai.get("api_key").is_none());
}

/// `set_default` 显式接管默认位。
#[tokio::test]
async fn test_upsert_set_default() {
    let app = TestApp::new().await;

    app.put("/api/providers")
        .json(&json!({"type": "deepseek", "api_key": "sk-d"}))
        .send()
        .await
        .unwrap();
    app.put("/api/providers")
        .json(&json!({"type": "openai", "api_key": "sk-o", "set_default": true}))
        .send()
        .await
        .unwrap();

    let body: serde_json::Value = app
        .get("/api/providers/openai")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["is_default"], true);

    let ds: serde_json::Value = app
        .get("/api/providers/deepseek")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ds["is_default"], false);
}

/// 别名 name 真正生效（不再是"用 type 当 name、表单 name 字段被忽略"）。
#[tokio::test]
async fn test_upsert_honors_alias_name() {
    let app = TestApp::new().await;

    let resp = app
        .put("/api/providers")
        .json(&json!({
            "name": "gateway",
            "type": "openai",
            "api_key": "sk-gateway",
            "base_url": "http://127.0.0.1:9999/v1"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = app
        .get("/api/providers/gateway")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["name"], "gateway");
    assert_eq!(body["provider_type"], "openai");

    // 原类型名没有被误建
    assert_eq!(
        app.get("/api/providers/openai")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

/// 改写已有条目的类型时不继承旧类型的凭据：宁可让用户补一次新密钥，
/// 也不能把 A 家的 key 发到 B 家的地址上。
#[tokio::test]
async fn test_type_change_drops_old_credentials() {
    let app = TestApp::new().await;

    app.put("/api/providers")
        .json(&json!({
            "name": "gateway",
            "type": "openai",
            "api_key": "sk-openai",
            "default_model": "gpt-5.2",
            "set_default": true
        }))
        .send()
        .await
        .unwrap();

    // 只改类型与地址，不带 api_key（前端也拿不到旧值）
    let resp = app
        .put("/api/providers")
        .json(&json!({
            "name": "gateway",
            "type": "deepseek",
            "base_url": "https://api.deepseek.com",
            "default_model": "deepseek-v4-flash"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = app
        .get("/api/providers/gateway")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["provider_type"], "deepseek");
    assert_eq!(body["default_model"], "deepseek-v4-flash");
    assert_eq!(body["has_api_key"], false, "旧类型的密钥被继承了: {body}");
}

/// 类型名大小写变体归一化为目录里的规范值 —— 落盘的必须是适配器认得的写法，
/// 否则该 provider 要到构建 Agent 时才会报 unsupported。
#[tokio::test]
async fn test_upsert_normalizes_provider_type() {
    let app = TestApp::new().await;

    let resp = app
        .put("/api/providers")
        .json(&json!({"type": "OpenAI", "api_key": "sk-x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = app
        .get("/api/providers/openai")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["name"], "openai");
    assert_eq!(body["provider_type"], "openai");
}

/// 该类型不支持的 api 档位在写盘前被拒（分派器只认各类型的合法档位）。
#[tokio::test]
async fn test_upsert_rejects_unsupported_api_mode() {
    let app = TestApp::new().await;

    let resp = app
        .put("/api/providers")
        .json(&json!({"type": "qwen", "api_key": "sk-x", "api": "completions"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let list: Vec<serde_json::Value> = app
        .get("/api/providers")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.is_empty());
}

/// 未支持的类型 → 400，且不落盘。
#[tokio::test]
async fn test_upsert_rejects_unknown_type() {
    let app = TestApp::new().await;

    let resp = app
        .put("/api/providers")
        .json(&json!({"type": "anthropic", "api_key": "sk-x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let list: Vec<serde_json::Value> = app
        .get("/api/providers")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.is_empty());
}

/// 删除默认 provider → 默认位回落到剩余条目（不悬空）。
#[tokio::test]
async fn test_delete_reassigns_default() {
    let app = TestApp::new().await;

    app.put("/api/providers")
        .json(&json!({"type": "deepseek", "api_key": "sk-d"}))
        .send()
        .await
        .unwrap();
    app.put("/api/providers")
        .json(&json!({"type": "openai", "api_key": "sk-o"}))
        .send()
        .await
        .unwrap();

    let resp = app.delete("/api/providers/deepseek").send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let openai: serde_json::Value = app
        .get("/api/providers/openai")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(openai["is_default"], true);
}

/// 删除不存在的 provider → 404。
#[tokio::test]
async fn test_delete_missing_provider() {
    let app = TestApp::new().await;
    let resp = app.delete("/api/providers/nope").send().await.unwrap();
    assert_eq!(resp.status(), 404);
}

// ── 类型目录 ──────────────────────────────────────────────────────────────────

/// 类型目录给出固定 URL 与默认模型，前端据此预填表单。
#[tokio::test]
async fn test_list_provider_types() {
    let app = TestApp::new().await;

    let types: Vec<serde_json::Value> = app
        .get("/api/providers/types")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let deepseek = types
        .iter()
        .find(|t| t["provider_type"] == "deepseek")
        .unwrap();
    assert_eq!(deepseek["default_base_url"], "https://api.deepseek.com");
    assert!(
        deepseek["suggested_models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "deepseek-v4-flash")
    );

    let openai = types
        .iter()
        .find(|t| t["provider_type"] == "openai")
        .unwrap();
    assert_eq!(openai["default_base_url"], "https://api.openai.com/v1");

    let qwen = types.iter().find(|t| t["provider_type"] == "qwen").unwrap();
    assert_eq!(
        qwen["default_base_url"],
        "https://dashscope.aliyuncs.com/compatible-mode/v1"
    );
}

// ── 连接测试 ──────────────────────────────────────────────────────────────────

/// 保存前测试：表单值直连假上游，成功路径。
#[tokio::test]
async fn test_draft_connection_success() {
    let app = TestApp::new().await;
    let (base_url, hits) = start_fake_upstream(FakeUpstream::ok()).await;

    let resp = app
        .post("/api/providers/test")
        .json(&json!({
            "type": "openai",
            "api": "chat",
            "api_key": "sk-test",
            "base_url": base_url,
            "default_model": "gpt-5.2"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], true, "{body}");
    assert_eq!(body["model"], "gpt-5.2");
    // 关键：真的发了请求（历史实现从不发请求）
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// 保存前测试：上游 401 → success=false，且错误带状态码与错误体。
#[tokio::test]
async fn test_draft_connection_reports_upstream_error() {
    let app = TestApp::new().await;
    let (base_url, _hits) = start_fake_upstream(FakeUpstream::failing(
        401,
        r#"{"error":{"message":"invalid api key"}}"#,
    ))
    .await;

    let resp = app
        .post("/api/providers/test")
        .json(&json!({
            "type": "openai",
            "api": "chat",
            "api_key": "sk-wrong",
            "base_url": base_url,
            "default_model": "gpt-5.2"
        }))
        .send()
        .await
        .unwrap();

    // 连接失败是本接口的正常结果，HTTP 仍是 200
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], false);
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("401"), "{message}");
    assert!(message.contains("invalid api key"), "{message}");
}

/// 保存前测试：不填模型 → 明确提示，不发请求。
#[tokio::test]
async fn test_draft_connection_requires_model() {
    let app = TestApp::new().await;
    let (base_url, hits) = start_fake_upstream(FakeUpstream::ok()).await;

    let resp = app
        .post("/api/providers/test")
        .json(&json!({
            "type": "openai",
            "api": "chat",
            "api_key": "sk-test",
            "base_url": base_url
        }))
        .send()
        .await
        .unwrap();

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], false);
    assert!(
        body["message"].as_str().unwrap().contains("未指定模型名"),
        "{body}"
    );
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// 已保存 provider 的测试走落盘配置（含 base_url 覆盖）。
#[tokio::test]
async fn test_saved_connection_uses_stored_config() {
    let app = TestApp::new().await;
    let (base_url, hits) = start_fake_upstream(FakeUpstream::ok()).await;

    app.put("/api/providers")
        .json(&json!({
            "type": "openai",
            "api": "chat",
            "api_key": "sk-saved",
            "base_url": base_url,
            "default_model": "gpt-5.2"
        }))
        .send()
        .await
        .unwrap();

    let resp = app.post("/api/providers/openai/test").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], true, "{body}");
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// 未配置的 provider 名 → 404（而不是"测试通过"）。
#[tokio::test]
async fn test_saved_connection_unknown_provider() {
    let app = TestApp::new().await;
    let resp = app.post("/api/providers/nope/test").send().await.unwrap();
    assert_eq!(resp.status(), 404);
}
