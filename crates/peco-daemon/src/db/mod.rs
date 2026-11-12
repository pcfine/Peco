// ============================================================================
// 数据库连接池与迁移
// ============================================================================

pub mod agents;
pub mod artifacts;
pub mod compaction_log;
pub mod context_shadow;
pub mod conversations;
pub mod dispatch_state;
pub mod dispatches;
pub mod documents;
pub mod knowledge_bases;
pub mod memory_audit;
pub mod memory_consolidation_optin;
pub mod memory_consolidation_state;
pub mod memory_recall_stats;
pub mod memory_supersede;
pub mod messages;
pub mod session_archive;
pub mod teams;
pub mod workflow_executions;
pub mod workflow_schedules;
pub mod workspaces;

use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

/// 从数据库 URL 创建连接池。
///
/// WAL 与 foreign_keys 走连接选项而非 `PRAGMA`：`PRAGMA` 是连接级的，
/// 在池上执行只作用于当时借到的那一条连接，后续新连接上没有外键约束 ——
/// 「插入不存在的 team 必须失败」这类判定会随机失效。
pub async fn connect(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new().connect_with(options).await?;
    tracing::info!("SQLite connection pool established");
    Ok(pool)
}

/// `schema.sql` 的执行分界：这一行之后的语句等版本化迁移跑完再执行。
///
/// 建在新列上的索引依赖迁移给旧库补出的列：迁移前建会在旧库上报
/// `no such column`，而 schema.sql 又要为新库一次建全（含这些索引）。
const SCHEMA_AFTER_MIGRATIONS: &str = "-- @after-migrations";

/// 运行 DDL 迁移，创建所有表和索引。
///
/// 使用 `IF NOT EXISTS` 确保幂等 — 重复执行不会报错。
pub async fn run_migrations(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let schema = include_str!("schema.sql");
    let (before_migrations, after_migrations) = schema
        .split_once(SCHEMA_AFTER_MIGRATIONS)
        .unwrap_or((schema, ""));

    run_sql_file(pool, before_migrations).await?;
    run_versioned_migrations(pool).await?;
    run_sql_file(pool, after_migrations).await?;

    tracing::info!("Database migrations completed successfully");
    Ok(())
}

/// 按 `;` 拆分逐条执行，跳过空语句与纯注释（`sqlx::raw_sql` 只吃单条语句）。
///
/// 一份 SQL 的全部语句走同一条连接：迁移 018 的重建要跨「建表 → 搬行 → 删表
/// → 改名」，若中途被连接池换到另一条连接，那条连接的 schema 缓存仍认为旧表
/// 存在，改名会报 `there is already another table or index with this name`。
async fn run_sql_file(pool: &SqlitePool, sql: &str) -> Result<(), sqlx::Error> {
    let mut conn = pool.acquire().await?;
    for statement in sql.split(';') {
        let trimmed = statement.trim();
        if trimmed.is_empty()
            || trimmed
                .lines()
                .all(|l| l.trim().is_empty() || l.trim().starts_with("--"))
        {
            continue;
        }
        sqlx::raw_sql(trimmed).execute(&mut *conn).await?;
    }
    Ok(())
}

