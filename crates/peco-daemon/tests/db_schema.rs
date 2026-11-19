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

/// 定时任务的两张表在全新库里由 `schema.sql` 一次建全（不靠迁移 029 补）。
///
/// 迁移 029 的门控只看「`scheduled_runs` 在不在」：`schema.sql` 漏写时全新库要靠
/// 029 建表才勉强可用，而两张表的 CHECK 与唯一索引只要有一处不同形，新旧库就会
/// 对同一条表达式给出不同的结论。故这里既查表也查那条部分唯一索引。
#[tokio::test]
async fn empty_db_has_the_schedule_tables() {
    let env = new_env(&[]).await;

    for table in ["scheduled_tasks", "scheduled_runs"] {
        assert!(table_exists(&env.pool, table).await, "表 {table} 应已建出");
    }
    assert!(
        index_exists(&env.pool, "uq_scheduled_run_slot").await,
        "同一槽位只能有一行 cron run，唯一索引必须建在全新库里"
    );
    assert!(index_exists(&env.pool, "idx_scheduled_due").await);
    assert!(index_exists(&env.pool, "idx_scheduled_runs_by_schedule").await);
    // notify_attempt_at 是后加的列：漏在 schema.sql 里，全新库的通知重试会永久卡住。
    let columns = table_columns(&env.pool, "scheduled_runs").await;
    for column in [
        "notify_state",
        "notify_attempts",
        "notify_attempt_at",
        "notified_at",
    ] {
        assert!(
            columns.contains(&column.to_string()),
            "scheduled_runs 缺列 {column}"
        );
    }
}

/// `schema.sql` 的文本必须在 `-- @after-migrations` 之前声明这两张表：放在分隔线
/// 之后会让旧库先跑 029 再建表，两边 DDL 一旦不同形就再也收敛不了。
#[test]
fn schema_sql_declares_the_schedule_tables_before_the_marker() {
    let schema = include_str!("../src/db/schema.sql");
    let (before, after) = schema
        .split_once("-- @after-migrations")
        .expect("schema.sql 必须有迁移分界线");
    for table in ["scheduled_tasks", "scheduled_runs"] {
        assert!(
            before.contains(&format!("CREATE TABLE IF NOT EXISTS {table} (")),
            "{table} 必须在迁移前段落建出"
        );
        assert!(
            !after.contains(&format!("CREATE TABLE IF NOT EXISTS {table} (")),
            "{table} 不得只在迁移后段落出现"
        );
    }
    assert!(
        before.contains("uq_scheduled_run_slot"),
        "槽位唯一索引属于新库的一次性建全"
    );
}

/// P1-2 附带：迁移可重复执行（daemon 每次启动都会跑一遍）。
#[tokio::test]
async fn migrations_are_rerunnable_without_duplicating_seed() {
    let env = new_env(&[]).await;
    peco_daemon::db::run_migrations(&env.pool).await.unwrap();
    assert_eq!(env.count_users().await, 1, "重跑迁移不得插入第二个用户");
}

/// 全新库的 `teams` 表自带 icon 列。
#[tokio::test]
async fn teams_table_has_icon_column() {
    let env = new_env(&[]).await;

    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('teams') WHERE name = 'icon'",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();

    assert_eq!(count, 1, "teams 必须含 icon 列");
}

/// 023 为旧库补 icon 列且保留原有行。
#[tokio::test]
async fn migration_023_adds_team_icon_to_legacy_db_without_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}/legacy.db?mode=rwc", dir.path().display());
    let pool = peco_daemon::db::connect(&url).await.unwrap();

    sqlx::raw_sql(
        "CREATE TABLE teams (\
           id TEXT PRIMARY KEY, display_name TEXT NOT NULL, \
           template_version INTEGER NOT NULL DEFAULT 0, \
           created_at TEXT NOT NULL DEFAULT (datetime('now'))); \
         INSERT INTO teams (id, display_name, template_version) \
         VALUES ('research', '研究组', 3);",
    )
    .execute(&pool)
    .await
    .unwrap();

    peco_daemon::db::run_migrations(&pool).await.unwrap();

    let icon_col = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('teams') WHERE name = 'icon'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(icon_col, 1);

    let row: (String, i64, String) = sqlx::query_as(
        "SELECT display_name, template_version, icon FROM teams WHERE id = 'research'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, ("研究组".to_string(), 3, String::new()));
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
    assert_agent_ui_columns_absent(&pool).await;

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
    assert_agent_ui_columns_absent(&pool).await;
}

