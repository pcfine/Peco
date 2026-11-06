//! 打开路径：`ws_id` 复用、`.workspace.json` 原子写、冲突决策表。

mod support;

use std::path::Path;

use peco_daemon::space::manifest::MANIFEST_FILE;
use peco_daemon::space::open;
use serde_json::json;
use support::new_env;

fn marker_path(work_dir: &Path) -> std::path::PathBuf {
    work_dir.join(MANIFEST_FILE)
}

fn read_marker_json(work_dir: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(marker_path(work_dir)).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn write_marker_json(work_dir: &Path, value: &serde_json::Value) {
    std::fs::write(marker_path(work_dir), value.to_string()).unwrap();
}

/// 记录里的会话 id 是 `ws_id` 的稳定种子。
fn manifest_ws_id(value: &serde_json::Value) -> String {
    format!(
        "ws_{}",
        value["session_id"].as_str().expect("session_id is present")
    )
}

/// P1-5：同 work_dir 同 team 连开两次 ⇒ 同一个 `ws_id`，且只有一行 workspaces。
#[tokio::test]
async fn reopening_same_work_dir_reuses_the_same_ws_id() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("alpha");

    let first = env.resolve(&work_dir, "research").await.unwrap();
    let ws_first = env.open(&first).await.unwrap();
    let second = env.resolve(&work_dir, "research").await.unwrap();
    let ws_second = env.open(&second).await.unwrap();

    assert_eq!(first, second, "同 work_dir 必须复用同一个 ws_id");
    assert!(
        std::sync::Arc::ptr_eq(&ws_first, &ws_second),
        "第二次打开应拿到同一个实例"
    );
    assert_eq!(
        env.count_workspaces(&work_dir).await,
        1,
        "UNIQUE(work_dir) 只允许一行"
    );

    let marker = read_marker_json(&work_dir);
    assert_eq!(
        manifest_ws_id(&marker),
        first,
        "marker 派生的身份必须与 DB 主键一致"
    );
}

/// P1-7：marker 写完不留 `.tmp*`，内容可被 `serde_json` 解析且 schema == 1。
#[tokio::test]
async fn marker_is_atomic_parsable_and_schema_one() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("beta");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();

    let leftovers: Vec<String> = std::fs::read_dir(&work_dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "marker 原子写不得残留临时文件: {leftovers:?}"
    );

    let marker = read_marker_json(&work_dir);
    assert_eq!(marker["format"], json!("peco-workspace"));
    assert_eq!(marker["schema"], json!(1));
    assert_eq!(
        marker["team"],
        json!(env.home.join("teams").join("research").to_string_lossy())
    );
    assert_eq!(manifest_ws_id(&marker), ws_id);
    assert!(marker.get("user_id").is_none(), "记录不再保存创建者");
    assert!(marker.get("ws_id").is_none(), "ws_id 由会话派生");
}

/// 已有记录在打开时保持原字节：创建时间、名称与会话都不被改写。
#[tokio::test]
async fn an_existing_manifest_is_not_rewritten_on_open() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("gamma");
    write_marker_json(
        &work_dir,
        &json!({
            "format": "peco-workspace",
            "schema": 1,
            "name": "kept",
            "description": "do not touch",
            "team": env.home.join("teams").join("research").to_string_lossy(),
            "session_id": "s_from_marker",
            "created_at": "2026-02-11T09:14:22.531Z",
        }),
    );
    let before = std::fs::read(marker_path(&work_dir)).unwrap();

    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    assert_eq!(ws_id, "ws_s_from_marker", "身份由会话派生");

    env.open(&ws_id).await.unwrap();

    assert_eq!(
        std::fs::read(marker_path(&work_dir)).unwrap(),
        before,
        "打开动作不得改写已有记录"
    );
}

/// P1-15 #3：marker 有、DB 无 ⇒ 用会话派生的 `ws_id` 补行，身份不变。
#[tokio::test]
async fn marker_survives_missing_db_row_and_keeps_identity() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("delta");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();

    sqlx::query("DELETE FROM workspaces WHERE id = ?")
        .bind(&ws_id)
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(env.count_workspaces(&work_dir).await, 0);

    let resolved = env.resolve(&work_dir, "research").await.unwrap();
    assert_eq!(resolved, ws_id, "索引丢失不得换来新身份");
    assert_eq!(env.count_workspaces(&work_dir).await, 1, "应补回一行");
}

/// P1-15 #4：marker 无、DB 有 ⇒ 从 DB 行重建 marker，`ws_id` 不变。
#[tokio::test]
async fn missing_marker_is_rebuilt_from_db_row() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("epsilon");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();

    std::fs::remove_file(marker_path(&work_dir)).unwrap();
    env.reopen(&ws_id).await.unwrap();
    assert_eq!(manifest_ws_id(&read_marker_json(&work_dir)), ws_id);
}