/// 执行版本化 SQL 迁移（`migrations/` 目录下的 SQL 文件）。
///
/// 每个迁移在执行前检查前置条件，若已满足则跳过。
async fn run_versioned_migrations(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    // Migration 002: Slim agents table
    // 检查旧列是否存在（有 config_json 列说明需要迁移）
    let has_old_schema = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'config_json'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if has_old_schema {
        tracing::info!("Running migration 002: slim agents table");
        let migration_sql = include_str!("migrations/002_slim_agents.sql");
        for statement in migration_sql.split(';') {
            let trimmed = statement.trim();
            if trimmed.is_empty()
                || trimmed
                    .lines()
                    .all(|l| l.trim().is_empty() || l.trim().starts_with("--"))
            {
                continue;
            }
            sqlx::raw_sql(trimmed).execute(pool).await?;
        }
        tracing::info!("Migration 002 completed");
    } else {
        tracing::debug!("Migration 002 skipped: agents table already slim");
    }

    // ── Migration 004: agents background_color ────────────────────────────
    // 004 只为 018 的表重建补列：018 会 `SELECT ... background_color` 搬行。
    // 025 之后该列被删除，若无条件按「缺列就补」执行，每次启动都会补回再删掉。
    // 因此只在 018 尚未执行（没有 team_id）的旧库上补。
    let has_bg_color = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'background_color'",
    )
    .fetch_one(pool)
    .await?
        > 0;
    let agents_has_team_id = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'team_id'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_bg_color && !agents_has_team_id {
        tracing::info!("Running migration 004: agents background_color");
        let migration_sql = include_str!("migrations/004_agent_background.sql");
        for statement in migration_sql.split(';') {
            let trimmed = statement.trim();
            if trimmed.is_empty()
                || trimmed
                    .lines()
                    .all(|l| l.trim().is_empty() || l.trim().starts_with("--"))
            {
                continue;
            }
            sqlx::raw_sql(trimmed).execute(pool).await?;
        }
        tracing::info!("Migration 004 completed");
    } else {
        tracing::debug!("Migration 004 skipped: background_color exists or agents.team_id present");
    }

    // ── Migration 003: conversations v2 (agent_name + archived_at) ──────────
    let has_agent_name = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('conversations') WHERE name = 'agent_name'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_agent_name {
        tracing::info!("Running migration 003: conversations v2");
        let migration_sql = include_str!("migrations/003_conversations_v2.sql");
        for statement in migration_sql.split(';') {
            let trimmed = statement.trim();
            if trimmed.is_empty()
                || trimmed
                    .lines()
                    .all(|l| l.trim().is_empty() || l.trim().starts_with("--"))
            {
                continue;
            }
            sqlx::raw_sql(trimmed).execute(pool).await?;
        }
        tracing::info!("Migration 003 completed");
    } else {
        tracing::debug!("Migration 003 skipped: agent_name column already exists");
    }

    // ── Migration 005: Workflow 管理模块 ──────────────────────────────────
    let has_workflow_executions = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='workflow_executions'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    // 检查是否还有旧 task 表需要清理（首次迁移时处理）
    let has_old_tasks = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='tasks'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_workflow_executions || has_old_tasks {
        tracing::info!("Running migration 005: workflow management module");
        let migration_sql = include_str!("migrations/005_workflow_executions.sql");
        for statement in migration_sql.split(';') {
            let trimmed = statement.trim();
            if trimmed.is_empty()
                || trimmed
                    .lines()
                    .all(|l| l.trim().is_empty() || l.trim().starts_with("--"))
            {
                continue;
            }
            sqlx::raw_sql(trimmed).execute(pool).await?;
        }
        tracing::info!("Migration 005 completed: task tables dropped, workflow tables created");
    } else {
        tracing::debug!(
            "Migration 005 skipped: workflow_executions table already exists and no old tasks table"
        );
    }

    // ── Migration 006: drop agents.color (unused theme color) ────────────
    let has_color = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'color'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if has_color {
        tracing::info!("Running migration 006: drop agents.color");
        let migration_sql = include_str!("migrations/006_drop_agent_color.sql");
        for statement in migration_sql.split(';') {
            let trimmed = statement.trim();
            if trimmed.is_empty()
                || trimmed
                    .lines()
                    .all(|l| l.trim().is_empty() || l.trim().starts_with("--"))
            {
                continue;
            }
            sqlx::raw_sql(trimmed).execute(pool).await?;
        }
        tracing::info!("Migration 006 completed");
    } else {
        tracing::debug!("Migration 006 skipped: color column already absent");
    }

    // ── Migration 007: Peco 压缩日志表 ────────────────────────────────────
    let has_compaction_log = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='peco_compaction_log'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_compaction_log {
        run_migration(
            pool,
            "007",
            include_str!("migrations/007_peco_compaction_log.sql"),
        )
        .await?;
        tracing::info!("Migration 007 completed");
    } else {
        tracing::debug!("Migration 007 skipped: peco_compaction_log table already exists");
    }

    // ── Migration 008: Peco 会话归档表 ────────────────────────────────────
    let has_session_archives = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='peco_session_archives'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_session_archives {
        run_migration(
            pool,
            "008",
            include_str!("migrations/008_peco_session_archives.sql"),
        )
        .await?;
        tracing::info!("Migration 008 completed");
    } else {
        tracing::debug!("Migration 008 skipped: peco_session_archives table already exists");
    }

    // ── Migration 009: 记忆巩固基建（删除审计 / 召回统计 / 整理水位）──────
    // 门控检查迁移文件中的最后一张表：run_migration 逐条语句执行、无事务，
    // 进程在部分执行后崩溃时，查首表会误判为已完成而跳过余下表；
    // 查末表 + 文件内全部 IF NOT EXISTS 保证部分执行后可安全重跑（同迁移 005）。
    let has_memory_consolidation_state = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memory_consolidation_state'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_memory_consolidation_state {
        run_migration(
            pool,
            "009",
            include_str!("migrations/009_peco_memory_consolidation.sql"),
        )
        .await?;
        tracing::info!("Migration 009 completed");
    } else {
        tracing::debug!("Migration 009 skipped: memory_consolidation_state table already exists");
    }

    // ── Migration 010: 自动整理用户级 opt-in 开关 ──────────────────────────
    let has_consolidation_optin = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memory_consolidation_optin'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_consolidation_optin {
        run_migration(
            pool,
            "010",
            include_str!("migrations/010_peco_memory_consolidation_optin.sql"),
        )
        .await?;
        tracing::info!("Migration 010 completed");
    } else {
        tracing::debug!("Migration 010 skipped: memory_consolidation_optin table already exists");
    }

    // ── Migration 011: 记忆取代 shadow 观测表 ─────────────────────────────
    let has_supersede_shadow = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memory_supersede_shadow'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_supersede_shadow {
        run_migration(
            pool,
            "011",
            include_str!("migrations/011_peco_memory_supersede_shadow.sql"),
        )
        .await?;
        tracing::info!("Migration 011 completed");
    } else {
        tracing::debug!("Migration 011 skipped: memory_supersede_shadow table already exists");
    }

    // ── Migration 012: 记忆取代 intent WAL + audit 扩展 ────────────────────
    // 门控查 intent 表（迁移文件最后创建的表，「表在即视为已迁」——
    // ALTER TABLE ADD COLUMN 不可重复执行，门控必须保证整块只跑一次）
    let has_supersede_intent = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memory_supersede_intent'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_supersede_intent {
        run_migration(
            pool,
            "012",
            include_str!("migrations/012_peco_memory_supersede_intent.sql"),
        )
        .await?;
        tracing::info!("Migration 012 completed");
    } else {
        tracing::debug!("Migration 012 skipped: memory_supersede_intent table already exists");
    }

    // ── Migration 013: 上下文压缩边界观测表 ───────────────────────────────
    let has_context_shadow = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='peco_context_shadow'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_context_shadow {
        run_migration(
            pool,
            "013",
            include_str!("migrations/013_peco_context_shadow.sql"),
        )
        .await?;
        tracing::info!("Migration 013 completed");
    } else {
        tracing::debug!("Migration 013 skipped: peco_context_shadow table already exists");
    }

    // ── Migration 014: documents 表 manifest 投影列 + 唯一索引 ─────────────
    // 门控查 relative_path（ALTER ADD COLUMN 不可重复执行）。
    let documents_has_relative_path = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('documents') WHERE name = 'relative_path'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !documents_has_relative_path {
        run_migration(
            pool,
            "014",
            include_str!("migrations/014_documents_manifest.sql"),
        )
        .await?;
        tracing::info!("Migration 014 completed");
    } else {
        tracing::debug!("Migration 014 skipped: documents.relative_path already exists");
    }

    // ── Migration 015: artifacts 表 ───────────────────────────────────────
    // 表存在则跳过，与其余迁移一致 —— 否则每次启动都会喊「正在迁移」。
    // 必须同时判**唯一索引**：`run_migration` 逐条执行且无事务，中途失败会留下
    // 「表在、索引不在」的库。此时只看表会永久 skip，而 `db::artifacts::insert`
    // 的 `ON CONFLICT(conversation_id, rel_path)` 依赖该索引，会每次都报错，
    // 表现为「产物永远不出现」。
    let has_artifacts = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'artifacts'",
    )
    .fetch_one(pool)
    .await?
        > 0;
    let has_artifact_unique_idx = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_artifacts_conv_path'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_artifacts || !has_artifact_unique_idx {
        run_migration(pool, "015", include_str!("migrations/015_artifacts.sql")).await?;
        tracing::info!("Migration 015 completed");
    } else {
        tracing::debug!("Migration 015 skipped: artifacts table already exists");
    }

    // ── Migration 016: conversations.scope ────────────────────────────────
    // 门控查列：ALTER ADD COLUMN 不可重复执行（重跑报 duplicate column name）。
    let has_scope = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('conversations') WHERE name = 'scope'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_scope {
        run_migration(
            pool,
            "016",
            include_str!("migrations/016_conversation_scope.sql"),
        )
        .await?;
        tracing::info!("Migration 016 completed");
    } else {
        tracing::debug!("Migration 016 skipped: scope column already exists");
    }

    // ── Migration 017: memory_audit.kb_scope ──────────────────────────────
    // 门控查列：ALTER ADD COLUMN 不可重复执行。查 `kb_scope` 而不是表：
    // 表由迁移 009 建出，只看表会让 017 永久 skip。
    let has_kb_scope = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('memory_audit') WHERE name = 'kb_scope'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_kb_scope {
        run_migration(
            pool,
            "017",
            include_str!("migrations/017_daemon_kb_scope.sql"),
        )
        .await?;
        tracing::info!("Migration 017 completed");
    } else {
        tracing::debug!("Migration 017 skipped: kb_scope column already exists");
    }

    // ── Migration 018: 投影表的 team / workspace 维度 ─────────────────────
    // 门控查 `agents.team_id`：全新库由 schema.sql 带出该列 → 跳过；旧库缺失
    // 才需要重建 agents（表级 UNIQUE 无法用 ALTER 删除）。看列而不是看表 ——
    // agents 表一直存在，只看表会让 018 永久 skip。
    if !agents_has_team_id {
        run_migration(
            pool,
            "018",
            include_str!("migrations/018_team_and_workspace_scope.sql"),
        )
        .await?;
        tracing::info!("Migration 018 completed");
    } else {
        tracing::debug!("Migration 018 skipped: agents.team_id already exists");
    }

    // ── Migration 019: d_dispatches.state → task_state ────────────────────
    // RENAME COLUMN 不可重跑（第二次报 no such column）。两列同时在场说明库被
    // 手工改过：改名会失败或丢数据，故只 warn 不动表，让人去看。
    let has_legacy_state = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('d_dispatches') WHERE name = 'state'",
    )
    .fetch_one(pool)
    .await?
        > 0;
    let has_task_state = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('d_dispatches') WHERE name = 'task_state'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if has_legacy_state && !has_task_state {
        run_migration(
            pool,
            "019",
            include_str!("migrations/019_rename_dispatch_state.sql"),
        )
        .await?;
        tracing::info!("Migration 019 completed");
    } else if has_legacy_state && has_task_state {
        tracing::warn!("d_dispatches has both state and task_state; leaving the table untouched");
    } else {
        tracing::debug!("Migration 019 skipped: d_dispatches.task_state already exists");
    }

    // ── Migration 020: d_dispatches.wake_skip_reason ──────────────────────
    // 门控查列：ALTER ADD COLUMN 不可重复执行。
    let has_wake_skip_reason = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('d_dispatches') WHERE name = 'wake_skip_reason'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_wake_skip_reason {
        run_migration(
            pool,
            "020",
            include_str!("migrations/020_dispatch_wake_skip_reason.sql"),
        )
        .await?;
        tracing::info!("Migration 020 completed");
    } else {
        tracing::debug!("Migration 020 skipped: d_dispatches.wake_skip_reason already exists");
    }

    // ── Migration 021: 空间唯一会话的部分唯一索引 ─────────────────────────
    // 门控查索引而不管 SQL 里的 `IF NOT EXISTS`：逐条执行无事务，进程崩在
    // 建索引前后都会留下「表在、索引不在」的库，看索引才自愈；命中也避免每次
    // 启动都记一条「正在迁移」。
    let has_space_session_idx = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type = 'index' AND name = 'idx_conversations_space_unique'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_space_session_idx {
        run_migration(
            pool,
            "021",
            include_str!("migrations/021_space_conversation.sql"),
        )
        .await?;
        tracing::info!("Migration 021 completed");
    } else {
        tracing::debug!("Migration 021 skipped: space session index already exists");
    }

    // ── Migration 022: d_dispatches.prompt_digest ─────────────────────────
    // 门控查列：ALTER ADD COLUMN 不可重复执行（重跑报 duplicate column name）。
    let has_prompt_digest = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('d_dispatches') WHERE name = 'prompt_digest'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_prompt_digest {
        run_migration(
            pool,
            "022",
            include_str!("migrations/022_dispatch_prompt_digest.sql"),
        )
        .await?;
        tracing::info!("Migration 022 completed");
    } else {
        tracing::debug!("Migration 022 skipped: d_dispatches.prompt_digest already exists");
    }

    // ── Migration 023: teams.icon ─────────────────────────────────────────
    let teams_has_icon = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('teams') WHERE name = 'icon'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !teams_has_icon {
        run_migration(pool, "023", include_str!("migrations/023_team_icon.sql")).await?;
        tracing::info!("Migration 023 completed");
    } else {
        tracing::debug!("Migration 023 skipped: teams.icon already exists");
    }

    // ── Migration 024: drop agents.icon ───────────────────────────────────
    let agents_has_icon = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'icon'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if agents_has_icon {
        run_migration(
            pool,
            "024",
            include_str!("migrations/024_drop_agent_icon.sql"),
        )
        .await?;
        tracing::info!("Migration 024 completed");
    } else {
        tracing::debug!("Migration 024 skipped: agents.icon already absent");
    }

    // ── Migration 025: drop agents.background_color ───────────────────────
    let agents_has_bg_color = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'background_color'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if agents_has_bg_color {
        run_migration(
            pool,
            "025",
            include_str!("migrations/025_drop_agent_background_color.sql"),
        )
        .await?;
        tracing::info!("Migration 025 completed");
    } else {
        tracing::debug!("Migration 025 skipped: agents.background_color already absent");
    }

    // ── 026 的中断恢复：残表 d_dispatches_new ─────────────────────────────
    // 必须赶在门控之前：迁移逐条执行且没有事务，崩在「删旧表」与「改名」之间时，
    // 下次启动 `schema.sql` 会先建出空的正式表，门控看到 `dispatch_id` 就跳过
    // 026，唯一一份台账便永远留在残表里。
    recover_interrupted_026(pool).await?;

    // ── Migration 026: d_dispatches 重建（awaiting 唤醒态 + dispatch_id）────
    // 门控查 `dispatch_id`：全新库由 schema.sql 直接建全 → 跳过；旧库缺这一列
    // 才需要重建（表级 CHECK 无法用 ALTER 改）。看列而不是看 wake_state 的取值，
    // 行里没有 awaiting 的老库同样要补这一列。
    let has_dispatch_id = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('d_dispatches') WHERE name = 'dispatch_id'",
    )
    .fetch_one(pool)
    .await?
        > 0;
    // 两列同时在场的库是手工改过的（019 已 warn 并拒绝改名）。这里同样不动它：
    // 重建要把 `task_state` 搬进 NOT NULL 列，而那一列在这些行里是 NULL —— 搬
    // 过去只能替用户编一个状态，或者直接让启动失败。
    let ledger_is_hand_edited = has_legacy_state && has_task_state;

    if !has_dispatch_id && !ledger_is_hand_edited {
        run_migration(
            pool,
            "026",
            include_str!("migrations/026_dispatch_awaiting_wake.sql"),
        )
        .await?;
        tracing::info!("Migration 026 completed");
    } else if has_dispatch_id {
        tracing::debug!("Migration 026 skipped: d_dispatches.dispatch_id already exists");
    } else {
        // 拒绝启动会把这类库的用户挡在门外，只能放开启动并说清后果：台账的每条
        // 查询与写入都带 dispatch_id，缺列的库上一行都读不出来、也写不进去。
        tracing::warn!(
            "d_dispatches has both state and task_state; skipping migration 026, so dispatch_id \
             stays missing: every ledger read and write will fail until that column is added by \
             hand (ALTER TABLE d_dispatches ADD COLUMN dispatch_id TEXT)"
        );
    }

    Ok(())
}