/// agents 的 UI 列已全部移除；行数据不受影响，且迁移可重跑。
#[tokio::test]
async fn migration_024_025_drop_agent_ui_columns() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}/post018.db?mode=rwc", dir.path().display());
    let pool = peco_daemon::db::connect(&url).await.unwrap();

    // 018 之后的形状：已有 team_id，但仍带两列 UI 字段。
    sqlx::raw_sql(
        "CREATE TABLE agents (\
           id TEXT PRIMARY KEY, user_id TEXT NOT NULL, team_id TEXT NOT NULL DEFAULT '', \
           name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '', \
           icon TEXT NOT NULL DEFAULT '🤖', background_color TEXT NOT NULL DEFAULT '', \
           status TEXT NOT NULL DEFAULT 'idle', \
           created_at TEXT NOT NULL DEFAULT (datetime('now')), \
           updated_at TEXT NOT NULL DEFAULT (datetime('now')), \
           UNIQUE(team_id, name))",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        "INSERT INTO agents (id, user_id, team_id, name, description, icon, background_color) \
         VALUES ('a1', 'local', 'research', 'reviewer', '代码审查员 — 看代码', '🔍', '#fff')",
    )
    .execute(&pool)
    .await
    .unwrap();

    peco_daemon::db::run_migrations(&pool).await.unwrap();
    assert_agent_ui_columns_absent(&pool).await;

    let kept = sqlx::query_as::<_, (String, String)>(
        "SELECT name, description FROM agents WHERE id = 'a1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kept, ("reviewer".into(), "代码审查员 — 看代码".into()));

    // 重跑：门控命中，列形状与行都不变。
    peco_daemon::db::run_migrations(&pool).await.unwrap();
    assert_agent_ui_columns_absent(&pool).await;
}

async fn assert_agent_ui_columns_absent(pool: &sqlx::SqlitePool) {
    let ui_cols = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name IN ('icon', 'background_color')",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(ui_cols, 0, "agents 不应再有 icon / background_color 列");
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
        "status TEXT NOT NULL DEFAULT 'idle'",
        "idx_knowledge_bases_team",
        "idx_conversations_workspace_agent",
        "-- @after-migrations",
    ] {
        assert!(schema.contains(needle), "schema.sql 必须含 {needle}");
    }

    // agents 的 UI 字段已移除；teams.icon 仍在，所以只断言 agents 的形状。
    assert!(
        !schema.contains("background_color"),
        "schema.sql 不应再声明 agents.background_color"
    );
    assert!(
        !schema.contains("icon TEXT NOT NULL DEFAULT '🤖'"),
        "schema.sql 不应再声明 agents.icon"
    );
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
        schema.contains("    prompt_digest   TEXT"),
        "d_dispatches 建表语句必须带 prompt_digest 列（新库不靠迁移补）"
    );
    assert!(
        schema.contains("    output          TEXT"),
        "d_dispatches 建表语句必须带 output 列（新库不靠迁移补）"
    );
    assert!(
        schema.contains("    output_truncated INTEGER NOT NULL DEFAULT 0"),
        "d_dispatches 建表语句必须带 output_truncated 列"
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

async fn table_columns(pool: &sqlx::SqlitePool, table: &str) -> Vec<String> {
    sqlx::query_scalar(&format!(
        "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
    ))
    .fetch_all(pool)
    .await
    .unwrap()
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

async fn table_exists(pool: &sqlx::SqlitePool, name: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap()
        > 0
}

async fn row_count(pool: &sqlx::SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM \"{table}\""))
        .fetch_one(pool)
        .await
        .unwrap()
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

/// 旧库（本改动之前的版本）没有空间唯一会话索引：启动迁移必须补上，且必须是
/// **部分**唯一索引 —— 只管 `scope='space'`，普通 agent 会话照样能并列多条。
///
/// 变异：删掉 021 的注册 → 索引不在，第一条断言变红；把 `WHERE scope='space'`
/// 写掉 → 两条 `scope='agent'` 插不进去，第二条断言变红。
#[tokio::test]
async fn migration_021_adds_the_partial_unique_space_session_index() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}/pre021.db?mode=rwc", dir.path().display());
    let pool = peco_daemon::db::connect(&url).await.unwrap();
    peco_daemon::db::run_migrations(&pool).await.unwrap();
    // 旧库的样子：表在，索引不在。
    sqlx::raw_sql("DROP INDEX idx_conversations_space_unique")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !index_exists(&pool, "idx_conversations_space_unique").await,
        "夹具需要一份缺索引的库"
    );

    peco_daemon::db::run_migrations(&pool).await.unwrap();

    assert!(
        index_exists(&pool, "idx_conversations_space_unique").await,
        "021 必须给旧库补出空间会话唯一索引"
    );

    for id in ["c_agent_1", "c_agent_2"] {
        sqlx::query(
            "INSERT INTO conversations (id, user_id, workspace_id, agent_name, scope) \
             VALUES (?, 'local', 'ws_a', '@master', 'agent')",
        )
        .bind(id)
        .execute(&pool)
        .await
        .expect("普通 agent 会话不受这条索引约束");
    }

    sqlx::raw_sql(
        "INSERT INTO conversations (id, user_id, workspace_id, agent_name, scope) \
         VALUES ('c_space_1', 'local', 'ws_a', '@master', 'space')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let duplicate = sqlx::raw_sql(
        "INSERT INTO conversations (id, user_id, workspace_id, agent_name, scope) \
         VALUES ('c_space_2', 'local', 'ws_a', '@master', 'space')",
    )
    .execute(&pool)
    .await
    .expect_err("同空间同 agent 的第二条 scope='space' 必须被唯一索引拦下");
    assert!(
        duplicate.to_string().contains("UNIQUE"),
        "错误应是唯一约束: {duplicate}"
    );

    // 重跑迁移：守卫命中，索引与数据都还在。
    peco_daemon::db::run_migrations(&pool).await.unwrap();
    assert!(index_exists(&pool, "idx_conversations_space_unique").await);
}

