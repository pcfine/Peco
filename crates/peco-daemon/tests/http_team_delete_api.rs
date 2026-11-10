//! `DELETE /api/teams/{team_id}`。

mod support;

use peco_daemon::db::teams;
use peco_daemon::db::workspaces;
use peco_daemon::space::peco::PECO_TEAM_ID;
use support::new_env;

/// 无绑定空间时删除目录与索引行。
#[tokio::test]
async fn delete_team_without_spaces_removes_row_and_directory() {
    let env = new_env(&["research", "writing"]).await;

    let reply = env.delete("/api/teams/research").await;

    assert_eq!(reply.status, 204, "body={}", reply.body);
    assert!(
        teams::get(&env.pool, "research").await.unwrap().is_none(),
        "索引行必须删除"
    );
    assert!(!env.home.join("teams/research").exists(), "目录必须删除");
    assert!(
        env.home.join("teams/writing").is_dir(),
        "只删目标团队，别的团队原样保留"
    );
    assert_eq!(env.get("/api/teams/research").await.status, 404);
    assert!(
        !env.get("/api/teams")
            .await
            .body
            .to_string()
            .contains("research"),
        "清单不得再出现已删团队"
    );
}

/// 有绑定空间时只删空间行，保留工作目录。
#[tokio::test]
async fn delete_team_removes_bound_spaces_but_keeps_work_dirs() {
    let env = new_env(&["research"]).await;
    let alpha = env.open_space("alpha", "research").await;
    let beta = env.open_space("beta", "research").await;
    let alpha_dir = env.home.join("spaces/alpha/work");
    let beta_dir = env.home.join("spaces/beta/work");
    assert!(alpha_dir.join(".workspace.json").is_file());
    assert!(beta_dir.join(".workspace.json").is_file());

    let reply = env.delete("/api/teams/research").await;

    assert_eq!(reply.status, 204, "body={}", reply.body);
    assert!(
        workspaces::list_by_team(&env.pool, "research")
            .await
            .unwrap()
            .is_empty(),
        "绑定空间行必须随团队删除"
    );
    assert!(
        alpha_dir.join(".workspace.json").is_file(),
        "工作目录与 marker 属于用户数据，不得删除"
    );
    assert!(beta_dir.join(".workspace.json").is_file());

    let spaces = env.get("/api/spaces").await;
    let ids: Vec<&str> = spaces
        .body
        .as_array()
        .expect("空间清单应是数组")
        .iter()
        .map(|row| row["ws_id"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&alpha.as_str()), "失效空间不得继续出现");
    assert!(!ids.contains(&beta.as_str()));
}

/// 不能删除 Peco。
#[tokio::test]
async fn delete_reserved_peco_team_is_400_and_untouched() {
    let env = new_env(&[PECO_TEAM_ID]).await;

    let reply = env.delete(&format!("/api/teams/{PECO_TEAM_ID}")).await;

    assert_eq!(reply.status, 400, "body={}", reply.body);
    assert_eq!(reply.code(), "invalid_argument");
    assert!(
        env.home.join("teams").join(PECO_TEAM_ID).is_dir(),
        "被拒的删除不得动现有目录"
    );
}

/// 非法 id 返回 400，不存在返回 404。
#[tokio::test]
async fn delete_invalid_or_missing_team() {
    let env = new_env(&["research"]).await;

    for bad in ["Writing", "a", "with%20space", "research2%21", "%2E%2E"] {
        let reply = env.delete(&format!("/api/teams/{bad}")).await;
        assert_eq!(reply.status, 400, "id={bad} body={}", reply.body);
        assert_eq!(reply.code(), "invalid_argument");
    }
    assert!(
        env.home.join("teams/research").is_dir(),
        "非法请求不得触碰任何目录"
    );

    let missing = env.delete("/api/teams/ghost").await;
    assert_eq!(missing.status, 404, "body={}", missing.body);
    assert_eq!(missing.code(), "team_not_found");
}

/// 目录-only 团队可删。
#[tokio::test]
async fn delete_directory_only_team() {
    let env = new_env(&["solo"]).await;
    assert!(teams::get(&env.pool, "solo").await.unwrap().is_none());

    let reply = env.delete("/api/teams/solo").await;

    assert_eq!(reply.status, 204, "body={}", reply.body);
    assert!(!env.home.join("teams/solo").exists());
}

/// 团队作用域索引一并清理。
#[tokio::test]
async fn delete_team_clears_team_scoped_indexes() {
    let env = new_env(&["research"]).await;
    for sql in [
        "INSERT INTO agents (id, user_id, team_id, name) \
         VALUES ('agent_1', 'local', 'research', 'writer')",
        "INSERT INTO knowledge_bases (id, user_id, team_id, name) \
         VALUES ('kb_1', 'local', 'research', 'notes')",
        "INSERT INTO documents (id, kb_id, filename, filepath) \
         VALUES ('doc_1', 'kb_1', 'a.md', '/tmp/a.md')",
    ] {
        sqlx::query(sql).execute(&env.pool).await.unwrap();
    }

    let reply = env.delete("/api/teams/research").await;
    assert_eq!(reply.status, 204, "body={}", reply.body);

    for sql in [
        "SELECT COUNT(*) FROM agents WHERE team_id = 'research'",
        "SELECT COUNT(*) FROM knowledge_bases WHERE team_id = 'research'",
        "SELECT COUNT(*) FROM documents",
        "SELECT COUNT(*) FROM teams WHERE id = 'research'",
    ] {
        let count: i64 = sqlx::query_scalar(sql).fetch_one(&env.pool).await.unwrap();
        assert_eq!(count, 0, "残留行：{sql}");
    }
}
