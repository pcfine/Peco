// ============================================================================
// 数据库连接池与迁移
// ============================================================================

pub mod agents;
pub mod conversations;
pub mod documents;
pub mod knowledge_bases;
pub mod messages;
pub mod task_logs;
pub mod tasks;

use sqlx::sqlite::SqlitePool;

/// 从数据库 URL 创建连接池。
pub async fn connect(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    let pool = SqlitePool::connect(database_url).await?;
    // 启用 WAL 模式和 foreign keys
    sqlx::raw_sql("PRAGMA journal_mode=WAL;").execute(&pool).await?;
    sqlx::raw_sql("PRAGMA foreign_keys=ON;").execute(&pool).await?;
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
            || trimmed.lines().all(|l| l.trim().is_empty() || l.trim().starts_with("--"))
        {
            continue;
        }
        sqlx::raw_sql(trimmed).execute(pool).await?;
    }

    tracing::info!("Database migrations completed successfully");
    Ok(())
}

// ── Server Config (键值对) ───────────────────────────────────────────────────

/// 从 `server_config` 表读取指定 key 的值。
pub async fn get_server_config(pool: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
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