/// 旧库（022 之前）没有 prompt_digest：启动迁移补上，且列门控让重跑安全。
///
/// 变异：删掉 022 的注册 ⇒ 第一条断言红；把门控写成「总是执行」⇒ 旧库补列后
/// 第二次 `run_migrations` 报 duplicate column name，最后一步失败。
#[tokio::test]
async fn migration_022_adds_the_prompt_digest_column_and_is_reentrant() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}/pre022.db?mode=rwc", dir.path().display());
    let pool = peco_daemon::db::connect(&url).await.unwrap();
    peco_daemon::db::run_migrations(&pool).await.unwrap();
    // 造出 022 之前的库形状：列不在，行要在。
    sqlx::query(
        "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, \
         target_team_id, origin_ws_id, agent_name, origin_kind, task_state, wake_state) \
         VALUES ('d1', 't1', 'ws_a', '/tmp/a', 'research', 'ws_peco', '@master', \
         'run_machine_task', 'working', 'pending')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql("ALTER TABLE d_dispatches DROP COLUMN prompt_digest")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !column_names(&pool)
            .await
            .contains(&"prompt_digest".to_string()),
        "夹具需要一份缺列的库"
    );

    peco_daemon::db::run_migrations(&pool).await.unwrap();

    let columns = column_names(&pool).await;
    assert!(
        columns.contains(&"prompt_digest".to_string()),
        "022 必须把缺的列补上：{columns:?}"
    );
    let digest = sqlx::query_scalar::<_, Option<String>>(
        "SELECT prompt_digest FROM d_dispatches WHERE id = 'd1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(digest, None, "补列不得给历史行编造描述");

    // 再跑一次：列门控命中，形状不变（ALTER 本身不可重入）。
    peco_daemon::db::run_migrations(&pool).await.unwrap();
    assert_eq!(column_names(&pool).await, columns);
}

