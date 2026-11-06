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

/// `schema.sql` 必须自带两套状态列与新索引名。
///
/// 019/020 的列守卫对「`schema.sql` 漏改」是瞎的：全新库照样能收敛，只是白跑
/// 一次 DDL，而迁移前段落若仍建旧索引，旧库会在迁移前就报 `no such column`。
#[test]
fn schema_sql_declares_task_state_and_wake_skip_reason() {
    let schema = include_str!("../src/db/schema.sql");
    let (before_migrations, after_migrations) = schema
        .split_once("-- @after-migrations")
        .expect("schema.sql 必须有迁移分界");

    assert!(
        schema.contains("    task_state      TEXT NOT NULL,"),
        "d_dispatches 建表语句必须带 task_state 列"
    );
    assert!(
        schema.contains("    wake_skip_reason TEXT"),
        "d_dispatches 建表语句必须带 wake_skip_reason 列"
    );
    assert!(
        !before_migrations.contains("idx_dispatches_state"),
        "迁移前段落不得建旧索引：旧库此时还没有 task_state 列"
    );
    assert!(
        after_migrations.contains("DROP INDEX IF EXISTS idx_dispatches_state"),
        "旧索引名要在迁移后显式丢弃"
    );
    assert!(
        after_migrations.contains("idx_dispatches_task_state"),
        "新索引建在迁移后段落"
    );
}

/// 造一张 019 之前的 `d_dispatches`：`state` 列、旧索引，两行数据（一行历史
/// `skipped`）。
async fn legacy_dispatch_ledger(pool: &sqlx::SqlitePool) {
    for statement in [
        "CREATE TABLE d_dispatches (\
           id TEXT PRIMARY KEY, task_id TEXT NOT NULL, target_ws_id TEXT NOT NULL, \
           target_work_dir TEXT NOT NULL, target_team_id TEXT NOT NULL, \
           origin_ws_id TEXT NOT NULL, agent_name TEXT NOT NULL, origin_kind TEXT NOT NULL, \
           state TEXT NOT NULL, wake_state TEXT NOT NULL DEFAULT 'pending', \
           wake_attempts INTEGER NOT NULL DEFAULT 0, wake_claimed_at TEXT, delivered_at TEXT, \
           result_digest TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now')), \
           terminal_at TEXT, UNIQUE (target_ws_id, task_id))",
        "CREATE INDEX idx_dispatches_wake ON d_dispatches(wake_state, wake_claimed_at)",
        "CREATE INDEX idx_dispatches_state ON d_dispatches(target_ws_id, state)",
        "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, \
           target_team_id, origin_ws_id, agent_name, origin_kind, state, wake_state) \
         VALUES ('d1', 't1', 'ws_a', '/tmp/a', 'research', 'ws_peco', '@master', \
           'run_machine_task', 'completed', 'pending')",
        "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, \
           target_team_id, origin_ws_id, agent_name, origin_kind, state, wake_state) \
         VALUES ('d2', 't2', 'ws_a', '/tmp/a', 'research', 'ws_peco', '@master', \
           'run_machine_task', 'working', 'skipped')",
    ] {
        sqlx::raw_sql(statement).execute(pool).await.unwrap();
    }
}

async fn column_names(pool: &sqlx::SqlitePool) -> Vec<String> {
    sqlx::query_scalar("SELECT name FROM pragma_table_info('d_dispatches') ORDER BY cid")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn index_exists(pool: &sqlx::SqlitePool, name: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap()
        > 0
}

/// daemon 启动对旧库跑一遍 019/020：列改名不丢行，历史 `skipped` 行留空原因，
/// 旧名索引换成新名索引（`RENAME COLUMN` 只改定义不改名字）。
#[tokio::test]
async fn migration_019_and_020_upgrade_a_legacy_dispatch_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}/legacy.db?mode=rwc", dir.path().display());
    let pool = peco_daemon::db::connect(&url).await.unwrap();
    legacy_dispatch_ledger(&pool).await;

    peco_daemon::db::run_migrations(&pool).await.unwrap();

    let columns = column_names(&pool).await;
    assert!(
        columns.contains(&"task_state".to_string()),
        "019 后应有 task_state：{columns:?}"
    );
    assert!(
        !columns.contains(&"state".to_string()),
        "019 后不得残留旧列 state：{columns:?}"
    );
    assert!(
        columns.contains(&"wake_skip_reason".to_string()),
        "020 后应有 wake_skip_reason：{columns:?}"
    );

    let rows = sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT id, task_state, wake_skip_reason FROM d_dispatches ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("d1".to_string(), "completed".to_string(), None),
            ("d2".to_string(), "working".to_string(), None),
        ],
        "改名不得丢行或改值；历史跳过行的成因不可考，留空"
    );

    assert!(
        !index_exists(&pool, "idx_dispatches_state").await,
        "旧索引名必须被丢弃，否则同一列会带着两个索引名"
    );
    assert!(
        index_exists(&pool, "idx_dispatches_task_state").await,
        "迁移后段落必须建出新索引"
    );

    // 再跑一次：两个守卫都命中，形状不变。
    peco_daemon::db::run_migrations(&pool).await.unwrap();
    assert_eq!(column_names(&pool).await, columns);
}

/// 「019 已成功、020 没跑」的半迁移库：列守卫只补 reason 列，不重建表、不编造原因。
#[tokio::test]
async fn migration_020_completes_a_half_migrated_dispatch_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}/half.db?mode=rwc", dir.path().display());
    let pool = peco_daemon::db::connect(&url).await.unwrap();
    peco_daemon::db::run_migrations(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, \
         target_team_id, origin_ws_id, agent_name, origin_kind, task_state, wake_state) \
         VALUES ('d1', 't1', 'ws_a', '/tmp/a', 'research', 'ws_peco', '@master', \
         'run_machine_task', 'working', 'skipped')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql("ALTER TABLE d_dispatches DROP COLUMN wake_skip_reason")
        .execute(&pool)
        .await
        .unwrap();

    peco_daemon::db::run_migrations(&pool).await.unwrap();

    assert!(
        column_names(&pool)
            .await
            .contains(&"wake_skip_reason".to_string()),
        "020 必须把缺的列补上"
    );
    let reason = sqlx::query_scalar::<_, Option<String>>(
        "SELECT wake_skip_reason FROM d_dispatches WHERE id = 'd1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(reason, None, "补列不得给历史行编造原因");
}
