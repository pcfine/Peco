//! `/api/spaces/{ws_id}/artifacts` 与 `/overview` —— 空间详情页与看板卡片的读接口。
//!
//! 两条都按**路径里的 ws_id** 选空间：不要求 `X-Peco-Space` 头（路径已携带），
//! 也都不得依赖空间仍处于打开状态。

mod support;

use peco_daemon::db::artifacts::{self, ArtifactRow};
use peco_daemon::db::conversations::{self, CreateConversationParams};
use serde_json::json;
use support::{TestEnv, new_env};
use uuid::Uuid;

async fn insert_conversation(env: &TestEnv, ws_id: &str, agent_name: &str) -> String {
    let id = Uuid::new_v4().to_string();
    conversations::insert(
        &env.pool,
        &CreateConversationParams {
            id: id.clone(),
            workspace_id: ws_id.to_string(),
            agent_id: None,
            agent_name: agent_name.to_string(),
            title: "t".into(),
        },
    )
    .await
    .unwrap();
    id
}

async fn insert_artifact(env: &TestEnv, conversation_id: &str, rel_path: &str) {
    artifacts::insert(
        &env.pool,
        &ArtifactRow {
            id: Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            user_id: peco_daemon::LOCAL_USER_ID.to_string(),
            kind: "document".into(),
            mime: "text/plain".into(),
            title: rel_path.to_string(),
            rel_path: rel_path.to_string(),
            size_bytes: 123,
            created_at: peco_core::workspace::manifest::now_rfc3339(),
        },
    )
    .await
    .unwrap();
}

/// 两个空间各一条产物 ⇒ 各自只看到自己的那一条。
///
/// 变异：去掉 `WHERE c.workspace_id = ?`（改成按 `artifacts.user_id` 过滤或不过滤）
/// → 两个断言同时变红（每个空间会看到两条）。
#[tokio::test]
async fn artifacts_are_scoped_to_the_space_in_the_path() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("board-a", "research").await;
    let ws_b = env.open_space("board-b", "research").await;
    let conv_a = insert_conversation(&env, &ws_a, "@master").await;
    let conv_b = insert_conversation(&env, &ws_b, "@coder").await;
    insert_artifact(&env, &conv_a, "output/a.txt").await;
    insert_artifact(&env, &conv_b, "output/b.txt").await;

    let reply_a = env.get(&format!("/api/spaces/{ws_a}/artifacts")).await;
    assert_eq!(reply_a.status, 200, "body={}", reply_a.body);
    assert_eq!(reply_a.body["total"], json!(1), "total={}", reply_a.body);
    let items_a = reply_a.body["items"].as_array().unwrap();
    assert_eq!(items_a.len(), 1);
    assert_eq!(items_a[0]["rel_path"], json!("output/a.txt"));
    assert_eq!(items_a[0]["conversation_id"], json!(conv_a));
    assert_eq!(items_a[0]["agent_name"], json!("@master"), "来自会话行");
    assert_eq!(items_a[0]["size_bytes"], json!(123));

    let reply_b = env.get(&format!("/api/spaces/{ws_b}/artifacts")).await;
    assert_eq!(reply_b.body["total"], json!(1), "body={}", reply_b.body);
    assert_eq!(
        reply_b.body["items"][0]["rel_path"],
        json!("output/b.txt"),
        "另一个空间的产物不得串过来"
    );
}

/// `limit` 默认 50、上限 200：不设上限等于让一次请求把整库读出来。
///
/// 变异：去掉 `.clamp(1, MAX_LIMIT)` → 205 条全部返回，`items.len()` 断言变红。
#[tokio::test]
async fn artifacts_limit_is_capped_at_two_hundred() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("board-many", "research").await;
    let conv = insert_conversation(&env, &ws_id, "@master").await;
    for index in 0..205 {
        insert_artifact(&env, &conv, &format!("output/f{index:03}.txt")).await;
    }

    let capped = env
        .get(&format!("/api/spaces/{ws_id}/artifacts?limit=1000"))
        .await;
    assert_eq!(capped.status, 200, "body={}", capped.body);
    assert_eq!(capped.body["items"].as_array().unwrap().len(), 200);
    assert_eq!(capped.body["total"], json!(205), "total 不随 limit 变化");

    let small = env
        .get(&format!("/api/spaces/{ws_id}/artifacts?limit=1"))
        .await;
    assert_eq!(small.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(small.body["total"], json!(205));
}