/// P1-15 #4 的另一半：marker 与目录都没了 ⇒ `409 work_dir_missing` 并标 `closed_at`。
#[tokio::test]
async fn vanished_work_dir_without_marker_reports_work_dir_missing() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("zeta");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();

    std::fs::remove_dir_all(&work_dir).unwrap();

    let err = env
        .open(&ws_id)
        .await
        .err()
        .expect("目录消失后打开必须失败");
    assert_eq!(err.code(), "work_dir_missing");

    let closed_at: Option<String> =
        sqlx::query_scalar("SELECT closed_at FROM workspaces WHERE id = ?")
            .bind(&ws_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert!(closed_at.is_some(), "空间行应被标记关闭");
}

/// P1-15 #6：同 work_dir 的 `ws_id` 不同 ⇒ `409 marker_db_conflict`，DB 行不被改写。
#[tokio::test]
async fn differing_ws_ids_raise_marker_db_conflict_without_touching_db() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("eta");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();

    let mut marker = read_marker_json(&work_dir);
    marker["session_id"] = json!("somebody_else");
    write_marker_json(&work_dir, &marker);

    let err = env
        .reopen(&ws_id)
        .await
        .err()
        .expect("marker 与 DB 的 ws_id 冲突必须失败");
    assert_eq!(err.code(), "marker_db_conflict");

    let db_ws_id: String = sqlx::query_scalar("SELECT id FROM workspaces WHERE work_dir = ?")
        .bind(work_dir.to_string_lossy().into_owned())
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(db_ws_id, ws_id, "冲突不得就地改名");
    assert_eq!(
        read_marker_json(&work_dir)["session_id"],
        json!("somebody_else")
    );
}

/// marker 的 team 与 DB 行不同 ⇒ marker 优先，DB 行改绑。
#[tokio::test]
async fn marker_wins_on_team_binding() {
    let env = new_env(&["research", "writing"]).await;
    let work_dir = env.work_dir("theta");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();

    let mut marker = read_marker_json(&work_dir);
    marker["team"] = json!(env.home.join("teams").join("writing").to_string_lossy());
    write_marker_json(&work_dir, &marker);

    env.reopen(&ws_id).await.unwrap();

    let team_id: String = sqlx::query_scalar("SELECT team_id FROM workspaces WHERE id = ?")
        .bind(&ws_id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(team_id, "writing", "marker 优先");
}

/// 团队目录不存在 ⇒ `team_missing`，且 work_dir 不被创建。
#[tokio::test]
async fn unknown_team_is_rejected_without_creating_work_dir() {
    let env = new_env(&["research"]).await;
    let work_dir = env.home.join("spaces").join("missing-team").join("work");

    let err = env.resolve(&work_dir, "nope").await.unwrap_err();
    assert_eq!(err.code(), "team_missing");
    assert!(!work_dir.exists(), "team_missing 不得创建 work_dir");
    assert_eq!(env.count_workspaces(&work_dir).await, 0);
}

/// record 里的能力根在 daemon 团队根之外 ⇒ `409 team_path_mismatch`，
/// 且两条打开路径（无索引行 / 有索引行）都不留下实例。
#[tokio::test]
async fn team_path_outside_the_daemon_root_is_rejected() {
    let env = new_env(&["x"]).await;
    let work_dir = env.work_dir("outside");

    // 同名团队存在，但 record 指向另一个同名目录：只有规范化后的路径能分辨。
    let elsewhere = tempfile::tempdir().unwrap();
    let foreign_team = elsewhere.path().join("x");
    std::fs::create_dir_all(&foreign_team).unwrap();

    write_marker_json(
        &work_dir,
        &json!({
            "format": "peco-workspace",
            "schema": 1,
            "name": "foreign",
            "description": "",
            "team": foreign_team.to_string_lossy(),
            "session_id": "s_outside",
            "created_at": "2026-02-11T09:14:22.531Z",
        }),
    );

    // 无索引行：走 `index_from_marker`，不得补行、不得留实例。
    let raw = env
        .hub
        .ensure_open(&work_dir, None)
        .await
        .err()
        .expect("团队路径不一致必须拒绝打开");
    let err = open::map_tool_error(raw, "ws_unknown");
    assert_eq!(err.code(), "team_path_mismatch", "{err}");
    assert_eq!(
        env.count_workspaces(&work_dir).await,
        0,
        "路径不一致不得建索引行"
    );
    assert!(
        env.hub.get(&work_dir).is_none(),
        "失败不得在 Hub 里留下实例"
    );

    // 有索引行：`resolve_ws_id` 只看身份字段，打开时仍必须被拦下。
    let ws_id = env.resolve(&work_dir, "x").await.unwrap();
    assert_eq!(ws_id, "ws_s_outside");
    let err = env.open(&ws_id).await.err().expect("打开必须失败");
    assert_eq!(err.code(), "team_path_mismatch", "{err}");
    assert!(env.hub.get(&work_dir).is_none(), "失败后 Hub 不得缓存实例");
    assert!(
        err.to_string().contains("teams/x"),
        "文案必须点明 daemon 侧的团队根: {err}"
    );
}