/// 造一张 026 之前的 `d_dispatches`：唤醒态五档、没有 `dispatch_id`。
///
/// 行覆盖「已投递」「已跳过（带原因）」「还在等终态」三种唤醒态 —— 重建表的搬行
/// 语句要么把它们原样搬过去，要么就会在这三条断言上显形。
async fn pre_026_dispatch_ledger(pool: &sqlx::SqlitePool) {
    for statement in [
        "CREATE TABLE d_dispatches (\
           id TEXT PRIMARY KEY, task_id TEXT NOT NULL, target_ws_id TEXT NOT NULL, \
           target_work_dir TEXT NOT NULL, target_team_id TEXT NOT NULL, \
           origin_ws_id TEXT NOT NULL, agent_name TEXT NOT NULL, origin_kind TEXT NOT NULL, \
           task_state TEXT NOT NULL, wake_state TEXT NOT NULL DEFAULT 'pending' \
             CHECK (wake_state IN ('pending','delivering','delivered','skipped','failed')), \
           wake_skip_reason TEXT, wake_attempts INTEGER NOT NULL DEFAULT 0, \
           wake_claimed_at TEXT, delivered_at TEXT, result_digest TEXT, prompt_digest TEXT, \
           created_at TEXT NOT NULL DEFAULT (datetime('now')), terminal_at TEXT, \
           UNIQUE (target_ws_id, task_id))",
        "CREATE INDEX idx_dispatches_wake ON d_dispatches(wake_state, wake_claimed_at)",
        "CREATE INDEX idx_dispatches_task_state ON d_dispatches(target_ws_id, task_state)",
        "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, \
           target_team_id, origin_ws_id, agent_name, origin_kind, task_state, wake_state, \
           delivered_at, result_digest) \
         VALUES ('d1', 't1', 'ws_a', '/tmp/a', 'research', 'ws_peco', '@master', \
           'run_machine_task', 'completed', 'delivered', datetime('now'), '重构完成')",
        "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, \
           target_team_id, origin_ws_id, agent_name, origin_kind, task_state, wake_state, \
           wake_skip_reason) \
         VALUES ('d2', 't2', 'ws_a', '/tmp/a', 'research', 'ws_own', '@master', \
           'run_machine_task', 'working', 'skipped', 'not_peco_origin')",
        "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, \
           target_team_id, origin_ws_id, agent_name, origin_kind, task_state, wake_state) \
         VALUES ('d3', 't3', 'ws_a', '/tmp/a', 'research', 'ws_peco', '@master', \
           'run_machine_task', 'working', 'pending')",
    ] {
        sqlx::raw_sql(statement).execute(pool).await.unwrap();
    }
}

