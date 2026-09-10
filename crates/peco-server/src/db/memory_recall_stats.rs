// ============================================================================
// memory_recall_stats 表 DAO — 文档级召回命中统计
// ============================================================================
//
// 读路径每次召回命中即记账：首见插入 count=1，重复命中 count 累加。
// 供巩固候选收集（低召回 → TTL 候选）与写路径去重参考。

use sqlx::SqlitePool;

/// 单条召回统计行。
#[derive(Debug, sqlx::FromRow)]
pub struct RecallStatRow {
    pub user_id: String,
    pub doc_id: String,
    pub last_recalled_at: String,
    pub recall_count: i64,
}

/// 记录一次召回命中：首见插入 count=1，重复命中 count 累加并刷新时刻。
pub async fn record_recall(
    pool: &SqlitePool,
    user_id: &str,
    doc_id: &str,
    recalled_at: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO memory_recall_stats (user_id, doc_id, last_recalled_at, recall_count) \
         VALUES (?, ?, ?, 1) \
         ON CONFLICT (user_id, doc_id) DO UPDATE SET \
         last_recalled_at = excluded.last_recalled_at, \
         recall_count = recall_count + 1",
    )
    .bind(user_id)
    .bind(doc_id)
    .bind(recalled_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// 批量记录一轮召回命中的多个文档（同一事务内，原子落账）。
pub async fn record_recalls_batch(
    pool: &SqlitePool,
    user_id: &str,
    doc_ids: &[String],
    recalled_at: &str,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for doc_id in doc_ids {
        sqlx::query(
            "INSERT INTO memory_recall_stats (user_id, doc_id, last_recalled_at, recall_count) \
             VALUES (?, ?, ?, 1) \
             ON CONFLICT (user_id, doc_id) DO UPDATE SET \
             last_recalled_at = excluded.last_recalled_at, \
             recall_count = recall_count + 1",
        )
        .bind(user_id)
        .bind(doc_id)
        .bind(recalled_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// 按用户列出召回统计（最近召回倒序）。
pub async fn list_by_user(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<RecallStatRow>, sqlx::Error> {
    sqlx::query_as::<_, RecallStatRow>(
        "SELECT user_id, doc_id, last_recalled_at, recall_count \
         FROM memory_recall_stats WHERE user_id = ? \
         ORDER BY last_recalled_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
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
    async fn test_record_recall_accumulates_count() {
        let (pool, _dir) = test_pool().await;

        record_recall(&pool, "u1", "doc-a", "2026-09-10T00:00:01+00:00")
            .await
            .unwrap();
        record_recall(&pool, "u1", "doc-a", "2026-09-10T00:00:02+00:00")
            .await
            .unwrap();
        record_recall(&pool, "u1", "doc-b", "2026-09-10T00:00:03+00:00")
            .await
            .unwrap();

        let rows = list_by_user(&pool, "u1").await.unwrap();
        assert_eq!(rows.len(), 2);

        let doc_a = rows.iter().find(|r| r.doc_id == "doc-a").unwrap();
        assert_eq!(doc_a.recall_count, 2, "重复命中应累加");
        assert_eq!(doc_a.last_recalled_at, "2026-09-10T00:00:02+00:00");

        let doc_b = rows.iter().find(|r| r.doc_id == "doc-b").unwrap();
        assert_eq!(doc_b.recall_count, 1);
    }

    #[tokio::test]
    async fn test_record_recalls_batch_is_atomic_and_accumulating() {
        let (pool, _dir) = test_pool().await;

        let batch = vec!["doc-a".to_string(), "doc-b".to_string()];
        record_recalls_batch(&pool, "u1", &batch, "2026-09-10T00:00:01+00:00")
            .await
            .unwrap();
        record_recalls_batch(&pool, "u1", &batch, "2026-09-10T00:00:02+00:00")
            .await
            .unwrap();

        let rows = list_by_user(&pool, "u1").await.unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(row.recall_count, 2);
            assert_eq!(row.last_recalled_at, "2026-09-10T00:00:02+00:00");
        }
    }

    #[tokio::test]
    async fn test_list_by_user_is_scoped_and_ordered_desc() {
        let (pool, _dir) = test_pool().await;

        record_recall(&pool, "u1", "doc-old", "2026-09-01T00:00:00+00:00")
            .await
            .unwrap();
        record_recall(&pool, "u1", "doc-new", "2026-09-10T00:00:00+00:00")
            .await
            .unwrap();
        record_recall(&pool, "u2", "doc-other", "2026-09-10T00:00:00+00:00")
            .await
            .unwrap();

        let rows = list_by_user(&pool, "u1").await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].doc_id, "doc-new");
        assert_eq!(rows[1].doc_id, "doc-old");
    }
}
