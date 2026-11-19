//! `/api/schedules` 的七个端点与四类错误码。
//!
//! 只走真实路由（`tower::ServiceExt::oneshot`），不直接调领域函数：路由注册、
//! 提取器、错误体形状都是契约的一部分。

mod support;

use axum::http::StatusCode;
use peco_daemon::db::schedules::{self, NewRun};
use peco_daemon::schedule::{RunStatus, TriggerKind};
use serde_json::{Value, json};
use support::{HttpReply, TestEnv, new_env};

/// 建一个已索引但**未打开**的空间：闹钟的目标空间通常是关着的。
async fn indexed_space(env: &TestEnv, name: &str, team: &str) -> String {
    let dir = env.work_dir(name);
    env.resolve(&dir, team).await.expect("空间应能被索引")
}

fn schedule_body(ws_id: &str) -> Value {
    json!({
        "name": "每日站会",
        "target_ws_id": ws_id,
        "prompt": "汇报昨天的进展",
        "cron_expr": "0 9 * * *"
    })
}

async fn create(env: &TestEnv, ws_id: &str) -> HttpReply {
    env.post("/api/schedules", schedule_body(ws_id)).await
}

async fn schedule_id(env: &TestEnv, ws_id: &str) -> String {
    let reply = create(env, ws_id).await;
    assert_eq!(
        reply.status,
        StatusCode::CREATED,
        "建排期失败: {}",
        reply.body
    );
    reply.body["id"].as_str().expect("必须返回 id").to_string()
}

// ============================================================================
// 列表 / 创建
// ============================================================================

/// 列表是裸数组（与 `/api/spaces` 一致），空库时是 `[]` 而不是 `null` 或包一层的对象。
#[tokio::test]
async fn the_list_is_a_bare_array() {
    let env = new_env(&["research"]).await;

    let reply = env.get("/api/schedules").await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, json!([]));
}

#[tokio::test]
async fn creating_a_schedule_returns_its_view_and_lists_it_newest_first() {
    let env = new_env(&["research"]).await;
    let ws_id = indexed_space(&env, "alpha", "research").await;

    let created = create(&env, &ws_id).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["name"], json!("每日站会"));
    assert_eq!(created.body["target_ws_id"], json!(ws_id));
    assert_eq!(
        created.body["target_label"],
        json!("research"),
        "label 为空回退 team_id"
    );
    assert_eq!(created.body["enabled"], json!(true));
    assert_eq!(created.body["last_status"], Value::Null);
    let first_next = created.body["next_run_at"]
        .as_str()
        .expect("启用的调度必须算出下一次")
        .to_string();

    let second = env
        .post(
            "/api/schedules",
            json!({
                "name": "整点巡检",
                "target_ws_id": ws_id,
                "prompt": "看看有没有异常",
                "cron_expr": "0 * * * *",
                "enabled": false,
            }),
        )
        .await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.body);
    assert_eq!(
        second.body["next_run_at"],
        Value::Null,
        "停用的调度没有下一次"
    );

    let list = env.get("/api/schedules").await;
    assert_eq!(list.status, StatusCode::OK);
    let rows = list.body.as_array().expect("裸数组");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], json!("整点巡检"), "新→旧");
    assert_eq!(rows[1]["next_run_at"], json!(first_next));
}

/// `cron_expr` 落用户原文：前端预设按 5 字段匹配，补过秒的形状既命不中预设，
/// 编辑时也不是用户写的那串。归一化只发生在解析侧。
#[tokio::test]
async fn the_cron_expr_is_stored_verbatim() {
    let env = new_env(&["research"]).await;
    let ws_id = indexed_space(&env, "alpha", "research").await;

    let created = create(&env, &ws_id).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(
        created.body["cron_expr"],
        json!("0 9 * * *"),
        "创建回执必须逐字返回用户原文"
    );

    let list = env.get("/api/schedules").await;
    let row = &list.body[0];
    assert_eq!(
        row["cron_expr"],
        json!("0 9 * * *"),
        "库里的也是原文: {row}"
    );
    assert!(
        row["next_run_at"].as_str().is_some(),
        "校验通过就必须排出下一次: {row}"
    );
}

