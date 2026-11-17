//! `/api/spaces` 的 HTTP 语义：团队校验、幂等创建、普查、marker 警告、关闭门槛、改绑。

mod support;

use std::collections::BTreeSet;
use std::path::Path;

use peco_daemon::db;
use peco_daemon::db::dispatches;
use peco_daemon::space::manifest::MANIFEST_FILE;
use peco_daemon::space::peco::PECO_TEAM_ID;
use serde_json::json;
use support::new_env;

fn ws_id_of(reply: &support::HttpReply) -> String {
    reply.body["ws_id"]
        .as_str()
        .unwrap_or_else(|| panic!("响应缺 ws_id: {}", reply.body))
        .to_string()
}

fn read_marker(work_dir: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(work_dir.join(MANIFEST_FILE)).unwrap();
    serde_json::from_str(&raw).unwrap()
}

/// P1-4：team 目录不存在 ⇒ `409 team_missing`，且 work_dir 未被创建。
#[tokio::test]
async fn unknown_team_is_409_and_leaves_work_dir_absent() {
    let env = new_env(&["research"]).await;
    let work_dir = env.home.join("spaces").join("ghost").join("work");

    let reply = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "nope"}),
        )
        .await;

    assert_eq!(reply.status, 409, "body={}", reply.body);
    assert_eq!(reply.code(), "team_missing");
    assert!(
        !work_dir.exists(),
        "team_missing 不得留下 work_dir: {}",
        work_dir.display()
    );
    assert_eq!(env.count_workspaces(&work_dir).await, 0, "不得建空间行");
}

/// P1-5：同 work_dir 连发两次 ⇒ 第二次 `200`、`ws_id` 相同、DB 只有一行。
#[tokio::test]
async fn same_work_dir_twice_yields_one_row_and_same_ws_id() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("alpha");
    let body = json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"});

    let first = env.post("/api/spaces", body.clone()).await;
    assert_eq!(first.status, 201, "首次创建应是 201: {}", first.body);
    let second = env.post("/api/spaces", body).await;

    assert_eq!(second.status, 200, "重复创建应是 200: {}", second.body);
    assert_eq!(ws_id_of(&first), ws_id_of(&second), "ws_id 必须相同");
    assert_eq!(env.count_workspaces(&work_dir).await, 1, "UNIQUE(work_dir)");
    assert_eq!(first.body["team_id"], json!("research"));
    assert_eq!(first.body["work_dir"], json!(work_dir.to_string_lossy()));
}

/// 同 work_dir 换成另一个 team ⇒ `409 team_conflict`，DB 行与 marker 都不动；
/// 改回原 team 则仍是 `200` —— 冲突判断不得污染幂等路径。
#[tokio::test]
async fn same_work_dir_with_another_team_is_409_and_leaves_row_and_marker_intact() {
    let env = new_env(&["research", "writing"]).await;
    let work_dir = env.work_dir("bound");
    let work_dir_json = json!(work_dir.to_string_lossy());

    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir_json, "team_id": "research"}),
        )
        .await;
    assert_eq!(created.status, 201, "body={}", created.body);
    let ws_id = ws_id_of(&created);
    let marker_before = std::fs::read(work_dir.join(MANIFEST_FILE)).unwrap();

    let reply = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir_json, "team_id": "writing"}),
        )
        .await;

    assert_eq!(reply.status, 409, "body={}", reply.body);
    assert_eq!(reply.code(), "team_conflict");
    let message = reply.body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("research") && message.contains("writing") && message.contains("PATCH"),
        "文案必须点明当前团队、请求团队与改绑入口: {message}"
    );

    let (row_id, row_team): (String, String) =
        sqlx::query_as("SELECT id, team_id FROM workspaces WHERE work_dir = ?")
            .bind(work_dir.to_string_lossy().into_owned())
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(row_team, "research", "冲突不得改绑 DB 行");
    assert_eq!(row_id, ws_id, "冲突不得换身份");
    assert_eq!(env.count_workspaces(&work_dir).await, 1, "不得插第二行");
    assert_eq!(
        std::fs::read(work_dir.join(MANIFEST_FILE)).unwrap(),
        marker_before,
        "marker 必须是原字节"
    );
    assert_eq!(
        env.get(&format!("/api/spaces/{ws_id}")).await.body["team_id"],
        json!("research"),
        "已打开实例仍绑旧团队"
    );

    // 幂等路径未被冲突判断波及：同 work_dir 同 team 再来一次仍是 200。
    let again = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir_json, "team_id": "research"}),
        )
        .await;
    assert_eq!(again.status, 200, "body={}", again.body);
    assert_eq!(ws_id_of(&again), ws_id, "ws_id 必须相同");
}

