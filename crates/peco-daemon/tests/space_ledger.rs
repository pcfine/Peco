//! 台账与空间行的生命周期：删空间不丢审计、关闭门槛不被绕过。

mod support;

use peco_daemon::db::dispatches;
use peco_daemon::error::DaemonError;
use peco_daemon::space::close::{close_workspace, purge_workspace};
use peco_daemon::space::manifest::MANIFEST_FILE;
use support::new_env;

/// P1-13 三格：删行不减台账、marker 存活时重建 `ws_id` 不变、目录与 marker 全删才换新身份。
#[tokio::test]
async fn ledger_outlives_workspace_row_and_identity_survives_rebuild() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("audit");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();

    for task in ["t1", "t2", "t3"] {
        assert_eq!(
            env.insert_dispatch(&ws_id, task, "dispatch_to_workspace", "peco", "completed")
                .await,
            1
        );
    }
    assert_eq!(env.count_dispatches(&ws_id).await, 3);

    // ○1 删空间行：台账行数必须不变
    sqlx::query("DELETE FROM workspaces WHERE id = ?")
        .bind(&ws_id)
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(
        env.count_dispatches(&ws_id).await,
        3,
        "○1 删空间行不得连带删台账"
    );

    // ○2 marker 存活时重建：ws_id 不变，新 task 可插，重复 upsert 被 UNIQUE 拦
    let rebuilt = env.resolve(&work_dir, "research").await.unwrap();
    assert_eq!(rebuilt, ws_id, "○2 marker 存活时身份不得改变");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM workspaces WHERE id = ?")
        .bind(&ws_id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1, "○2 应补回一行");

    assert_eq!(
        env.insert_dispatch(&ws_id, "t4", "dispatch_to_workspace", "peco", "submitted")
            .await,
        1
    );
    assert_eq!(env.count_dispatches(&ws_id).await, 4);
    assert_eq!(
        env.insert_dispatch(&ws_id, "t4", "dispatch_to_workspace", "peco", "working")
            .await,
        0,
        "○2 重复 upsert 必须被拦"
    );
    assert_eq!(env.count_dispatches(&ws_id).await, 4);

    // ○3 目录与 marker 全删：新 ws_id，旧台账照旧存在
    sqlx::query("DELETE FROM workspaces WHERE id = ?")
        .bind(&ws_id)
        .execute(&env.pool)
        .await
        .unwrap();
    assert!(work_dir.join(MANIFEST_FILE).exists());
    std::fs::remove_dir_all(&work_dir).unwrap();
    std::fs::create_dir_all(&work_dir).unwrap();

    let brand_new = env.resolve(&work_dir, "research").await.unwrap();
    assert_ne!(brand_new, ws_id, "○3 身份丢失后必须生成新 ws_id");
    // ○2 已把旧身份的台账加到 4 行（3 条 + t4），○3 只要求它们全都还在。
    assert_eq!(
        env.count_dispatches(&ws_id).await,
        4,
        "○3 旧台账不得被新身份带走"
    );
    assert_eq!(
        env.insert_dispatch(
            &brand_new,
            "t5",
            "dispatch_to_workspace",
            "peco",
            "submitted"
        )
        .await,
        1
    );
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM d_dispatches")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(total, 5, "新旧台账必须共存");
}