/// 026 被中断在「删旧表」与「改名」之间时，把残表升格回正式表。
///
/// 那一步之后的库有两个特征：正式表为空（`schema.sql` 刚建出的空壳，或旧表已被
/// 删掉），`d_dispatches_new` 装着唯一一份台账。残表已按 026 的新形状建好，故
/// 升格只需重复 026 尾部的动作，不再搬一次行。
///
/// 正式表非空时不猜谁对：两边都留着，把残表名与行数写进日志交给人判断 —— 静默
/// 删除唯一可能正确的数据，比留一张看得见的残表糟得多。
async fn recover_interrupted_026(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let Some(leftover_rows) = count_rows_if_table(pool, "d_dispatches_new").await? else {
        return Ok(());
    };
    let live_rows = count_rows_if_table(pool, "d_dispatches")
        .await?
        .unwrap_or(0);

    if live_rows > 0 {
        tracing::warn!(
            leftover_table = "d_dispatches_new",
            leftover_rows,
            live_rows,
            "d_dispatches_new was left behind by an interrupted 026; the live table already \
             holds rows, so nothing is merged into it — check by hand before dropping the leftover"
        );
        return Ok(());
    }

    run_sql_file(pool, PROMOTE_LEFTOVER_026).await?;
    tracing::warn!(
        leftover_table = "d_dispatches_new",
        rows = leftover_rows,
        "migration 026 was interrupted between DROP and RENAME; the leftover table was promoted \
         back to d_dispatches"
    );
    Ok(())
}