/// P1-6：清空 teams + workspaces 后 `rescan`（库函数），`GET /api/spaces` 的
/// `ws_id` 集合与清空前完全相同。
#[tokio::test]
async fn rescan_restores_the_same_ws_id_set() {
    let env = new_env(&["research"]).await;
    for name in ["alpha", "beta"] {
        let work_dir = env.work_dir(name);
        let reply = env
            .post(
                "/api/spaces",
                json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
            )
            .await;
        assert_eq!(reply.status, 201, "body={}", reply.body);
    }

    let before = ws_id_set(&env.get("/api/spaces").await);
    assert_eq!(before.len(), 2, "两个空间都应出现在列表里");

    // 先删空间行再删团队行：workspaces.team_id 是 RESTRICT 外键。
    sqlx::query("DELETE FROM workspaces")
        .execute(&env.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM teams")
        .execute(&env.pool)
        .await
        .unwrap();
    let report = db::workspaces::rescan(&env.pool, &env.home).await.unwrap();
    assert_eq!(report.spaces, 2, "两个 marker 都应被普查到");

    let after = ws_id_set(&env.get("/api/spaces").await);
    assert_eq!(after, before, "普查不得改变空间身份");
}

fn ws_id_set(reply: &support::HttpReply) -> BTreeSet<String> {
    reply
        .body
        .as_array()
        .unwrap_or_else(|| panic!("列表应是数组: {}", reply.body))
        .iter()
        .map(|item| item["ws_id"].as_str().unwrap().to_string())
        .collect()
}

/// 目录里已有 `.workspace.json` ⇒ 身份由它的会话派生，打开不改写记录。
#[tokio::test]
async fn an_existing_manifest_keeps_its_identity_and_bytes() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("gamma");
    let before = json!({
        "format": "peco-workspace",
        "schema": 1,
        "name": "kept",
        "description": "do not touch",
        "team": env.home.join("teams").join("research").to_string_lossy(),
        "session_id": "s_from_marker",
        "created_at": "2026-02-11T09:14:22.531Z",
    });
    std::fs::write(work_dir.join(MANIFEST_FILE), before.to_string()).unwrap();
    let bytes_before = std::fs::read(work_dir.join(MANIFEST_FILE)).unwrap();

    let reply = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;

    // 该 work_dir 尚无空间行，所以状态是创建成功（201）。
    assert_eq!(reply.status, 201, "body={}", reply.body);
    assert_eq!(
        reply.body["ws_id"],
        json!("ws_s_from_marker"),
        "身份由会话派生"
    );
    assert_eq!(
        std::fs::read(work_dir.join(MANIFEST_FILE)).unwrap(),
        bytes_before,
        "打开不得改写现有记录"
    );
}