/// 未索引的 ws_id ⇒ `404 space_not_found`（与 `/api/spaces/{ws_id}/tasks` 同口径）。
#[tokio::test]
async fn artifacts_of_an_unknown_space_are_404() {
    let env = new_env(&["research"]).await;

    let reply = env.get("/api/spaces/ws_ghost/artifacts").await;

    assert_eq!(reply.status, 404, "body={}", reply.body);
    assert_eq!(reply.code(), "space_not_found");
}

/// 今日四格：1 completed、0 failed、1 working、1 input_required；昨日那行不进今日计数。
///
/// 变异：
/// - `active` 直接用 `ACTIVE_STATES` 全量（含等待中）→ `active` 变 2 而红
///   （格子会重复计数同一个任务）；
/// - 去掉 `date(created_at,'localtime') = date('now','localtime')` → `active`
///   把昨日那行也算进来而红。
#[tokio::test]
async fn overview_counts_today_and_only_today() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("overview-today");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();

    let done = "task-done";
    let working = "task-working";
    let waiting = "task-waiting";
    let yesterday = "task-yesterday";
    env.insert_dispatch(
        &ws_id,
        done,
        "dispatch_to_workspace",
        "ws_peco",
        "completed",
    )
    .await;
    env.insert_dispatch(
        &ws_id,
        working,
        "dispatch_to_workspace",
        "ws_peco",
        "working",
    )
    .await;
    env.insert_dispatch(
        &ws_id,
        waiting,
        "dispatch_to_workspace",
        "ws_peco",
        "input_required",
    )
    .await;
    env.insert_dispatch(
        &ws_id,
        yesterday,
        "dispatch_to_workspace",
        "ws_peco",
        "working",
    )
    .await;
    sqlx::query("UPDATE d_dispatches SET created_at = datetime('now','-1 day') WHERE task_id = ?")
        .bind(yesterday)
        .execute(&env.pool)
        .await
        .unwrap();

    let reply = env.get(&format!("/api/spaces/{ws_id}/overview")).await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["ws_id"], json!(ws_id));
    assert_eq!(reply.body["team_id"], json!("research"));
    assert_eq!(reply.body["tasks"]["today"]["completed"], json!(1));
    assert_eq!(reply.body["tasks"]["today"]["failed"], json!(0));
    assert_eq!(reply.body["tasks"]["today"]["active"], json!(1));
    assert_eq!(reply.body["tasks"]["today"]["waiting"], json!(1));
    assert_eq!(
        reply.body["inflight"]["dispatch"],
        json!(3),
        "在飞是当下状态，不受「今天」时间窗限制"
    );
    assert_eq!(reply.body["inflight"]["foreign"], json!(0));

    let recent = reply.body["tasks"]["recent"].as_array().unwrap();
    assert_eq!(recent.len(), 4);
    assert_eq!(
        recent.last().unwrap()["task_id"],
        json!(yesterday),
        "created_at 倒序：昨天那行排最后"
    );
    // 卡片上的状态列要能直接读到两套状态域，字段名与 `/api/tasks` 一致。
    let done_row = recent
        .iter()
        .find(|row| row["task_id"] == json!(done))
        .expect("已完成那行必须在 recent 里");
    assert_eq!(done_row["task_state"], json!("completed"));
    assert_eq!(done_row["wake_state"], json!("pending"));
    assert_eq!(done_row["wake_skip_reason"], json!(null));
    assert!(
        done_row.get("state").is_none(),
        "recent 行不得再出现旧字段 state：{done_row}"
    );
}