/// 旧库（026 之前）升级：已有行、唤醒态与跳过原因原样保留，`dispatch_id` 为
/// NULL，重建出的表收 `awaiting`。
///
/// 变异：搬行时漏列 / 搬错列 ⇒ 行的三列断言红；新建表照抄旧 CHECK ⇒ 写
/// `awaiting` 那步报 constraint failed；门控写成「总是执行」⇒ 第二次
/// `run_migrations` 把刚写进去的 `delivered` 行搬丢，最后的形状断言红。
#[tokio::test]
async fn migration_026_rebuilds_the_ledger_and_keeps_every_row() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}/pre026.db?mode=rwc", dir.path().display());
    let pool = peco_daemon::db::connect(&url).await.unwrap();
    pre_026_dispatch_ledger(&pool).await;
    assert!(
        !column_names(&pool)
            .await
            .contains(&"dispatch_id".to_string()),
        "夹具需要一份缺 dispatch_id 的库"
    );

    peco_daemon::db::run_migrations(&pool).await.unwrap();

    let columns = column_names(&pool).await;
    assert!(
        columns.contains(&"dispatch_id".to_string()),
        "026 必须给旧库补出 dispatch_id：{columns:?}"
    );

    let rows = sqlx::query_as::<_, (String, String, String, Option<String>, Option<String>)>(
        "SELECT id, task_state, wake_state, wake_skip_reason, dispatch_id \
         FROM d_dispatches ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "d1".to_string(),
                "completed".to_string(),
                "delivered".to_string(),
                None,
                None
            ),
            (
                "d2".to_string(),
                "working".to_string(),
                "skipped".to_string(),
                Some("not_peco_origin".to_string()),
                None
            ),
            (
                "d3".to_string(),
                "working".to_string(),
                "pending".to_string(),
                None,
                None
            ),
        ],
        "搬行不得改值，dispatch_id 无旧值可搬一律 NULL"
    );
    let delivered_at: Option<String> =
        sqlx::query_scalar("SELECT delivered_at FROM d_dispatches WHERE id = 'd1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(delivered_at.is_some(), "搬行不得丢送达时间戳");

    // 新 CHECK 收 awaiting：迁移后旧库也该能写这一态。
    sqlx::query("UPDATE d_dispatches SET wake_state = 'awaiting' WHERE id = 'd3'")
        .execute(&pool)
        .await
        .expect("026 之后 wake_state 必须收 awaiting");

    // 重建出的表必须仍带 `UNIQUE (target_ws_id, task_id)`：台账的幂等写入靠
    // `ON CONFLICT(target_ws_id, task_id) DO NOTHING` 吃掉重复行，约束一丢，
    // 这条 INSERT 自己就会报 no matching constraint（或落下第二行）。
    let duplicate = sqlx::query(
        "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, target_team_id, \
         origin_ws_id, agent_name, origin_kind, task_state) \
         VALUES ('d4', 't1', 'ws_a', '/tmp/a', 'research', 'ws_peco', '@master', \
         'run_machine_task', 'completed') \
         ON CONFLICT(target_ws_id, task_id) DO NOTHING",
    )
    .execute(&pool)
    .await
    .expect("026 重建后的表必须满足 ON CONFLICT(target_ws_id, task_id)");
    assert_eq!(
        duplicate.rows_affected(),
        0,
        "同一 (target_ws_id, task_id) 的重复行必须被 DO NOTHING 吃掉"
    );

    assert!(index_exists(&pool, "idx_dispatches_wake").await);
    assert!(index_exists(&pool, "idx_dispatches_task_state").await);

    // 再跑一次：门控命中，形状与数据都不变。
    peco_daemon::db::run_migrations(&pool).await.unwrap();
    assert_eq!(column_names(&pool).await, columns);
    let still_awaiting: String =
        sqlx::query_scalar("SELECT wake_state FROM d_dispatches WHERE id = 'd3'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(still_awaiting, "awaiting", "重跑迁移不得重建表");
}

/// 027/028 给旧库补出正文两列，历史行留 `NULL` / `0`：不能替它们编一段正文。
///
/// 变异点：把历史行的 `output_truncated` 默认成 1 或写空串 ⇒ 两条断言红；门控
/// 写成「总是执行」⇒ 第二次 `run_migrations` 撞 duplicate column name。
#[tokio::test]
async fn migration_027_adds_the_body_columns_without_touching_historical_rows() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}/pre027.db?mode=rwc", dir.path().display());
    let pool = peco_daemon::db::connect(&url).await.unwrap();
    // 026 之前的库同样缺 027 的两列，且 026 会先把它重建成新形状。
    pre_026_dispatch_ledger(&pool).await;
    assert!(
        !column_names(&pool).await.contains(&"output".to_string()),
        "夹具需要一份没有 output 的库"
    );

    peco_daemon::db::run_migrations(&pool).await.unwrap();

    let columns = column_names(&pool).await;
    assert!(
        columns.contains(&"output".to_string()),
        "027 必须补出 output：{columns:?}"
    );
    assert!(
        columns.contains(&"output_truncated".to_string()),
        "028 必须补出 output_truncated：{columns:?}"
    );
    let rows = sqlx::query_as::<_, (String, Option<String>, i64)>(
        "SELECT id, output, output_truncated FROM d_dispatches ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("d1".to_string(), None, 0),
            ("d2".to_string(), None, 0),
            ("d3".to_string(), None, 0),
        ],
        "历史行没有正文可搬：留 NULL，且不得声称被截断"
    );

    // 再跑一次：列门控命中，形状不变（ALTER 本身不可重入）。
    peco_daemon::db::run_migrations(&pool).await.unwrap();
    assert_eq!(column_names(&pool).await, columns);
}

/// 崩在 027 与 028 两条 ALTER 之间：下一次启动必须只补缺的那一列。
///
/// 这一格之前是「唤醒永久失效」的根：门控只看 `output` 就把 028 一起跳过，
/// `output_truncated` 永远缺失，`finish` 与台账查询双双报 no such column ——
/// `awaiting → pending` 再也写不出去。
///
/// 变异点：028 的门控改成查 `output` ⇒ 「两列都在」断言红（缺
/// `output_truncated`），后面的 `finish` 直接报 no such column。
#[tokio::test]
async fn migration_028_heals_a_db_left_between_the_two_alter_statements() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}/half027.db?mode=rwc", dir.path().display());
    let pool = peco_daemon::db::connect(&url).await.unwrap();
    pre_026_dispatch_ledger(&pool).await;
    peco_daemon::db::run_migrations(&pool).await.unwrap();

    // 复现崩点：027 的 ALTER 已生效，028 的还没跑。列在场时删掉它（全新库由
    // schema.sql 一次建全）；不在场就说明门控已经把 028 跳过了，现场已经成立。
    if column_names(&pool)
        .await
        .contains(&"output_truncated".to_string())
    {
        sqlx::raw_sql("ALTER TABLE d_dispatches DROP COLUMN output_truncated")
            .execute(&pool)
            .await
            .unwrap();
    }
    let half = column_names(&pool).await;
    assert!(half.contains(&"output".to_string()), "夹具需要 027 已生效");
    assert!(
        !half.contains(&"output_truncated".to_string()),
        "夹具需要 028 未执行：{half:?}"
    );

    peco_daemon::db::run_migrations(&pool).await.unwrap();

    let columns = column_names(&pool).await;
    assert!(
        columns.contains(&"output".to_string()),
        "自愈后 output 必须在：{columns:?}"
    );
    assert!(
        columns.contains(&"output_truncated".to_string()),
        "自愈后 output_truncated 必须在：{columns:?}"
    );

    // 两列都在，写入侧才跑得动：缺列时这条 UPDATE 报 no such column。
    let affected = peco_daemon::db::dispatches::finish(
        &pool,
        "t3",
        "completed",
        Some("摘要"),
        Some("正文"),
        false,
    )
    .await
    .unwrap();
    assert_eq!(affected, 1, "半迁移库上 finish 必须可执行");

    // 读端同理：正文与截断标志都按列名读。
    let ledger = peco_daemon::space::ledger::SqliteTaskLedger::new(pool.clone());
    let row =
        peco_core::tools::TaskLedger::find_terminal(&ledger, std::path::Path::new("/tmp/a"), "t3")
            .await
            .unwrap()
            .expect("台账读必须可执行");
    assert_eq!(row.output.as_deref(), Some("正文"));
    assert!(!row.output_truncated, "落库时没截断");
}