/// P1-9 ○1：两条 Peco 派发未终态 ⇒ `409 inflight_tasks` + `inflight_dispatch==2`
/// / `inflight_foreign==0`，且空间仍在 registry（未被关闭）。
#[tokio::test]
async fn close_is_rejected_with_two_dispatch_tasks_inflight() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("alpha");
    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;
    let ws_id = ws_id_of(&created);

    for task in ["t1", "t2"] {
        assert_eq!(
            env.insert_dispatch(&ws_id, task, "dispatch_to_workspace", "peco", "working")
                .await,
            1
        );
    }

    let reply = env
        .post(
            &format!("/api/spaces/{ws_id}/close"),
            json!({"force": false}),
        )
        .await;

    assert_eq!(reply.status, 409, "body={}", reply.body);
    assert_eq!(reply.code(), "inflight_tasks");
    assert_eq!(reply.body["inflight_dispatch"], json!(2));
    assert_eq!(reply.body["inflight_foreign"], json!(0));
    assert_eq!(
        env.get(&format!("/api/spaces/{ws_id}")).await.body["status"],
        json!("open"),
        "门槛不通过不得关闭空间"
    );
}

/// P1-9 ○2：只有一条外来在飞 ⇒ 同样 `409`（`inflight_foreign>=1`、`dispatch==0`）；
/// `force` 后该行 `task_state='orphan'` 且 `wake_state` 仍是 `skipped`。
#[tokio::test]
async fn force_close_orphans_foreign_inflight_without_waking() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("beta");
    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;
    let ws_id = ws_id_of(&created);
    assert_eq!(env.insert_l3_foreign(&ws_id, "f1").await, 1);

    let rejected = env
        .post(
            &format!("/api/spaces/{ws_id}/close"),
            json!({"force": false}),
        )
        .await;
    assert_eq!(rejected.status, 409, "body={}", rejected.body);
    assert_eq!(rejected.code(), "inflight_tasks");
    assert_eq!(rejected.body["inflight_foreign"], json!(1));
    assert_eq!(rejected.body["inflight_dispatch"], json!(0));

    let forced = env
        .post(
            &format!("/api/spaces/{ws_id}/close"),
            json!({"force": true}),
        )
        .await;
    assert_eq!(forced.status, 200, "body={}", forced.body);
    assert_eq!(forced.body["stopped"], json!(true));
    assert_eq!(
        env.task_and_wake_state(&ws_id, "f1").await,
        ("orphan".to_string(), "skipped".to_string()),
        "force 关闭必须置 orphan，且不产生唤醒"
    );
    assert_eq!(
        env.wake_skip_reason(&ws_id, "f1").await.as_deref(),
        Some("not_peco_origin"),
        "这行注册时就已跳过，关闭不顶掉更早的原因"
    );
    assert_eq!(
        env.get(&format!("/api/spaces/{ws_id}")).await.body["status"],
        json!("closed")
    );
}

