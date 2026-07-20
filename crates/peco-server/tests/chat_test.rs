// ============================================================================
// Chat 集成测试 — 对话 CRUD、消息历史、SSE 流式
// ============================================================================

mod common;

use common::TestApp;
use serde_json::json;

// ── 创建对话 ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_conversation() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/conversations")
        .json(&json!({
            "title": "测试对话"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["title"], "测试对话");
    assert!(body["id"].as_str().is_some());
    // 自动分配的全能助手 agent_id
    assert!(body["agent_id"].as_str().is_some());
    assert!(body["agent_name"].as_str().is_some());
}

#[tokio::test]
async fn test_create_conversation_default_title() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/conversations")
        .json(&json!({}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    // 空 title 时默认为 "新对话"
    assert!(!body["title"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn test_create_conversation_with_specific_agent() {
    let app = TestApp::new().await;

    // 先创建一个专业 Agent
    let agent_resp = app
        .post("/api/agents")
        .json(&json!({
            "name": "专用助手",
            "description": "专用",
            "system_prompt": "你是专用助手"
        }))
        .send()
        .await
        .unwrap();
    let agent: serde_json::Value = agent_resp.json().await.unwrap();
    let agent_id = agent["id"].as_str().unwrap();

    // 创建绑定该 Agent 的对话
    let resp = app
        .post("/api/conversations")
        .json(&json!({
            "title": "专用对话",
            "agent_id": agent_id
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["agent_name"], "专用助手");
}

// ── 对话列表 ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_conversations() {
    let app = TestApp::new().await;

    // 创建 3 个对话
    for i in 1..=3 {
        let resp = app
            .post("/api/conversations")
            .json(&json!({ "title": format!("对话-{}", i) }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
    }

    let resp = app.get("/api/conversations").send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 3);
}

// ── 获取消息 ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_get_messages_empty() {
    let app = TestApp::new().await;

    // 创建空白对话
    let conv_resp = app
        .post("/api/conversations")
        .json(&json!({ "title": "空对话" }))
        .send()
        .await
        .unwrap();
    let conv: serde_json::Value = conv_resp.json().await.unwrap();
    let conv_id = conv["id"].as_str().unwrap();

    // 获取消息列表（应为空）
    let resp = app
        .get(&format!("/api/conversations/{}/messages", conv_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

// ── 删除对话 ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_conversation() {
    let app = TestApp::new().await;

    // 创建
    let conv_resp = app
        .post("/api/conversations")
        .json(&json!({ "title": "待删除" }))
        .send()
        .await
        .unwrap();
    let conv: serde_json::Value = conv_resp.json().await.unwrap();
    let conv_id = conv["id"].as_str().unwrap();

    // 删除
    let resp = app
        .delete(&format!("/api/conversations/{}", conv_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 确认已删除（列表为空）
    let resp = app.get("/api/conversations").send().await.unwrap();
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    let exists = body.iter().any(|c| c["id"] == conv_id);
    assert!(!exists);
}

// ── 认证要求 ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_conversation_endpoints_require_auth() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .get(format!("{}/api/conversations", app.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

// ── SSE 流式对话（需要 LLM API Key，默认 skip） ───────────────────────────────

#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY in environment"]
async fn test_sse_stream_basic() {
    let app = TestApp::new().await;

    // 创建对话
    let conv_resp = app
        .post("/api/conversations")
        .json(&json!({ "title": "SSE 测试" }))
        .send()
        .await
        .unwrap();
    let conv: serde_json::Value = conv_resp.json().await.unwrap();
    let conv_id = conv["id"].as_str().unwrap();

    // SSE 流式请求
    let resp = app
        .client
        .get(format!(
            "{}/api/conversations/{}/stream?message=你好",
            app.base_url, conv_id
        ))
        .bearer_auth(&app.user_token)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().contains("text/event-stream"))
            .unwrap_or(false),
        "expected text/event-stream content type"
    );

    // 收集 SSE 事件文本
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("event:") || !body.is_empty(),
        "SSE stream should contain events"
    );
}

#[tokio::test]
async fn test_sse_stream_requires_auth() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .get(format!(
            "{}/api/conversations/some-id/stream?message=hi",
            app.base_url
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}
