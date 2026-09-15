// ============================================================================
// Workflow 调度 cron 表达式集成测试
// ============================================================================
//
// 覆盖：
//   - 5 字段表达式（前端预设的默认形态）能真正注册进调度器 —— 回归本次修复的
//     静默失败：croner 校验放行 5 字段，而 Job::new_async 要求 6 字段，
//     注册错误被吞掉后用户配置的定时 workflow 永不触发
//   - 6 字段表达式同样可用（校验放宽后不再被 croner 默认档位拒绝）
//   - 明显非法的表达式仍返回 400（放宽字段数不等于放弃校验）

mod common;

use common::TestApp;

const WORKFLOW_NAME: &str = "sched-cron-normalize";
const WORKFLOWS_PATH: &str = "/api/workflows";
const SCHEDULES_PATH: &str = "/api/schedules";

/// 最小可用 Workflow 定义（单 shell 步骤，不做实际动作）。
fn workflow_yaml() -> String {
    format!(
        r#"---
workflow:
  name: "{WORKFLOW_NAME}"
  description: "cron 归一化集成验证"
  version: "1.0"
  steps:
    - id: "noop"
      name: "占位"
      type: shell
      config:
        command: "true"
"#
    )
}

/// 先建一个 Workflow，返回 TestApp。
async fn app_with_workflow() -> TestApp {
    let app = TestApp::new().await;
    let resp = app
        .post(WORKFLOWS_PATH)
        .json(&serde_json::json!({ "yaml": workflow_yaml() }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "前置 Workflow 创建失败: {:?}",
        resp.text().await
    );
    app
}

// ── 注册路径 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn five_field_cron_registers_scheduled_workflow() {
    let app = app_with_workflow().await;

    let resp = app
        .post(SCHEDULES_PATH)
        .json(&serde_json::json!({
            "workflow_name": WORKFLOW_NAME,
            "cron": "0 9 * * *",
            "enabled": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "5 字段 cron 应被接受: {:?}",
        resp.text().await
    );

    assert!(
        app.state
            .cron_scheduler
            .contains_workflow(WORKFLOW_NAME, &app.user_id)
            .await,
        "5 字段 cron 必须真正注册进调度器（补秒域后构造 job）"
    );
}

#[tokio::test]
async fn six_field_cron_registers_scheduled_workflow() {
    let app = app_with_workflow().await;

    let resp = app
        .post(SCHEDULES_PATH)
        .json(&serde_json::json!({
            "workflow_name": WORKFLOW_NAME,
            "cron": "0 0 9 * * *",
            "enabled": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "带秒的 6 字段 cron 应被接受: {:?}",
        resp.text().await
    );
    assert!(
        app.state
            .cron_scheduler
            .contains_workflow(WORKFLOW_NAME, &app.user_id)
            .await,
        "6 字段 cron 原样注册"
    );
}

#[tokio::test]
async fn invalid_cron_is_rejected() {
    let app = app_with_workflow().await;

    let resp = app
        .post(SCHEDULES_PATH)
        .json(&serde_json::json!({
            "workflow_name": WORKFLOW_NAME,
            "cron": "not a cron",
            "enabled": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "非法表达式仍应 400");
    assert!(
        !app.state
            .cron_scheduler
            .contains_workflow(WORKFLOW_NAME, &app.user_id)
            .await,
        "被拒的调度不得进入调度器"
    );
}