/// P1-12：`PATCH` 到不存在的 team ⇒ `409 team_missing`，marker 与 DB 均未改。
#[tokio::test]
async fn patch_to_unknown_team_touches_nothing() {
    let env = new_env(&["research", "writing"]).await;
    let work_dir = env.work_dir("delta");
    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research", "label": "delta"}),
        )
        .await;
    let ws_id = ws_id_of(&created);

    let marker_before = std::fs::read(work_dir.join(MANIFEST_FILE)).unwrap();
    let row_before: (String, Option<String>, String) =
        sqlx::query_as("SELECT team_id, label, exit_policy FROM workspaces WHERE id = ?")
            .bind(&ws_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();

    let reply = env
        .patch(
            &format!("/api/spaces/{ws_id}"),
            json!({"team_id": "ghost", "label": "renamed", "exit_policy": "stop"}),
        )
        .await;

    assert_eq!(reply.status, 409, "body={}", reply.body);
    assert_eq!(reply.code(), "team_missing");
    assert_eq!(
        std::fs::read(work_dir.join(MANIFEST_FILE)).unwrap(),
        marker_before,
        "失败时 marker 必须是原字节"
    );
    let row_after: (String, Option<String>, String) =
        sqlx::query_as("SELECT team_id, label, exit_policy FROM workspaces WHERE id = ?")
            .bind(&ws_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(row_after, row_before, "失败时 DB 行不得改动");
}

/// 改绑到存在的 team：marker 与 DB 同时改，响应反映新 team。
#[tokio::test]
async fn patch_to_existing_team_rebinds_marker_and_row() {
    let env = new_env(&["research", "writing"]).await;
    let work_dir = env.work_dir("epsilon");
    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;
    let ws_id = ws_id_of(&created);

    let reply = env
        .patch(
            &format!("/api/spaces/{ws_id}"),
            json!({"team_id": "writing", "label": "renamed", "exit_policy": "stop"}),
        )
        .await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["team_id"], json!("writing"));
    assert_eq!(reply.body["label"], json!("renamed"));
    assert_eq!(reply.body["exit_policy"], json!("stop"));
    assert_eq!(
        reply.body["effective_exit_policy"],
        json!("stop"),
        "单空间 stop 覆盖全局 ask"
    );
    assert_eq!(
        read_marker(&work_dir)["team"],
        json!(env.home.join("teams").join("writing").to_string_lossy()),
        "marker 同步"
    );

    let row_team: String = sqlx::query_scalar("SELECT team_id FROM workspaces WHERE id = ?")
        .bind(&ws_id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(row_team, "writing");
}

/// 详情与列表的 404 / 计数口径：不存在的 ws_id 是 `space_not_found`。
#[tokio::test]
async fn detail_of_unknown_space_is_404() {
    let env = new_env(&["research"]).await;
    let reply = env.get("/api/spaces/ws_missing").await;
    assert_eq!(reply.status, 404, "body={}", reply.body);
    assert_eq!(reply.code(), "space_not_found");
    assert!(
        reply.body.get("inflight_dispatch").is_none()
            && reply.body.get("inflight_foreign").is_none(),
        "门槛计数只属于 409 inflight_tasks，别的错误不带: {}",
        reply.body
    );
    assert_eq!(
        env.post("/api/spaces/ws_missing/close", json!({}))
            .await
            .status,
        404
    );
}

/// `DELETE` 保留台账行（purge 只删空间行）。
#[tokio::test]
async fn purge_keeps_the_ledger() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("zeta");
    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;
    let ws_id = ws_id_of(&created);
    assert_eq!(
        env.insert_dispatch(&ws_id, "done", "dispatch_to_workspace", "peco", "completed")
            .await,
        1
    );

    let replied = env.delete(&format!("/api/spaces/{ws_id}")).await;
    assert_eq!(replied.status, 200, "body={}", replied.body);
    assert_eq!(replied.body["purged"], json!(true));
    assert_eq!(env.count_workspaces(&work_dir).await, 0, "空间行应被删除");
    assert_eq!(env.count_dispatches(&ws_id).await, 1, "台账必须长于空间行");
    assert!(
        work_dir.exists(),
        "purge 只删索引行，不碰 work_dir 与 marker"
    );
}

/// `input_required` / `auth_required` 不是终态 ⇒ 计入门槛，关闭被 `409` 拒绝。
#[tokio::test]
async fn close_is_rejected_while_a_task_waits_for_input() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("input-wait");
    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;
    let ws_id = ws_id_of(&created);
    let peco_ws_id = env.env.peco.ws_id.clone();

    env.insert_dispatch(
        &ws_id,
        "w1",
        "dispatch_to_workspace",
        &peco_ws_id,
        "input_required",
    )
    .await;
    let counts = dispatches::inflight_counts(&env.pool, &ws_id, &peco_ws_id)
        .await
        .unwrap();
    assert_eq!(counts.dispatch, 1, "input_required 是在飞，不是终态");

    env.insert_dispatch(
        &ws_id,
        "w2",
        "dispatch_to_workspace",
        &peco_ws_id,
        "auth_required",
    )
    .await;

    let reply = env
        .post(
            &format!("/api/spaces/{ws_id}/close"),
            json!({"force": false}),
        )
        .await;
    assert_eq!(reply.status, 409, "body={}", reply.body);
    assert_eq!(reply.code(), "inflight_tasks");
    assert_eq!(reply.body["inflight_dispatch"], json!(2));

    // 强制关闭必须把这两行也 orphan 化，否则重启对账永远扫不到。
    let forced = env
        .post(
            &format!("/api/spaces/{ws_id}/close"),
            json!({"force": true}),
        )
        .await;
    assert_eq!(forced.status, 200, "body={}", forced.body);
    assert_eq!(env.task_and_wake_state(&ws_id, "w1").await.0, "orphan");
    assert_eq!(env.task_and_wake_state(&ws_id, "w2").await.0, "orphan");
    // 两行本来还在等唤醒；强制关闭把它们停掉，并留下「空间关了」这个原因。
    for task_id in ["w1", "w2"] {
        assert_eq!(
            env.wake_skip_reason(&ws_id, task_id).await.as_deref(),
            Some("space_closed"),
            "{task_id} 的跳过原因必须是空间关闭"
        );
    }
}

