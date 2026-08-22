// ============================================================================
// 数据库连接池与迁移
// ============================================================================

pub mod agents;
pub mod conversations;
pub mod documents;
pub mod knowledge_bases;
pub mod messages;
pub mod sync;
pub mod workflow_executions;
pub mod workflow_schedules;
pub mod workspace_hashes;

use sqlx::sqlite::SqlitePool;

/// 从数据库 URL 创建连接池。
pub async fn connect(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    let pool = SqlitePool::connect(database_url).await?;
    // 启用 WAL 模式和 foreign keys
    sqlx::raw_sql("PRAGMA journal_mode=WAL;")
        .execute(&pool)
        .await?;
    sqlx::raw_sql("PRAGMA foreign_keys=ON;")
        .execute(&pool)
        .await?;
    tracing::info!("SQLite connection pool established");
    Ok(pool)
}

/// 运行 DDL 迁移，创建所有表和索引。
///
/// 使用 `IF NOT EXISTS` 确保幂等 — 重复执行不会报错。
/// 按 `;` 拆分 SQL 语句逐一执行（`sqlx::raw_sql` 仅支持单条语句）。
pub async fn run_migrations(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let schema = include_str!("schema.sql");

    // 按分号拆分，逐条执行
    for statement in schema.split(';') {
        let trimmed = statement.trim();
        // 跳过空语句和纯注释行
        if trimmed.is_empty()
            || trimmed
                .lines()
                .all(|l| l.trim().is_empty() || l.trim().starts_with("--"))
        {
            continue;
        }
        sqlx::raw_sql(trimmed).execute(pool).await?;
    }

    // 运行版本化迁移
    run_versioned_migrations(pool).await?;

    tracing::info!("Database migrations completed successfully");
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
    let has_bg_color = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'background_color'",
    )
    .fetch_one(pool)
    .await?
        > 0;

    if !has_bg_color {
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
        tracing::debug!("Migration 004 skipped: background_color column already exists");
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

    Ok(())
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