/// 门槛 ○1：Peco 派发的在飞任务计入 `inflight_dispatch`，无 `force` 时关闭被拒。
#[tokio::test]
async fn close_is_blocked_by_dispatched_inflight_until_forced() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("gate-dispatch");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();

    env.insert_dispatch(&ws_id, "d1", "dispatch_to_workspace", "peco", "submitted")
        .await;
    env.insert_dispatch(&ws_id, "d2", "dispatch_to_workspace", "peco", "working")
        .await;

    let err = close_workspace(&env.env, &env.hub, &ws_id, false)
        .await
        .unwrap_err();
    match err {
        DaemonError::InflightTasks {
            inflight_dispatch,
            inflight_foreign,
            ..
        } => {
            assert_eq!(inflight_dispatch, 2);
            assert_eq!(inflight_foreign, 0);
        }
        other => panic!("期望 inflight_tasks，实际 {} ({other})", other.code()),
    }
    assert!(env.is_open(&ws_id).await, "409 之后空间必须仍在实例表里");

    let outcome = close_workspace(&env.env, &env.hub, &ws_id, true)
        .await
        .unwrap();
    assert_eq!(outcome.orphaned, 2);
    assert_eq!(outcome.inflight.dispatch, 2);
    assert!(!env.is_open(&ws_id).await);

    let rows = dispatches::inflight_counts(&env.pool, &ws_id, &env.env.peco.ws_id)
        .await
        .unwrap();
    assert_eq!(rows.total(), 0, "force 后不得再有未终态行");
    let states: Vec<String> =
        sqlx::query_scalar("SELECT state FROM d_dispatches WHERE target_ws_id = ?")
            .bind(&ws_id)
            .fetch_all(&env.pool)
            .await
            .unwrap();
    assert_eq!(states, vec!["orphan", "orphan"]);

    let closed_at: Option<String> =
        sqlx::query_scalar("SELECT closed_at FROM workspaces WHERE id = ?")
            .bind(&ws_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert!(closed_at.is_some(), "close 必须写 closed_at");
}

/// 门槛 ○2：外来在飞（`origin_ws_id == target_ws_id`）同样计入，且 `wake_state` 不被改写。
#[tokio::test]
async fn close_is_blocked_by_foreign_inflight_and_keeps_wake_state() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("gate-foreign");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();

    env.insert_dispatch(&ws_id, "f1", "run_machine_task", &ws_id, "working")
        .await;
    sqlx::query("UPDATE d_dispatches SET wake_state = 'skipped' WHERE target_ws_id = ?")
        .bind(&ws_id)
        .execute(&env.pool)
        .await
        .unwrap();

    let err = close_workspace(&env.env, &env.hub, &ws_id, false)
        .await
        .unwrap_err();
    match err {
        DaemonError::InflightTasks {
            inflight_dispatch,
            inflight_foreign,
            ..
        } => {
            assert_eq!(inflight_dispatch, 0);
            assert_eq!(inflight_foreign, 1);
        }
        other => panic!("期望 inflight_tasks，实际 {} ({other})", other.code()),
    }

    let outcome = close_workspace(&env.env, &env.hub, &ws_id, true)
        .await
        .unwrap();
    assert_eq!(outcome.orphaned, 1);

    let (state, wake_state) = sqlx::query_as::<_, (String, String)>(
        "SELECT state, wake_state FROM d_dispatches WHERE target_ws_id = ?",
    )
    .bind(&ws_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(state, "orphan");
    assert_eq!(
        wake_state, "skipped",
        "外来在飞行的 wake_state 必须保持 skipped（不产生唤醒）"
    );
}

/// `purge` 走同一道门槛，通过后删行但保留台账。
#[tokio::test]
async fn purge_removes_row_but_keeps_ledger() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("purge");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();
    env.insert_dispatch(&ws_id, "p1", "dispatch_to_workspace", "peco", "working")
        .await;

    let err = purge_workspace(&env.env, &env.hub, &ws_id, false)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "inflight_tasks");

    let outcome = purge_workspace(&env.env, &env.hub, &ws_id, true)
        .await
        .unwrap();
    assert!(outcome.purged);
    assert_eq!(env.count_workspaces(&work_dir).await, 0, "空间行应被删掉");
    assert_eq!(env.count_dispatches(&ws_id).await, 1, "台账必须保留");
    assert!(
        !env.hub.is_open(&work_dir),
        "purge 后实例表不得再看到该空间"
    );
}

/// 未在册的空间：关闭/删除返回 `space_not_found`。
#[tokio::test]
async fn closing_unknown_space_reports_not_found() {
    let env = new_env(&["research"]).await;
    let err = purge_workspace(&env.env, &env.hub, "ws_ghost", false)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "space_not_found");
}
