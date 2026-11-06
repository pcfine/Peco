//! 空库装配：单用户种子、三张新表、外键真的生效。

mod support;

use support::new_env;

/// P1-2：空库启动后 `users` 只有一行且 id='local'，三张新表都在。
#[tokio::test]
async fn empty_db_seeds_single_local_user_and_new_tables() {
    let env = new_env(&[]).await;

    assert_eq!(env.count_users().await, 1, "users 必须恰好一行");
    let (id, username, email) =
        sqlx::query_as::<_, (String, String, String)>("SELECT id, username, email FROM users")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(id, "local");
    assert_eq!(username, "local");
    assert_eq!(email, "local@localhost");

    for table in ["teams", "workspaces", "d_dispatches"] {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(table)
        .fetch_one(&env.pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "表 {table} 应已建出");
    }
}

/// P1-2 附带：迁移可重复执行（daemon 每次启动都会跑一遍）。
#[tokio::test]
async fn migrations_are_rerunnable_without_duplicating_seed() {
    let env = new_env(&[]).await;
    peco_daemon::db::run_migrations(&env.pool).await.unwrap();
    assert_eq!(env.count_users().await, 1, "重跑迁移不得插入第二个用户");
}

/// P1-3：`team_id` 指向不存在的团队时外键必须拦下。
#[tokio::test]
async fn workspaces_reject_unknown_team_with_foreign_key_error() {
    let env = new_env(&[]).await;

    let err = sqlx::query(
        "INSERT INTO workspaces (id, work_dir, team_id) VALUES ('ws_x', '/tmp/x', 'nope')",
    )
    .execute(&env.pool)
    .await
    .expect_err("team_id 无对应 teams 行时必须失败");

    assert!(
        err.to_string().contains("FOREIGN KEY"),
        "错误串应含 FOREIGN KEY，实际: {err}"
    );
}

/// `d_dispatches.target_ws_id` 故意没有外键：删空间行之后台账必须还在。
#[tokio::test]
async fn dispatch_ledger_has_no_foreign_key_to_workspaces() {
    let env = new_env(&[]).await;
    peco_daemon::db::teams::upsert(
        &env.pool,
        &peco_daemon::db::teams::NewTeam {
            id: "research",
            display_name: "research",
            template_version: 1,
        },
    )
    .await
    .unwrap();

    // 没有任何 workspaces 行的 target_ws_id 也必须能写入
    let inserted = env
        .insert_dispatch(
            "ws_ghost",
            "t1",
            "dispatch_to_workspace",
            "peco",
            "submitted",
        )
        .await;
    assert_eq!(inserted, 1, "台账行不得被外键拦住");
    assert_eq!(env.count_dispatches("ws_ghost").await, 1);
}

/// 台账的幂等判据：同一 (target_ws_id, task_id) 只允许一行。
#[tokio::test]
async fn dispatch_ledger_dedups_on_target_and_task() {
    let env = new_env(&[]).await;

    assert_eq!(
        env.insert_dispatch(
            "ws_a",
            "task-1",
            "dispatch_to_workspace",
            "peco",
            "submitted"
        )
        .await,
        1
    );
    assert_eq!(
        env.insert_dispatch("ws_a", "task-1", "dispatch_to_workspace", "peco", "working")
            .await,
        0,
        "重复 (target_ws_id, task_id) 必须被 UNIQUE 拦成 0 行"
    );
    assert_eq!(env.count_dispatches("ws_a").await, 1);
}