/// 6 字段本来就合法：落库与回读都不得被二次加工。
#[tokio::test]
async fn a_six_field_cron_is_stored_untouched() {
    let env = new_env(&["research"]).await;
    let ws_id = indexed_space(&env, "alpha", "research").await;

    let created = env
        .post(
            "/api/schedules",
            json!({
                "name": "每日九点",
                "target_ws_id": ws_id,
                "prompt": "p",
                "cron_expr": "0 0 9 * * *"
            }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["cron_expr"], json!("0 0 9 * * *"));

    let list = env.get("/api/schedules").await;
    assert_eq!(list.body[0]["cron_expr"], json!("0 0 9 * * *"));
}

// ============================================================================
// 三类错误码
// ============================================================================

/// `invalid_cron` 的 details 必须是具体原因：前端只能展示它。
#[tokio::test]
async fn an_unparsable_cron_is_rejected_with_a_reason() {
    let env = new_env(&["research"]).await;
    let ws_id = indexed_space(&env, "alpha", "research").await;

    let reply = env
        .post(
            "/api/schedules",
            json!({
                "name": "坏表达式",
                "target_ws_id": ws_id,
                "prompt": "p",
                "cron_expr": "0 0 99 * * *"
            }),
        )
        .await;

    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.code(), "invalid_cron");
    assert!(
        reply.body["details"]
            .as_str()
            .is_some_and(|d| !d.is_empty()),
        "必须带具体原因: {}",
        reply.body
    );
    assert_eq!(
        env.get("/api/schedules").await.body,
        json!([]),
        "拒绝时不得落库"
    );
}

#[tokio::test]
async fn an_empty_name_is_an_invalid_schedule() {
    let env = new_env(&["research"]).await;
    let ws_id = indexed_space(&env, "alpha", "research").await;

    let reply = env
        .post(
            "/api/schedules",
            json!({
                "name": "   ",
                "target_ws_id": ws_id,
                "prompt": "p",
                "cron_expr": "0 9 * * *"
            }),
        )
        .await;

    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.code(), "invalid_schedule");
}

#[tokio::test]
async fn an_unknown_target_space_is_a_404() {
    let env = new_env(&["research"]).await;

    let reply = create(&env, "ws_ghost").await;

    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.code(), "space_not_found");
}

// ============================================================================
// PATCH
// ============================================================================

/// 改 cron 要真的重算 `next_run_at`；只改名字不得把已经排好的槽位往后推。
#[tokio::test]
async fn patching_the_cron_reschedules_and_renaming_does_not() {
    let env = new_env(&["research"]).await;
    let ws_id = indexed_space(&env, "alpha", "research").await;
    let id = schedule_id(&env, &ws_id).await;
    let before = env.get("/api/schedules").await.body[0]["next_run_at"]
        .as_str()
        .unwrap()
        .to_string();

    let renamed = env
        .patch(&format!("/api/schedules/{id}"), json!({"name": "改名了"}))
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
    assert_eq!(renamed.body["name"], json!("改名了"));
    assert_eq!(renamed.body["next_run_at"], json!(before), "改名不该动排期");

    let recron = env
        .patch(
            &format!("/api/schedules/{id}"),
            json!({"cron_expr": "30 7 * * *"}),
        )
        .await;
    assert_eq!(recron.status, StatusCode::OK, "{}", recron.body);
    assert_eq!(
        recron.body["cron_expr"],
        json!("30 7 * * *"),
        "落库的是用户原文，不被补成 6 字段"
    );
    assert_ne!(
        recron.body["next_run_at"],
        json!(before),
        "换表达式必须换下一次时刻"
    );
    let next = recron.body["next_run_at"].as_str().unwrap();
    assert!(
        next.ends_with(":30:00"),
        "秒域补 0 之后落点仍在整分: {next}"
    );
}

/// 改表达式也只存原文：别的形状会让用户手上的文本与库里的对不上。
#[tokio::test]
async fn patching_the_cron_keeps_the_user_text() {
    let env = new_env(&["research"]).await;
    let ws_id = indexed_space(&env, "alpha", "research").await;
    let id = schedule_id(&env, &ws_id).await;

    let reply = env
        .patch(
            &format!("/api/schedules/{id}"),
            json!({"cron_expr": "*/5 * * * *"}),
        )
        .await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(
        reply.body["cron_expr"],
        json!("*/5 * * * *"),
        "回执逐字等于入参"
    );
    let list = env.get("/api/schedules").await;
    assert_eq!(list.body[0]["cron_expr"], json!("*/5 * * * *"));
}