/// `effective_exit_policy` 只作展示 —— 单空间 `inherit` 时跟随全局，全局缺省
/// `ask` 在服务端等价 `keep`，而 `POST /close` 仍然无条件关闭。
#[tokio::test]
async fn effective_exit_policy_is_reported_without_gating_close() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("policy");
    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;
    let ws_id = ws_id_of(&created);

    let detail = env.get(&format!("/api/spaces/{ws_id}")).await;
    assert_eq!(detail.body["exit_policy"], json!("inherit"));
    assert_eq!(
        detail.body["effective_exit_policy"],
        json!("keep"),
        "inherit + 全局 ask 等价 keep"
    );

    // keep 不拦关闭：关闭动作由 UI 显式触发，配置不是开关。
    let closed = env
        .post(
            &format!("/api/spaces/{ws_id}/close"),
            json!({"force": false}),
        )
        .await;
    assert_eq!(closed.status, 200, "body={}", closed.body);
    assert_eq!(
        env.get(&format!("/api/spaces/{ws_id}")).await.body["status"],
        json!("closed")
    );
}

/// 三个生命周期时刻要能分开三组：`last_opened_at` 记录「开过」，
/// `closed_at` 记录「显式关过」。只靠 `status` 时，`closed` 既可能是
/// 「库里有行、本次没开」也可能是「已关闭」，列表无法分组。
///
/// 变异：`space_view` 里把 `closed_at` 恒填 `None` → 关闭后的断言变红。
#[tokio::test]
async fn closed_at_and_last_opened_at_separate_the_three_groups() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("lifecycle");
    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;
    let ws_id = ws_id_of(&created);

    let open_row = env.get("/api/spaces").await;
    let row = open_row
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["ws_id"] == json!(ws_id))
        .expect("列表里必须有这个空间")
        .clone();
    assert_eq!(row["status"], json!("open"));
    assert_eq!(row["closed_at"], json!(null), "刚打开的空间没有被关闭过");
    assert!(
        row["last_opened_at"].as_str().is_some(),
        "打开成功必须写入 last_opened_at: {row}"
    );

    let closed = env
        .post(
            &format!("/api/spaces/{ws_id}/close"),
            json!({"force": true}),
        )
        .await;
    assert_eq!(closed.status, 200, "body={}", closed.body);

    let after = env.get("/api/spaces").await;
    let row = after
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["ws_id"] == json!(ws_id))
        .expect("关闭的行仍留在清单里")
        .clone();
    assert_eq!(row["status"], json!("closed"));
    assert!(
        row["closed_at"].as_str().is_some(),
        "显式关闭必须写 closed_at: {row}"
    );
    assert!(row["last_opened_at"].as_str().is_some(), "打开时刻保留");
}