/// 026 尾部的三个动作，与那份 SQL 分开写：残表里已经装好了行，整份重跑只会卡在
/// 「建表不带 IF NOT EXISTS」那一步。
const PROMOTE_LEFTOVER_026: &str = "\
DROP TABLE IF EXISTS d_dispatches;
ALTER TABLE d_dispatches_new RENAME TO d_dispatches;
CREATE INDEX IF NOT EXISTS idx_dispatches_wake ON d_dispatches(wake_state, wake_claimed_at);
CREATE INDEX IF NOT EXISTS idx_dispatches_task_state ON d_dispatches(target_ws_id, task_state)";

/// 表存在时返回它的行数，不存在返回 `None`。
async fn count_rows_if_table(pool: &SqlitePool, table: &str) -> Result<Option<i64>, sqlx::Error> {
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
    )
    .bind(table)
    .fetch_one(pool)
    .await?
        > 0;
    if !exists {
        return Ok(None);
    }
    sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM \"{table}\""))
        .fetch_one(pool)
        .await
        .map(Some)
}

/// 执行一段幂等迁移 SQL（按 `;` 拆分逐条执行，跳过空语句与纯注释）。
async fn run_migration(
    pool: &SqlitePool,
    name: &str,
    migration_sql: &str,
) -> Result<(), sqlx::Error> {
    tracing::info!("Running migration {name}");
    run_sql_file(pool, migration_sql).await
}