/// 把库复现成「026 崩在删旧表与改名之间」的现场：残表装着唯一一份台账，正式表没了。
///
/// 直接跑 026 文件里逐条语句、跑到 `DROP TABLE d_dispatches` 就停 —— 崩点与生产
/// 逐条执行的顺序同源，夹具不会随 SQL 改动而漂移。
async fn crash_026_between_drop_and_rename(pool: &sqlx::SqlitePool) {
    let sql = include_str!("../src/db/migrations/026_dispatch_awaiting_wake.sql");
    for statement in sql.split(';') {
        let statement = statement.trim();
        if statement.is_empty()
            || statement
                .lines()
                .all(|line| line.trim().is_empty() || line.trim().starts_with("--"))
        {
            continue;
        }
        sqlx::raw_sql(statement).execute(pool).await.unwrap();
        if statement.starts_with("DROP TABLE d_dispatches") {
            return;
        }
    }
    panic!("026 的 SQL 里必须有一条 DROP TABLE d_dispatches");
}

/// 崩在「删旧表」与「改名」之间：重启后残表必须升格回正式表，而不是被
/// `schema.sql` 先建出的空壳和 026 的门控一起埋掉。
///
/// 变异：删掉 `run_versioned_migrations` 里的 `recover_interrupted_026` ⇒ 门控
/// 看到空壳表的 `dispatch_id` 就跳过 026，行数断言读到 0 行、`d_dispatches_new`
/// 也还在，三条断言同时变红。
#[tokio::test]
async fn migration_026_promotes_the_leftover_table_when_the_rename_was_lost() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}/crash026.db?mode=rwc", dir.path().display());
    let pool = peco_daemon::db::connect(&url).await.unwrap();
    pre_026_dispatch_ledger(&pool).await;
    crash_026_between_drop_and_rename(&pool).await;
    assert!(
        !table_exists(&pool, "d_dispatches").await,
        "崩点之后正式表不该还在"
    );
    assert_eq!(
        row_count(&pool, "d_dispatches_new").await,
        3,
        "夹具要让残表装着唯一一份台账"
    );

    peco_daemon::db::run_migrations(&pool).await.unwrap();

    assert_eq!(
        row_count(&pool, "d_dispatches").await,
        3,
        "残表里的行必须被提升为正式表的内容"
    );
    assert!(
        !table_exists(&pool, "d_dispatches_new").await,
        "升格之后残表名不该还在"
    );
    assert!(
        column_names(&pool)
            .await
            .contains(&"dispatch_id".to_string()),
        "升格来的表已是 026 的新形状"
    );
    assert!(index_exists(&pool, "idx_dispatches_wake").await);
    assert!(index_exists(&pool, "idx_dispatches_task_state").await);

    // 升格来的表收 `awaiting`：形状与 026 建出来的表一致。
    sqlx::query("UPDATE d_dispatches SET wake_state = 'awaiting' WHERE id = 'd3'")
        .execute(&pool)
        .await
        .expect("026 的 CHECK 必须随残表一起被提升");

    // 门控这次仍要收敛到「不再动表」：重跑一遍不多不少。
    peco_daemon::db::run_migrations(&pool).await.unwrap();
    assert_eq!(row_count(&pool, "d_dispatches").await, 3);
}