/// 迁移 018：旧库的 `agents` 重建为按团队唯一，存量行不丢，且可重跑。
///
/// 旧库造的是 018 之前的形状：没有 `team_id`，唯一维度是 `(user_id, name)`
/// （`user_id` 恒为 'local'）。重建后同一名字必须能在两个团队各存一行。
///
/// 变异方法（实测过）：① 把 018 里的 `UNIQUE(team_id, name)` 换回
/// `UNIQUE(user_id, name)` —— 第二个团队的同名行插入报 UNIQUE 失败；
/// ② 删掉 `mod.rs` 的 018 门控（无条件重跑）—— 第二次 `run_migrations`
/// 在 `DROP TABLE agents` 之后的 `ALTER TABLE ... RENAME` 上报错。
#[tokio::test]
async fn migration_018_rebuilds_agents_per_team_and_survives_rerun() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}/legacy.db?mode=rwc", dir.path().display());
    let pool = peco_daemon::db::connect(&url).await.unwrap();

    // 018 之前的三张投影表：agents 的唯一维度是 (user_id, name)，
    // knowledge_bases / conversations 没有团队与空间列。这三张表不带外键，
    // 免得这张手搭的旧库还要先备份 users 行。
    sqlx::raw_sql(
        "CREATE TABLE agents (\
           id TEXT PRIMARY KEY, user_id TEXT NOT NULL, name TEXT NOT NULL, \
           description TEXT NOT NULL DEFAULT '', icon TEXT NOT NULL DEFAULT '', \
           background_color TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'idle', \
           created_at TEXT NOT NULL DEFAULT (datetime('now')), \
           updated_at TEXT NOT NULL DEFAULT (datetime('now')), \
           UNIQUE(user_id, name))",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        "CREATE TABLE knowledge_bases (\
           id TEXT PRIMARY KEY, user_id TEXT NOT NULL, name TEXT NOT NULL, \
           description TEXT NOT NULL DEFAULT '', \
           created_at TEXT NOT NULL DEFAULT (datetime('now')), \
           updated_at TEXT NOT NULL DEFAULT (datetime('now')))\
         ;CREATE TABLE conversations (\
           id TEXT PRIMARY KEY, user_id TEXT NOT NULL, agent_id TEXT, \
           agent_name TEXT NOT NULL DEFAULT 'unknown', title TEXT NOT NULL DEFAULT '', \
           archived_at TEXT, \
           created_at TEXT NOT NULL DEFAULT (datetime('now')), \
           updated_at TEXT NOT NULL DEFAULT (datetime('now')), \
           scope TEXT NOT NULL DEFAULT 'agent')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        "INSERT INTO agents (id, user_id, name, description) \
         VALUES ('a_master', 'local', '@master', 'legacy master')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql("INSERT INTO knowledge_bases (id, user_id, name) VALUES ('kb1', 'local', 'kb')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        "INSERT INTO conversations (id, user_id, agent_name, title) \
         VALUES ('c1', 'local', '@master', 'legacy chat')",
    )
    .execute(&pool)
    .await
    .unwrap();

    peco_daemon::db::run_migrations(&pool).await.unwrap();

    let team_id_col = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'team_id'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(team_id_col, 1, "迁移 018 后 agents 应含 team_id 列");

    let kept =
        sqlx::query_scalar::<_, String>("SELECT description FROM agents WHERE id = 'a_master'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(kept, "legacy master", "重建表不得丢存量行");

    // 唯一维度是 (team_id, name)：两团队同名可以，同团队同名不行。
    for (id, team) in [("a_research", "research"), ("a_design", "design")] {
        sqlx::query(
            "INSERT INTO agents (id, user_id, team_id, name) VALUES (?, 'local', ?, '@master')",
        )
        .bind(id)
        .bind(team)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("团队 {team} 的同名 agent 应能插入: {e}"));
    }
    let dup = sqlx::query(
        "INSERT INTO agents (id, user_id, team_id, name) VALUES ('a_dup', 'local', 'research', '@master')",
    )
    .execute(&pool)
    .await;
    assert!(dup.is_err(), "同团队同名 agent 必须被唯一约束拦下");

    // 重跑迁移：门控命中则跳过，行数与列形状都不变。
    peco_daemon::db::run_migrations(&pool).await.unwrap();
    let rows = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agents")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 3, "重跑迁移不得重建表或丢行");
}

/// 全新库的列形状：`schema.sql` 必须自带团队/空间维度，否则 018 的列门控
/// （查 `agents.team_id`）对「`schema.sql` 漏加」是瞎的。
#[test]
fn schema_sql_declares_team_and_workspace_scope() {
    let schema = include_str!("../src/db/schema.sql");
    for needle in [
        "team_id TEXT NOT NULL DEFAULT ''",
        "workspace_id TEXT NOT NULL DEFAULT ''",
        "UNIQUE(team_id, name)",
        "idx_knowledge_bases_team",
        "idx_conversations_workspace_agent",
        "-- @after-migrations",
    ] {
        assert!(schema.contains(needle), "schema.sql 必须含 {needle}");
    }
}