#[tokio::test]
async fn disabling_clears_the_next_run_and_reenabling_restores_it() {
    let env = new_env(&["research"]).await;
    let ws_id = indexed_space(&env, "alpha", "research").await;
    let id = schedule_id(&env, &ws_id).await;

    let off = env
        .patch(&format!("/api/schedules/{id}"), json!({"enabled": false}))
        .await;
    assert_eq!(off.status, StatusCode::OK, "{}", off.body);
    assert_eq!(off.body["enabled"], json!(false));
    assert_eq!(off.body["next_run_at"], Value::Null);

    let on = env
        .patch(&format!("/api/schedules/{id}"), json!({"enabled": true}))
        .await;
    assert_eq!(on.status, StatusCode::OK, "{}", on.body);
    assert!(
        on.body["next_run_at"].as_str().is_some(),
        "重新启用必须重算下一次: {}",
        on.body
    );
}

#[tokio::test]
async fn patching_an_unknown_schedule_is_a_404() {
    let env = new_env(&["research"]).await;

    let reply = env
        .patch("/api/schedules/ghost", json!({"name": "x"}))
        .await;

    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.code(), "schedule_not_found");
}

#[tokio::test]
async fn patching_to_an_unparsable_cron_is_rejected() {
    let env = new_env(&["research"]).await;
    let ws_id = indexed_space(&env, "alpha", "research").await;
    let id = schedule_id(&env, &ws_id).await;

    let reply = env
        .patch(
            &format!("/api/schedules/{id}"),
            json!({"cron_expr": "nope"}),
        )
        .await;

    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.code(), "invalid_cron");
    let unchanged = env.get("/api/schedules").await.body[0]["cron_expr"].clone();
    assert_eq!(unchanged, json!("0 9 * * *"), "拒绝时表达式必须原样留下");
}

// ============================================================================
// 预览
// ============================================================================

/// 三个严格递增的 UTC 文本：前端的「接下来三次」直接照抄。
#[tokio::test]
async fn previewing_returns_three_increasing_times() {
    let env = new_env(&["research"]).await;

    let reply = env
        .post("/api/schedules/preview", json!({"cron_expr": "0 9 * * *"}))
        .await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let next: Vec<&str> = reply.body["next"]
        .as_array()
        .expect("next 必须是数组")
        .iter()
        .map(|value| value.as_str().expect("UTC 文本"))
        .collect();
    assert_eq!(next.len(), 3);
    for pair in next.windows(2) {
        assert!(pair[0] < pair[1], "必须严格递增: {next:?}");
    }
    for text in &next {
        assert_eq!(text.len(), 19, "形状与 datetime('now') 一致: {text}");
    }
}

#[tokio::test]
async fn previewing_a_broken_cron_is_rejected() {
    let env = new_env(&["research"]).await;

    let reply = env
        .post("/api/schedules/preview", json!({"cron_expr": "0 9 * *"}))
        .await;

    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.code(), "invalid_cron");
}

// ============================================================================
// 删除
// ============================================================================

