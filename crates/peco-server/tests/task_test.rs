// ============================================================================
// Task 集成测试 — 定时任务 CRUD、Toggle、日志
// ============================================================================

mod common;

use common::TestApp;
use serde_json::json;

// ── 辅助：创建一个 Agent 并返回 agent_id ──────────────────────────────────────

async fn create_test_agent(app: &TestApp) -> String {
    let resp = app
        .post("/api/agents")
        .json(&json!({
            "name": "task-test-agent",
            "description": "for task testing",
            "system_prompt": "test"
        }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    body["id"].as_str().unwrap().to_string()
}

// ── 创建任务 ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_task() {
    let app = TestApp::new().await;
    let agent_id = create_test_agent(&app).await;

    let resp = app
        .post("/api/tasks")
        .json(&json!({
            "agent_id": agent_id,
            "name": "每日总结",
            "cron_expr": "0 9 * * 1-5",
            "prompt": "请总结今天的代码变更"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "每日总结");
    assert_eq!(body["cron_expr"], "0 9 * * 1-5");
    assert_eq!(body["enabled"], true); // bool, not integer
    assert!(body["id"].as_str().is_some());
}

#[tokio::test]
async fn test_create_task_invalid_cron() {
    let app = TestApp::new().await;
    let agent_id = create_test_agent(&app).await;

    let resp = app
        .post("/api/tasks")
        .json(&json!({
            "agent_id": agent_id,
            "name": "无效 cron",
            "cron_expr": "not-a-valid-cron",
            "prompt": "test"
        }))
        .send()
        .await
        .unwrap();

    // cron 表达式不合法 → 400
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_create_task_missing_agent() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/tasks")
        .json(&json!({
            "agent_id": "nonexistent-agent-id",
            "name": "没有 agent 的任务",
            "cron_expr": "0 9 * * *",
            "prompt": "test"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 404);
}

// ── 列表 ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_tasks() {
    let app = TestApp::new().await;
    let agent_id = create_test_agent(&app).await;

    // 创建 2 个任务
    for i in 1..=2 {
        let resp = app
            .post("/api/tasks")
            .json(&json!({
                "agent_id": agent_id,
                "name": format!("task-{}", i),
                "cron_expr": "0 9 * * 1-5",
                "prompt": format!("task {} prompt", i)
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
    }

    let resp = app.get("/api/tasks").send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 2);
}

// ── Toggle 启用/禁用 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_toggle_task() {
    let app = TestApp::new().await;
    let agent_id = create_test_agent(&app).await;

    // 创建
    let create_resp = app
        .post("/api/tasks")
        .json(&json!({
            "agent_id": agent_id,
            "name": "可切换任务",
            "cron_expr": "0 9 * * 1-5",
            "prompt": "test"
        }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let task_id = created["id"].as_str().unwrap();
    assert_eq!(created["enabled"], true);

    // Toggle → 禁用
    let resp = app
        .post(&format!("/api/tasks/{}/toggle", task_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["enabled"], false);

    // Toggle → 重新启用
    let resp = app
        .post(&format!("/api/tasks/{}/toggle", task_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["enabled"], true);
}

// ── 删除 ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_task() {
    let app = TestApp::new().await;
    let agent_id = create_test_agent(&app).await;

    // 创建
    let create_resp = app
        .post("/api/tasks")
        .json(&json!({
            "agent_id": agent_id,
            "name": "待删除任务",
            "cron_expr": "0 9 * * *",
            "prompt": "test"
        }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let task_id = created["id"].as_str().unwrap();

    // 删除
    let resp = app
        .delete(&format!("/api/tasks/{}", task_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 确认已删除
    let resp = app.get("/api/tasks").send().await.unwrap();
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    let exists = body.iter().any(|t| t["id"] == task_id);
    assert!(!exists);
}

// ── 更新 ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_update_task() {
    let app = TestApp::new().await;
    let agent_id = create_test_agent(&app).await;

    // 创建
    let create_resp = app
        .post("/api/tasks")
        .json(&json!({
            "agent_id": agent_id,
            "name": "原名称",
            "cron_expr": "0 9 * * *",
            "prompt": "原始 prompt"
        }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let task_id = created["id"].as_str().unwrap();

    // 更新
    let resp = app
        .patch(&format!("/api/tasks/{}", task_id))
        .json(&json!({
            "name": "新名称",
            "prompt": "新 prompt"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "新名称");
    assert_eq!(body["prompt"], "新 prompt");
}

// ── 执行日志 ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_task_logs_empty() {
    let app = TestApp::new().await;
    let agent_id = create_test_agent(&app).await;

    // 创建任务
    let create_resp = app
        .post("/api/tasks")
        .json(&json!({
            "agent_id": agent_id,
            "name": "无日志任务",
            "cron_expr": "0 9 * * *",
            "prompt": "test"
        }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let task_id = created["id"].as_str().unwrap();

    // 未执行的任务，日志列表为空
    let resp = app
        .get(&format!("/api/tasks/{}/logs", task_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}
