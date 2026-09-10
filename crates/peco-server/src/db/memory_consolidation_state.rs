// ============================================================================
// memory_consolidation_state 表 DAO — 整理水位与最近一次运行统计
// ============================================================================
//
// 每用户一行：候选收集水位（last_scanned_ts）+ 最近运行时刻与统计
// （last_run_at / last_run_stats，JSON），支撑巩固流程的幂等断点续跑。

use sqlx::SqlitePool;

/// 整理水位行。
#[derive(Debug, sqlx::FromRow)]
pub struct ConsolidationStateRow {
    pub user_id: String,
    pub last_scanned_ts: Option<String>,
    pub last_run_at: Option<String>,
    pub last_run_stats: Option<String>,
}

/// upsert 整理水位。
///
/// 参数为 `None` 时保留该列既有值（部分更新），`Some` 时覆盖 —
/// 巩固流水线各步骤可独立推进自己的水位而互不覆盖。
pub async fn upsert_state(
    pool: &SqlitePool,
    user_id: &str,
    last_scanned_ts: Option<&str>,
    last_run_at: Option<&str>,
    last_run_stats: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO memory_consolidation_state \
         (user_id, last_scanned_ts, last_run_at, last_run_stats) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT (user_id) DO UPDATE SET \
         last_scanned_ts = COALESCE(excluded.last_scanned_ts, last_scanned_ts), \
         last_run_at = COALESCE(excluded.last_run_at, last_run_at), \
         last_run_stats = COALESCE(excluded.last_run_stats, last_run_stats)",
    )
    .bind(user_id)
    .bind(last_scanned_ts)
    .bind(last_run_at)
    .bind(last_run_stats)
    .execute(pool)
    .await?;
    Ok(())
}

/// 读取用户整理水位；从未运行过时返回 `None`。
pub async fn get_state(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Option<ConsolidationStateRow>, sqlx::Error> {
    sqlx::query_as::<_, ConsolidationStateRow>(
        "SELECT user_id, last_scanned_ts, last_run_at, last_run_stats \
         FROM memory_consolidation_state WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = db::connect(&url).await.unwrap();
        db::run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    #[tokio::test]
    async fn test_upsert_state_inserts_then_partial_updates() {
        let (pool, _dir) = test_pool().await;

        assert!(get_state(&pool, "u1").await.unwrap().is_none());

        upsert_state(
            &pool,
            "u1",
            Some("2026-09-10T00:00:00+00:00"),
            Some("2026-09-10T00:01:00+00:00"),
            Some(r#"{"scanned":10,"merged":2,"deleted":1}"#),
        )
        .await
        .unwrap();

        let row = get_state(&pool, "u1").await.unwrap().unwrap();
        assert_eq!(
            row.last_scanned_ts.as_deref(),
            Some("2026-09-10T00:00:00+00:00")
        );
        assert_eq!(
            row.last_run_at.as_deref(),
            Some("2026-09-10T00:01:00+00:00")
        );
        assert_eq!(
            row.last_run_stats.as_deref(),
            Some(r#"{"scanned":10,"merged":2,"deleted":1}"#)
        );

        // None 参数保留既有值：只推进候选收集水位
        upsert_state(&pool, "u1", Some("2026-09-10T01:00:00+00:00"), None, None)
            .await
            .unwrap();
        let row = get_state(&pool, "u1").await.unwrap().unwrap();
        assert_eq!(
            row.last_scanned_ts.as_deref(),
            Some("2026-09-10T01:00:00+00:00")
        );
        assert_eq!(
            row.last_run_at.as_deref(),
            Some("2026-09-10T00:01:00+00:00")
        );
        assert_eq!(
            row.last_run_stats.as_deref(),
            Some(r#"{"scanned":10,"merged":2,"deleted":1}"#)
        );

        // 只更新运行统计
        upsert_state(&pool, "u1", None, Some("2026-09-10T02:00:00+00:00"), None)
            .await
            .unwrap();
        let row = get_state(&pool, "u1").await.unwrap().unwrap();
        assert_eq!(
            row.last_scanned_ts.as_deref(),
            Some("2026-09-10T01:00:00+00:00")
        );
        assert_eq!(
            row.last_run_at.as_deref(),
            Some("2026-09-10T02:00:00+00:00")
        );
    }

    #[tokio::test]
    async fn test_state_is_scoped_per_user() {
        let (pool, _dir) = test_pool().await;

        upsert_state(&pool, "u1", Some("2026-09-10T00:00:00+00:00"), None, None)
            .await
            .unwrap();

        assert!(get_state(&pool, "u2").await.unwrap().is_none());

        upsert_state(&pool, "u2", Some("2026-09-10T03:00:00+00:00"), None, None)
            .await
            .unwrap();
        let u1 = get_state(&pool, "u1").await.unwrap().unwrap();
        let u2 = get_state(&pool, "u2").await.unwrap().unwrap();
        assert_ne!(u1.last_scanned_ts, u2.last_scanned_ts);
    }
}