/// Peco 只有引导建出的那一个保留空间：拿它的团队新建空间一律 `400`，且拒绝发生
/// 在任何 I/O 之前 —— 目录不会被建出来，索引行也不会被补。
#[tokio::test]
async fn creating_a_space_in_the_reserved_peco_team_is_400_and_leaves_nothing() {
    let env = new_env(&[PECO_TEAM_ID]).await;
    let work_dir = env.home.join("spaces").join("second").join("work");

    let reply = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": PECO_TEAM_ID}),
        )
        .await;

    assert_eq!(reply.status, 400, "body={}", reply.body);
    assert_eq!(reply.code(), "invalid_argument");
    assert!(
        !work_dir.exists(),
        "拒绝必须早于 create_dir_all: {}",
        work_dir.display()
    );
    assert_eq!(env.count_workspaces(&work_dir).await, 0, "不得留下空间行");
    assert!(
        env.home.join("teams").join(PECO_TEAM_ID).is_dir(),
        "拒绝不得动 Peco 团队目录"
    );
    assert_eq!(
        env.team_ids().await,
        Vec::<String>::new(),
        "拒绝必须早于 ensure_team_indexed，不得补出索引行"
    );
}

/// 改绑到 Peco 同样被拒：marker 与 DB 行都保持原样。
#[tokio::test]
async fn rebinding_a_space_to_the_reserved_peco_team_is_400() {
    let env = new_env(&["research", PECO_TEAM_ID]).await;
    let work_dir = env.work_dir("zeta");
    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;
    assert_eq!(created.status, 201, "body={}", created.body);
    let ws_id = ws_id_of(&created);
    let marker_before = std::fs::read(work_dir.join(MANIFEST_FILE)).unwrap();

    let reply = env
        .patch(
            &format!("/api/spaces/{ws_id}"),
            json!({"team_id": PECO_TEAM_ID}),
        )
        .await;

    assert_eq!(reply.status, 400, "body={}", reply.body);
    assert_eq!(reply.code(), "invalid_argument");
    let row_team: String = sqlx::query_scalar("SELECT team_id FROM workspaces WHERE id = ?")
        .bind(&ws_id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(row_team, "research", "被拒的改绑不得改 DB 行");
    assert_eq!(
        std::fs::read(work_dir.join(MANIFEST_FILE)).unwrap(),
        marker_before,
        "被拒的改绑不得改 marker"
    );
}

/// 列表里按 `ws_id` 挑一行：清单接口的断言不该依赖行序。
fn row_in(list: &serde_json::Value, ws_id: &str) -> serde_json::Value {
    list.as_array()
        .unwrap_or_else(|| panic!("空间清单必须是数组: {list}"))
        .iter()
        .find(|row| row["ws_id"] == json!(ws_id))
        .unwrap_or_else(|| panic!("清单里必须有 {ws_id}: {list}"))
        .clone()
}

/// 关闭之后显式打开：`POST /open` 回 200，视图是**打开之后**的那一行。
///
/// 变异：handler 复用打开之前取到的 row 拼视图 → `closed_at` 仍是旧值，断言变红。
#[tokio::test]
async fn open_endpoint_reopens_a_closed_space() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("reopen");
    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;
    let ws_id = ws_id_of(&created);

    let closed = env
        .post(
            &format!("/api/spaces/{ws_id}/close"),
            json!({"force": true}),
        )
        .await;
    assert_eq!(closed.status, 200, "body={}", closed.body);
    assert_eq!(
        env.get(&format!("/api/spaces/{ws_id}")).await.body["status"],
        json!("closed")
    );

    let reopened = env
        .post(&format!("/api/spaces/{ws_id}/open"), json!({}))
        .await;
    assert_eq!(reopened.status, 200, "body={}", reopened.body);
    assert_eq!(reopened.body["ws_id"], json!(ws_id));
    assert_eq!(reopened.body["status"], json!("open"));
    assert_eq!(
        reopened.body["closed_at"],
        json!(null),
        "重开必须清掉关闭时刻: {}",
        reopened.body
    );
    assert!(
        reopened.body["last_opened_at"].as_str().is_some(),
        "打开必须写入 last_opened_at: {}",
        reopened.body
    );

    let listed = row_in(&env.get("/api/spaces").await.body, &ws_id);
    assert_eq!(listed["status"], json!("open"));
    assert_eq!(listed["closed_at"], json!(null));
    assert!(listed["last_opened_at"].as_str().is_some());
}

