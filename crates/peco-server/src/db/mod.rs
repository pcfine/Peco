// ============================================================================
// 数据库连接池与迁移
// ============================================================================

pub mod agents;
pub mod compaction_log;
pub mod conversations;
pub mod documents;
pub mod knowledge_bases;
pub mod memory_audit;
pub mod memory_consolidation_optin;
pub mod memory_consolidation_state;
pub mod memory_recall_stats;
pub mod messages;
pub mod session_archive;
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

    Ok(())
}

/// 执行一段幂等迁移 SQL（按 `;` 拆分逐条执行，跳过空语句与纯注释）。
async fn run_migration(
    pool: &SqlitePool,
    name: &str,
    migration_sql: &str,
) -> Result<(), sqlx::Error> {
    tracing::info!("Running migration {name}");
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
}