#[tokio::test]
async fn deleting_removes_the_schedule_and_its_runs() {
    let env = new_env(&["research"]).await;
    let ws_id = indexed_space(&env, "alpha", "research").await;
    let id = schedule_id(&env, &ws_id).await;
    // 在途 run：删除前必须先掐掉它，行本身随外键级联消失。
    sqlx_insert_pending_run(&env, &id).await;

    let reply = env.delete(&format!("/api/schedules/{id}")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["removed"], json!(true));

    assert_eq!(env.get("/api/schedules").await.body, json!([]));
    assert!(
        schedules::task_by_id(&env.pool, &id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        schedules::runs_by_schedule(&env.pool, &id, 10)
            .await
            .unwrap()
            .is_empty(),
        "run 行必须随调度一起消失"
    );

    let again = env.delete(&format!("/api/schedules/{id}")).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    assert_eq!(again.code(), "schedule_not_found");
}

// ============================================================================
// 手动运行 + 执行历史
// ============================================================================

/// 手动运行返回 `{run_id, status}`，那一行立刻能被执行历史读到。
///
/// 目标空间里没有 `@master`（这份夹具不铺 agent），所以这一轮落在 `failed`：
/// 这里守的是「台账如实记下装不上」，而不是「一定能跑起来」（后者见
/// `schedule_fire.rs` 的端到端用例）。
#[tokio::test]
async fn a_manual_run_records_a_run_row() {
    let env = new_env(&["research"]).await;
    let ws_id = indexed_space(&env, "alpha", "research").await;
    let id = schedule_id(&env, &ws_id).await;
    assert!(!env.is_open(&ws_id).await, "起点必须是关着的空间");

    let reply = env
        .post(&format!("/api/schedules/{id}/run"), json!({}))
        .await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let run_id = reply.body["run_id"].as_str().expect("必须返回 run_id");
    assert!(
        env.is_open(&ws_id).await,
        "触发必须自己把目标空间打开（没有用户会替它点开）"
    );

    let history = env.get(&format!("/api/schedules/{id}/runs")).await;
    assert_eq!(history.status, StatusCode::OK, "{}", history.body);
    let rows = history.body.as_array().expect("裸数组");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], json!(run_id));
    assert_eq!(rows[0]["trigger_kind"], json!("manual"));
    assert_eq!(rows[0]["attempts"], json!(1), "装不上的那一次也要计尝试");
    assert_eq!(
        rows[0]["status"],
        json!("failed"),
        "没有 @master 的空间必须如实记失败: {}",
        rows[0]
    );
    assert!(
        rows[0]["error"].as_str().is_some_and(|e| !e.is_empty()),
        "失败必须留成因: {}",
        rows[0]
    );
}

/// 已有未完成 run 时再手动触发是 409：两条同时跑会互相踩着同一轮会话。
#[tokio::test]
async fn running_a_busy_schedule_is_a_409() {
    let env = new_env(&["research"]).await;
    let ws_id = indexed_space(&env, "alpha", "research").await;
    let id = schedule_id(&env, &ws_id).await;
    sqlx_insert_pending_run(&env, &id).await;

    let reply = env
        .post(&format!("/api/schedules/{id}/run"), json!({}))
        .await;

    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.code(), "schedule_busy");
}

#[tokio::test]
async fn running_an_unknown_schedule_is_a_404() {
    let env = new_env(&["research"]).await;

    let reply = env.post("/api/schedules/ghost/run", json!({})).await;

    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.code(), "schedule_not_found");
}

/// 历史接口的 `limit` 有上限：不设上限时一次请求能把整张表拖出来。
#[tokio::test]
async fn the_history_honours_its_limit() {
    let env = new_env(&["research"]).await;
    let ws_id = indexed_space(&env, "alpha", "research").await;
    let id = schedule_id(&env, &ws_id).await;
    for _ in 0..5 {
        sqlx_insert_pending_run(&env, &id).await;
    }

    let reply = env.get(&format!("/api/schedules/{id}/runs?limit=2")).await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body.as_array().expect("裸数组").len(), 2);
}

#[tokio::test]
async fn the_history_of_an_unknown_schedule_is_a_404() {
    let env = new_env(&["research"]).await;

    let reply = env.get("/api/schedules/ghost/runs").await;

    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.code(), "schedule_not_found");
}

/// 直接插一条待重试的 run：手动触发的手动排队路径（会话忙）在 HTTP 层不可控。
async fn sqlx_insert_pending_run(env: &TestEnv, schedule_id: &str) {
    let id = uuid::Uuid::new_v4().to_string();
    schedules::insert_run(
        &env.pool,
        &NewRun {
            id: &id,
            schedule_id,
            trigger_kind: TriggerKind::Manual,
            scheduled_for: &peco_daemon::schedule::utc_text(chrono::Utc::now()),
            status: RunStatus::Pending,
            error: None,
            finished_at: None,
        },
    )
    .await
    .expect("插入 run 不得失败")
    .expect("manual 触发不受槽位索引约束");
}