/// 幂等：已经打开的空间再开一次仍是 200，状态不变。
#[tokio::test]
async fn open_endpoint_is_idempotent() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("twice");
    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;
    let ws_id = ws_id_of(&created);

    for attempt in 1..=2 {
        let reply = env
            .post(&format!("/api/spaces/{ws_id}/open"), json!({}))
            .await;
        assert_eq!(reply.status, 200, "第 {attempt} 次打开: {}", reply.body);
        assert_eq!(reply.body["status"], json!("open"));
        assert_eq!(reply.body["closed_at"], json!(null));
    }
    assert_eq!(
        env.get(&format!("/api/spaces/{ws_id}")).await.body["status"],
        json!("open")
    );
}

/// 未知 id 是 `space_not_found`，与关闭、详情同一个口径。
#[tokio::test]
async fn open_unknown_space_is_404() {
    let env = new_env(&["research"]).await;

    let reply = env.post("/api/spaces/ws_nope/open", json!({})).await;

    assert_eq!(reply.status, 404, "body={}", reply.body);
    assert_eq!(reply.code(), "space_not_found");
}

/// 目录消失 ⇒ `work_dir_missing`，并把这一行记成「已关闭」—— 列表的三组靠它分开。
///
/// 前件是「打开过、实例又不在、目录也没了」：实例还活着时 `/open` 直接回视图，
/// 那条路径由 `open_of_a_live_space_survives_a_deleted_work_dir` 钉住。
#[tokio::test]
async fn open_reports_missing_work_dir_and_marks_closed() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("gone");
    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;
    let ws_id = ws_id_of(&created);

    // 前件成立才算「打开过、目录又没了」：last_opened_at 有值、closed_at 还是空，
    // 实例则已经不在表里 —— 重启之后每一行都是这个样子。
    let before = env.get(&format!("/api/spaces/{ws_id}")).await;
    assert!(before.body["last_opened_at"].as_str().is_some());
    assert_eq!(before.body["closed_at"], json!(null));
    env.hub.forget(&work_dir);
    assert!(!env.is_open(&ws_id).await, "前件：实例不在表里");

    std::fs::remove_dir_all(&work_dir).unwrap();

    let reply = env
        .post(&format!("/api/spaces/{ws_id}/open"), json!({}))
        .await;
    assert_eq!(reply.status, 409, "body={}", reply.body);
    assert_eq!(reply.code(), "work_dir_missing");

    let after = env.get(&format!("/api/spaces/{ws_id}")).await;
    assert!(
        after.body["closed_at"].as_str().is_some(),
        "打不开的空间要落进「已关闭」: {}",
        after.body
    );
}

/// 实例已经活着时目录被删：`/open` 仍是 200 —— 在跑的实例不该被一次目录检查报成打不开。
///
/// 变异：去掉 handler 里 `hub.is_open` 的短路 → 落进 `ensure_open_indexed` 的目录判定，
/// 这条请求变成 409 `work_dir_missing`。
#[tokio::test]
async fn open_of_a_live_space_survives_a_deleted_work_dir() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("live");
    let created = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;
    let ws_id = ws_id_of(&created);
    assert!(env.is_open(&ws_id).await, "夹具前提：创建即打开");

    std::fs::remove_dir_all(&work_dir).unwrap();

    let reply = env
        .post(&format!("/api/spaces/{ws_id}/open"), json!({}))
        .await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["status"], json!("open"));
    assert_eq!(
        reply.body["closed_at"],
        json!(null),
        "活着的实例不该被记成「已关闭」: {}",
        reply.body
    );

    let detail = env.get(&format!("/api/spaces/{ws_id}")).await;
    assert_eq!(detail.body["status"], json!("open"));
}
