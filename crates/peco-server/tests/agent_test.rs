// ============================================================================
// Agent 集成测试 — CRUD + 权限隔离 + 缓存失效
// ============================================================================

mod common;

use common::TestApp;
use serde_json::json;

// ── 创建 Agent ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_agent() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/agents")
        .json(&json!({
            "name": "代码审查员",
            "description": "负责代码质量审查",
            "system_prompt": "你是一位资深代码审查专家",
            "model": "deepseek-v4-flash",
            "tools": ["shell_exec", "fetch"],
            "icon": "🔍",
            "color": "#22c55e"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "代码审查员");
    assert_eq!(body["description"], "负责代码质量审查");
    assert_eq!(body["model"], "deepseek-v4-flash");
    assert_eq!(body["icon"], "🔍");
    assert!(body["id"].as_str().is_some());
    // 验证 tools 在 config_json 中被正确存储
    let tools = body["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t == "shell_exec"));
    assert!(tools.iter().any(|t| t == "fetch"));
}

#[tokio::test]
async fn test_create_agent_empty_name() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/agents")
        .json(&json!({
            "name": "",
            "description": "test",
            "system_prompt": "test"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_create_agent_duplicate_name() {
    let app = TestApp::new().await;

    // 第一次：成功
    let resp = app
        .post("/api/agents")
        .json(&json!({
            "name": "duplicate-test",
            "description": "test",
            "system_prompt": "test"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // 第二次：同名冲突
    let resp = app
        .post("/api/agents")
        .json(&json!({
            "name": "duplicate-test",
            "description": "test2",
            "system_prompt": "test2"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 409);
}

// ── 列表 ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_agents() {
    let app = TestApp::new().await;

    // 创建 2 个 agent
    for i in 1..=2 {
        let resp = app
            .post("/api/agents")
            .json(&json!({
                "name": format!("agent-{}", i),
                "description": "test",
                "system_prompt": "test"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
    }

    let resp = app.get("/api/agents").send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.len() >= 2);
}

#[tokio::test]
async fn test_list_agents_empty() {
    let app = TestApp::new().await;

    // 不创建任何 agent，列表应为空数组
    let resp = app.get("/api/agents").send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

// ── 获取详情 ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_get_agent() {
    let app = TestApp::new().await;

    // 创建
    let create_resp = app
        .post("/api/agents")
        .json(&json!({
            "name": "details-test",
            "description": "查看详情",
            "system_prompt": "你是专家"
        }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let agent_id = created["id"].as_str().unwrap();

    // 获取
    let resp = app
        .get(&format!("/api/agents/{}", agent_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "details-test");
    assert_eq!(body["system_prompt"], "你是专家");
}

#[tokio::test]
async fn test_get_agent_not_found() {
    let app = TestApp::new().await;

    let resp = app
        .get("/api/agents/nonexistent-id")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 404);
}

// ── 更新 ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_update_agent() {
    let app = TestApp::new().await;

    // 创建
    let create_resp = app
        .post("/api/agents")
        .json(&json!({
            "name": "update-test",
            "description": "原始描述",
            "system_prompt": "原始 prompt"
        }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let agent_id = created["id"].as_str().unwrap();

    // 更新
    let resp = app
        .patch(&format!("/api/agents/{}", agent_id))
        .json(&json!({
            "description": "更新后的描述",
            "system_prompt": "更新后的 prompt"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["description"], "更新后的描述");
    assert_eq!(body["system_prompt"], "更新后的 prompt");
    // 未更新的字段保持不变
    assert_eq!(body["name"], "update-test");
}

// ── 删除 ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_agent() {
    let app = TestApp::new().await;

    // 创建
    let create_resp = app
        .post("/api/agents")
        .json(&json!({
            "name": "delete-test",
            "description": "将要被删除",
            "system_prompt": "test"
        }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let agent_id = created["id"].as_str().unwrap();

    // 删除
    let resp = app
        .delete(&format!("/api/agents/{}", agent_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 确认已删除
    let resp = app
        .get(&format!("/api/agents/{}", agent_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ── 权限隔离 ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_cross_user_isolation() {
    let app = TestApp::new().await;

    // user1 创建 agent
    let create_resp = app
        .post("/api/agents")
        .json(&json!({
            "name": "private-agent",
            "description": "私有 agent",
            "system_prompt": "私有"
        }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let agent_id = created["id"].as_str().unwrap();

    // user2 登录
    let (_, user2_token) = app.register_user2().await;

    // user2 尝试获取 user1 的 agent → 404
    let resp = app
        .client
        .get(format!("{}/api/agents/{}", app.base_url, agent_id))
        .bearer_auth(&user2_token)
        .send()
        .await
        .unwrap();
    // NOT found for user2 (permission isolation)
    assert!(
        resp.status() == 404 || resp.status() == 403,
        "expected 404 or 403 for cross-user access, got {}",
        resp.status()
    );

    // user2 的列表不包含 user1 的 agent
    let resp = app
        .client
        .get(format!("{}/api/agents", app.base_url))
        .bearer_auth(&user2_token)
        .send()
        .await
        .unwrap();
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    let has_private = body.iter().any(|a| a["id"] == agent_id);
    assert!(!has_private, "user2 should not see user1's private agent");
}

// ── 非认证访问 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_agent_endpoints_require_auth() {
    let app = TestApp::new().await;

    for path in ["/api/agents", "/api/agents/some-id"] {
        let resp = app
            .client
            .get(format!("{}{}", app.base_url, path))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401, "{} should require auth", path);
    }
}