// ── Server Config (键值对) ───────────────────────────────────────────────────

/// 从 `server_config` 表读取指定 key 的值。
pub async fn get_server_config(
    pool: &SqlitePool,
    key: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT value FROM server_config WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
}

/// 写入/更新 `server_config` 表中的键值对。
pub async fn set_server_config(
    pool: &SqlitePool,
    key: &str,
    value: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT OR REPLACE INTO server_config (key, value, updated_at) VALUES (?, ?, datetime('now'))",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = connect(&url).await.unwrap();
        run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    /// 迁移 009：首次执行建出三张记忆巩固表；已有数据在二次执行后保持不变
    /// （前置存在性检查命中 → 跳过，不重建、不清空）。
    #[tokio::test]
    async fn migration_009_creates_tables_once_and_survives_rerun() {
        let (pool, _dir) = test_pool().await;

        for table in [
            "memory_audit",
            "memory_recall_stats",
            "memory_consolidation_state",
        ] {
            let count = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?",
            )
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(count, 1, "表 {table} 应已由迁移 009 创建");
        }

        // 写入一行审计数据后重跑迁移，数据与表数量必须保持不变
        sqlx::query(
            "INSERT INTO memory_audit \
             (user_id, kb_name, doc_id, title, content, source, reason, deleted_by, deleted_at) \
             VALUES ('u1', '@private_memory', 'doc-1', 't', 'c', 'ppa_semantic', \
             'manual_organize', 'agent:@memory', '2026-09-10T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        run_migrations(&pool).await.unwrap();

        let audit_rows = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memory_audit")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(audit_rows, 1, "重跑迁移不得清空已有审计数据");

        for table in [
            "memory_audit",
            "memory_recall_stats",
            "memory_consolidation_state",
        ] {
            let count = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?",
            )
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(count, 1, "重跑迁移后表 {table} 不得重复创建");
        }
    }

    /// 迁移 010：首次执行建出 opt-in 表；已有数据在二次执行后保持不变
    /// （前置存在性检查命中 → 跳过，不重建、不清空）。
    #[tokio::test]
    async fn migration_010_creates_table_once_and_survives_rerun() {
        let (pool, _dir) = test_pool().await;

        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?",
        )
        .bind("memory_consolidation_optin")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            count, 1,
            "表 memory_consolidation_optin 应已由迁移 010 创建"
        );

        // 写入一行 opt-in 记录后重跑迁移，数据与表数量必须保持不变
        sqlx::query(
            "INSERT INTO memory_consolidation_optin (user_id, enabled, opted_in_at, updated_at) \
             VALUES ('u1', 1, '2026-09-10T00:00:00+00:00', '2026-09-10T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        run_migrations(&pool).await.unwrap();

        let optin_rows = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM memory_consolidation_optin WHERE enabled = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(optin_rows, 1, "重跑迁移不得清空已有 opt-in 记录");

        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?",
        )
        .bind("memory_consolidation_optin")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            count, 1,
            "重跑迁移后表 memory_consolidation_optin 不得重复创建"
        );
    }

    /// 迁移 011：首次执行建出 shadow 观测表；已有数据在二次执行后保持不变
    /// （前置存在性检查命中 → 跳过，不重建、不清空）。
    #[tokio::test]
    async fn migration_011_creates_table_once_and_survives_rerun() {
        let (pool, _dir) = test_pool().await;

        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?",
        )
        .bind("memory_supersede_shadow")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "表 memory_supersede_shadow 应已由迁移 011 创建");

        // 写入一行 shadow 记录后重跑迁移，数据与表数量必须保持不变
        sqlx::query(
            "INSERT INTO memory_supersede_shadow \
             (user_id, created_at, candidates_json, facts_json, decisions_json) \
             VALUES ('u1', '2026-10-01T00:00:00+00:00', '[]', '[]', '{}')",
        )
        .execute(&pool)
        .await
        .unwrap();

        run_migrations(&pool).await.unwrap();

        let shadow_rows =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memory_supersede_shadow")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(shadow_rows, 1, "重跑迁移不得清空已有 shadow 记录");

        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?",
        )
        .bind("memory_supersede_shadow")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            count, 1,
            "重跑迁移后表 memory_supersede_shadow 不得重复创建"
        );
    }

    /// 迁移 012：首次执行建出 intent WAL 表并扩展 memory_audit 列；
    /// 已有数据在二次执行后保持不变（ALTER 不可重复，靠门控整块跳过）。
    #[tokio::test]
    async fn migration_012_creates_table_once_and_survives_rerun() {
        let (pool, _dir) = test_pool().await;

        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?",
        )
        .bind("memory_supersede_intent")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "表 memory_supersede_intent 应已由迁移 012 创建");

        // audit 新列就位（topic_key / successor_doc_id）
        for col in ["topic_key", "successor_doc_id"] {
            let n = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pragma_table_info('memory_audit') WHERE name = ?",
            )
            .bind(col)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(n, 1, "memory_audit 应含新列 {col}");
        }

        // 写入一行 intent 数据后重跑迁移，数据与表数量必须保持不变
        sqlx::query(
            "INSERT INTO memory_supersede_intent \
             (user_id, kb_name, old_doc_id, old_title, old_source, \
              new_doc_id, new_title, new_content, new_source, created_at, updated_at) \
             VALUES ('u1', '@private_memory', 'old-1', 't-old', 'ppa_semantic', \
             'new-1', 't-new', '新事实', 'ppa_semantic', \
             '2026-10-01T00:00:00+00:00', '2026-10-01T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        run_migrations(&pool).await.unwrap();

        let intent_rows =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memory_supersede_intent")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(intent_rows, 1, "重跑迁移不得清空已有 intent 数据");

        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?",
        )
        .bind("memory_supersede_intent")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            count, 1,
            "重跑迁移后表 memory_supersede_intent 不得重复创建"
        );
    }

    /// 迁移 014：补齐投影列与唯一索引；旧库升级后仍可启动且不丢存量行。
    #[tokio::test]
    async fn migration_014_upgrades_legacy_documents_without_data_loss() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/legacy.db?mode=rwc", dir.path().display());
        let pool = connect(&url).await.unwrap();

        // 造一个投影列之前的旧库，含一行 upload 记录
        sqlx::raw_sql(
            "CREATE TABLE documents (\
               id TEXT PRIMARY KEY, kb_id TEXT NOT NULL, filename TEXT NOT NULL, \
               filepath TEXT NOT NULL, file_size INTEGER NOT NULL DEFAULT 0, \
               mime_type TEXT NOT NULL DEFAULT 'application/octet-stream', \
               status TEXT NOT NULL DEFAULT 'pending', error_msg TEXT, \
               created_at TEXT NOT NULL DEFAULT (datetime('now')))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO documents (id, kb_id, filename, filepath) \
             VALUES ('d1', 'kb1', 'a.md', '/x/a.md')",
        )
        .execute(&pool)
        .await
        .unwrap();

        run_migrations(&pool).await.unwrap();

        for col in ["relative_path", "doc_id"] {
            let n = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pragma_table_info('documents') WHERE name = ?",
            )
            .bind(col)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(n, 1, "迁移 014 后 documents 应含列 {col}");
        }
        let idx = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_documents_kb_relative'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            idx, 1,
            "迁移 014 后应存在唯一索引 idx_documents_kb_relative"
        );

        let rows = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM documents")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "迁移不得删除存量 documents 行");
    }

    /// 迁移 013：首次执行建出边界观测表；已有数据在二次执行后保持不变。
    #[tokio::test]
    async fn migration_013_creates_table_once_and_survives_rerun() {
        let (pool, _dir) = test_pool().await;

        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?",
        )
        .bind("peco_context_shadow")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "表 peco_context_shadow 应已由迁移 013 创建");

        sqlx::query(
            "INSERT INTO peco_context_shadow \
             (id, user_id, conversation_id, turn_index, created_at, trigger_caliber, \
              pinned_tokens, committed_full_tokens, committed_view_tokens, view_window_tokens, \
              fold_triggered, evicted_turns, guard_triggered, strip_evaluated, strip_enforced, \
              stripped_items, released_tokens, tool_output_items, tool_output_over_cap, \
              image_output_items, image_base64_bytes, snapshot_json_bytes) \
             VALUES ('s-1', 'u1', 'u1-private-session', 3, '2026-10-01T00:00:00+00:00', 'full', \
              100, 200, 150, 120, 1, 2, 0, 1, 0, 4, 50, 6, 2, 0, 0, 1024)",
        )
        .execute(&pool)
        .await
        .unwrap();

        run_migrations(&pool).await.unwrap();

        let rows = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM peco_context_shadow")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "重跑迁移不得清空已有观测数据");

        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?",
        )
        .bind("peco_context_shadow")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "重跑迁移后表 peco_context_shadow 不得重复创建");
    }

    /// 造一张 019 之前的 `d_dispatches`：`state` 列 + 旧索引，附带两行数据。
    async fn legacy_dispatch_ledger(pool: &SqlitePool) {
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

    async fn column_names(pool: &SqlitePool, table: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT name FROM pragma_table_info(?) ORDER BY cid")
            .bind(table)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn has_index(pool: &SqlitePool, name: &str) -> bool {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name = ?",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
            > 0
    }

    /// 019 + 020 对旧库的整条升级：列改名不丢行，历史 `skipped` 行留空原因，
    /// 旧名索引换成新名索引（`RENAME COLUMN` 只改定义不改名字）。
    #[tokio::test]
    async fn migration_019_and_020_upgrade_a_legacy_ledger_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/legacy.db?mode=rwc", dir.path().display());
        let pool = connect(&url).await.unwrap();
        legacy_dispatch_ledger(&pool).await;

        run_migrations(&pool).await.unwrap();

        let columns = column_names(&pool, "d_dispatches").await;
        assert!(
            columns.contains(&"task_state".to_string()),
            "019 后应有 task_state：{columns:?}"
        );
        assert!(
            !columns.contains(&"state".to_string()),
            "019 后 state 不得残留：{columns:?}"
        );
        assert!(
            columns.contains(&"wake_skip_reason".to_string()),
            "020 后应有 wake_skip_reason：{columns:?}"
        );

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM d_dispatches")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2,
            "改名不得丢行"
        );
        let historical_reason = sqlx::query_scalar::<_, Option<String>>(
            "SELECT wake_skip_reason FROM d_dispatches WHERE id = 'd2'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(historical_reason, None, "历史跳过行的成因不可考，留空");

        assert!(
            !has_index(&pool, "idx_dispatches_state").await,
            "旧索引名必须被丢弃，否则同一列会有两个索引名"
        );
        assert!(
            has_index(&pool, "idx_dispatches_task_state").await,
            "迁移后段必须建出 task_state 索引"
        );

        // 020 的 CHECK 与枚举必须同集合：枚举多一个值而迁移漏改，写入只会在
        // 运行期报 constraint failed
        for reason in dispatch_state::WakeSkipReason::ALL {
            sqlx::query("UPDATE d_dispatches SET wake_skip_reason = ? WHERE id = 'd1'")
                .bind(reason.as_str())
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("迁移后的库不认识原因 {}：{e}", reason.as_str()));
        }
        assert!(
            sqlx::query("UPDATE d_dispatches SET wake_skip_reason = 'made_up' WHERE id = 'd1'")
                .execute(&pool)
                .await
                .is_err(),
            "CHECK 必须拒绝清单外的原因"
        );

        // 再跑一次：所有守卫都命中，仍是同一形状
        run_migrations(&pool).await.unwrap();
        assert_eq!(column_names(&pool, "d_dispatches").await, columns);
    }

    /// 「只有 task_state 没有 reason」的半迁移库：020 的列守卫补列，不重建表。
    #[tokio::test]
    async fn migration_020_completes_a_half_migrated_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/half.db?mode=rwc", dir.path().display());
        let pool = connect(&url).await.unwrap();
        // 先建全新形状，再把 reason 列摘掉 —— 模拟 019 成功、020 未执行的库
        run_migrations(&pool).await.unwrap();
        sqlx::query(
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

        run_migrations(&pool).await.unwrap();

        assert!(
            column_names(&pool, "d_dispatches")
                .await
                .contains(&"wake_skip_reason".to_string())
        );
        let reason = sqlx::query_scalar::<_, Option<String>>(
            "SELECT wake_skip_reason FROM d_dispatches WHERE id = 'd1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(reason, None, "补列不得给历史行编造原因");
    }

    /// 两列同时在场的库（手工改过）：019 只 warn 不动表 —— 改名会失败，硬来会丢数据。
    #[tokio::test]
    async fn migration_019_leaves_a_ledger_with_both_columns_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/both.db?mode=rwc", dir.path().display());
        let pool = connect(&url).await.unwrap();
        legacy_dispatch_ledger(&pool).await;
        sqlx::raw_sql("ALTER TABLE d_dispatches ADD COLUMN task_state TEXT")
            .execute(&pool)
            .await
            .unwrap();

        run_migrations(&pool).await.unwrap();

        let columns = column_names(&pool, "d_dispatches").await;
        assert!(columns.contains(&"state".to_string()), "旧列必须原样留下");
        assert!(
            columns.contains(&"task_state".to_string()),
            "新列不得被改名覆盖"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM d_dispatches WHERE state = 'working'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1,
            "旧列数据不得被搬动"
        );
    }

    /// `schema.sql` 的全新库形状必须自带 `task_state` / `wake_skip_reason` /
    /// `dispatch_id` 与六档唤醒态，且不得在迁移前段落建 `state` 索引：019/020/026
    /// 的门控对「`schema.sql` 漏改」都是瞎的 —— 漏了也照样能迁移收敛，只是全新库
    /// 会白跑一次 DDL，而 026 之后新库还得再重建一遍表。
    #[test]
    fn schema_sql_declares_the_dispatch_state_columns() {
        let schema = include_str!("schema.sql");
        assert!(
            schema.contains("    task_state      TEXT NOT NULL,"),
            "d_dispatches 建表语句必须带 task_state 列"
        );
        assert!(
            schema.contains("    wake_skip_reason TEXT"),
            "d_dispatches 建表语句必须带 wake_skip_reason 列"
        );
        assert!(
            schema.contains("    dispatch_id     TEXT,"),
            "d_dispatches 建表语句必须带 dispatch_id 列（新库不靠迁移补）"
        );
        assert!(
            schema.contains("'awaiting','pending','delivering','delivered','skipped','failed'"),
            "wake_state 的 CHECK 必须含 awaiting"
        );

        let (before_migrations, after_migrations) =
            schema.split_once(SCHEMA_AFTER_MIGRATIONS).unwrap();
        assert!(
            !before_migrations.contains("idx_dispatches_state"),
            "迁移前段落不得引用旧索引名或旧列"
        );
        assert!(
            after_migrations.contains("DROP INDEX IF EXISTS idx_dispatches_state"),
            "旧索引名要在迁移后显式丢弃"
        );
        assert!(
            after_migrations.contains("idx_dispatches_task_state"),
            "task_state 索引建在迁移后段落"
        );
    }

    /// `schema.sql` 必须自带 `scope` 列：016 的列守卫对「`schema.sql` 漏加」是瞎的
    /// —— 漏加时全新库 `has_scope=false`，016 补列后两库的列形状仍然相同。
    #[test]
    fn schema_sql_declares_conversation_scope() {
        assert!(
            include_str!("schema.sql").contains("scope TEXT NOT NULL DEFAULT 'agent'"),
            "schema.sql 的 conversations 建表语句必须带 scope 列"
        );
    }
}
