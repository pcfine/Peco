// ============================================================================
// Agent 集成测试 — CRUD + 权限隔离 + 缓存失效 + agent.md 真相源
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
    // model 现在是 Option<String>，创建时指定了值
    assert_eq!(body["model"], "deepseek-v4-flash");
    assert_eq!(body["icon"], "🔍");
    assert!(body["id"].as_str().is_some());
    // 验证 tools 被正确存储
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
    // list 不含 model/tools/system_prompt（轻量索引）
    let first = &body[0];
    assert!(first.get("model").is_none());
    assert!(first.get("tools").is_none());
}

#[tokio::test]
async fn test_list_agents_empty() {
    let app = TestApp::new().await;

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

    // 获取（数据来自 agent.md 文件）
    let resp = app
        .get(&format!("/api/agents/{}", agent_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "details-test");
    assert_eq!(body["system_prompt"], "你是专家");
    // 创建时未指定 model，agent.md 中为空字符串/不指定
    // parse 后为 null
}

#[tokio::test]
async fn test_get_agent_not_found() {
    let app = TestApp::new().await;

    let resp = app.get("/api/agents/nonexistent-id").send().await.unwrap();

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

// ── agent.md 真相源验证 ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_agent_detail_from_file_not_db() {
    let app = TestApp::new().await;

    // 创建 agent
    let create_resp = app
        .post("/api/agents")
        .json(&json!({
            "name": "file-truth-test",
            "description": "原始描述",
            "system_prompt": "原始 system prompt",
            "model": "deepseek-v4-flash",
            "temperature": 0.7
        }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let agent_id = created["id"].as_str().unwrap();

    // 直接修改 agent.md 文件内容（模拟手动编辑）
    let agent_dir = app
        .state
        .data_dir
        .join("workspaces")
        .join(&app.user_id)
        .join("agents")
        .join("file-truth-test");
    let agent_md_path = agent_dir.join("agent.md");
    let new_content = "---\nagent:\n  name: \"file-truth-test\"\n  \
        description: \"手动修改后的描述\"\nllm:\n  provider: \"deepseek\"\n  \
        model: \"deepseek-v4-pro\"\n  temperature: 0.3\n---\n手动修改后的 system prompt";
    std::fs::write(&agent_md_path, new_content).unwrap();

    // GET 详情 → 应返回文件中的新内容，而非 DB 旧值
    let resp = app
        .get(&format!("/api/agents/{}", agent_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["description"], "手动修改后的描述");
    assert_eq!(body["model"], "deepseek-v4-pro");
    assert_eq!(body["system_prompt"], "手动修改后的 system prompt");
    assert_eq!(body["temperature"], 0.3);
}

#[tokio::test]
async fn test_create_agent_with_all_new_fields() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/agents")
        .json(&json!({
            "name": "full-config-test",
            "description": "完整配置测试",
            "system_prompt": "You are a test assistant.",
            "model": "deepseek-v4-pro",
            "provider": "deepseek",
            "temperature": 0.5,
            "max_tokens": 8192,
            "stream": true,
            "reasoning_effort": "high",
            "tools": ["shell", "fetch"],
            "mcp_servers": ["filesystem"],
            "skills": ["code-review"],
            "max_turns": 30,
            "icon": "🧪",
            "color": "#ff6600"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();

    // 验证所有字段 round-trip
    assert_eq!(body["name"], "full-config-test");
    assert_eq!(body["model"], "deepseek-v4-pro");
    assert_eq!(body["provider"], "deepseek");
    assert_eq!(body["temperature"], 0.5);
    assert_eq!(body["max_tokens"], 8192);
    assert_eq!(body["stream"], true);
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["max_turns"], 30);
    assert_eq!(body["icon"], "🧪");
    assert_eq!(body["color"], "#ff6600");

    let tools: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(tools.contains(&"shell"));
    assert!(tools.contains(&"fetch"));

    let mcp: Vec<&str> = body["mcp_servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(mcp.contains(&"filesystem"));

    let skills: Vec<&str> = body["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(skills.contains(&"code-review"));
}

#[tokio::test]
async fn test_update_partial_fields_preserves_others() {
    let app = TestApp::new().await;

    // 创建完整配置的 agent
    let create_resp = app
        .post("/api/agents")
        .json(&json!({
            "name": "partial-update-test",
            "description": "原始",
            "system_prompt": "原始 prompt",
            "model": "deepseek-v4-flash",
            "temperature": 0.7,
            "max_turns": 25,
            "tools": ["shell"]
        }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let agent_id = created["id"].as_str().unwrap();

    // 只更新 description
    let resp = app
        .patch(&format!("/api/agents/{}", agent_id))
        .json(&json!({
            "description": "更新后的描述"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["description"], "更新后的描述");
    // 未更新的字段保持不变
    assert_eq!(body["model"], "deepseek-v4-flash");
    assert_eq!(body["temperature"], 0.7);
    assert_eq!(body["max_turns"], 25);
    let tools: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(tools.contains(&"shell"));
}