/// 失败不进「已完成」：今日三列里失败单占一格，不计入成功数。
///
/// 变异：把 `failed` 并回 `completed`（`completed` 用终态集合计数）→
/// `completed` 变 2 而红。
#[tokio::test]
async fn overview_counts_failed_separately_from_completed() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("overview-failed");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();

    env.insert_dispatch(
        &ws_id,
        "task-done",
        "dispatch_to_workspace",
        "ws_peco",
        "completed",
    )
    .await;
    env.insert_dispatch(
        &ws_id,
        "task-failed",
        "dispatch_to_workspace",
        "ws_peco",
        "failed",
    )
    .await;
    env.insert_dispatch(
        &ws_id,
        "task-working",
        "dispatch_to_workspace",
        "ws_peco",
        "working",
    )
    .await;

    let reply = env.get(&format!("/api/spaces/{ws_id}/overview")).await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["tasks"]["today"]["completed"], json!(1));
    assert_eq!(reply.body["tasks"]["today"]["failed"], json!(1));
    assert_eq!(reply.body["tasks"]["today"]["active"], json!(1));
    assert_eq!(reply.body["tasks"]["today"]["waiting"], json!(0));
}

/// `recent` 最多 5 条。
///
/// 变异：把 `RECENT_LIMIT` 改成不设上限（或 100）→ 本测试变红。
#[tokio::test]
async fn overview_keeps_recent_to_five_rows() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("overview-recent");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    for index in 1..=6 {
        let task_id = format!("task-{index}");
        env.insert_dispatch(
            &ws_id,
            &task_id,
            "dispatch_to_workspace",
            "ws_peco",
            "working",
        )
        .await;
        sqlx::query("UPDATE d_dispatches SET created_at = datetime('now', ?) WHERE task_id = ?")
            .bind(format!("-{index} minutes"))
            .bind(&task_id)
            .execute(&env.pool)
            .await
            .unwrap();
    }

    let reply = env.get(&format!("/api/spaces/{ws_id}/overview")).await;

    let recent = reply.body["tasks"]["recent"].as_array().unwrap();
    let ids: Vec<&str> = recent
        .iter()
        .map(|row| row["task_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["task-1", "task-2", "task-3", "task-4", "task-5"],
        "最新 5 条，最新在前"
    );
}

/// 空间未打开时 `agent_count` / `skill_count` 为 0，接口照常 200。
///
/// 变异：把 `state.spaces.opened(...)` 换成 `state.spaces.handle(...)`（未打开即
/// `409 space_not_open`）→ 本测试变红。
#[tokio::test]
async fn overview_of_a_closed_space_reports_zero_capabilities() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("overview-closed");
    // 只索引、不打开：等价于 daemon 重启后尚未 open 的空间。
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();

    let reply = env.get(&format!("/api/spaces/{ws_id}/overview")).await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["agent_count"], json!(0));
    assert_eq!(reply.body["skill_count"], json!(0));
    assert_eq!(
        reply.body["tasks"]["recent"],
        json!([]),
        "没有台账行就是空清单"
    );
}

/// 空间打开时计数来自实例：团队里装了几个 agent / skill 就是几。
///
/// 变异：把计数写死 0（或恒取磁盘目录数而不看实例）→ 本测试变红。
#[tokio::test]
async fn overview_of_an_open_space_counts_agents_and_skills() {
    let env = new_env(&["research"]).await;
    let root = env.home.join("teams/research");
    for name in ["@master", "@coder"] {
        let dir = root.join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.md"),
            format!("---\nagent:\n  name: \"{name}\"\n  description: \"d\"\n---\n\n# p\n"),
        )
        .unwrap();
    }
    let skill = root.join("skills/code-review");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: \"code-review\"\ndescription: \"d\"\n---\n\n# s\n",
    )
    .unwrap();

    let ws_id = env.open_space("overview-open", "research").await;

    let reply = env.get(&format!("/api/spaces/{ws_id}/overview")).await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["agent_count"], json!(2));
    assert_eq!(reply.body["skill_count"], json!(1));
}
