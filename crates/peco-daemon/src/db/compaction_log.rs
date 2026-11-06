// ============================================================================
// peco_compaction_log 表 DAO — 上下文滚动压缩历史
// ============================================================================

use sqlx::SqlitePool;

/// 压缩日志完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct CompactionLogRow {
    pub id: String,
    pub user_id: String,
    pub conversation_id: String,
    pub evicted_turns: i64,
    pub tokens_before: i64,
    pub tokens_after: i64,
    pub summary_chars: i64,
    pub created_at: String,
}

/// 插入一条压缩日志。
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    pool: &SqlitePool,
    id: &str,
    user_id: &str,
    conversation_id: &str,
    evicted_turns: usize,
    tokens_before: usize,
    tokens_after: usize,
    summary_chars: usize,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO peco_compaction_log \
         (id, user_id, conversation_id, evicted_turns, tokens_before, tokens_after, summary_chars) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(user_id)
    .bind(conversation_id)
    .bind(evicted_turns as i64)
    .bind(tokens_before as i64)
    .bind(tokens_after as i64)
    .bind(summary_chars as i64)
    .execute(pool)
    .await?;
    Ok(())
}

/// 按会话列出压缩日志（时间正序 — 时间线语义；rowid 断开同秒并列）。
pub async fn list_by_conversation(
    pool: &SqlitePool,
    user_id: &str,
    conversation_id: &str,
) -> Result<Vec<CompactionLogRow>, sqlx::Error> {
    sqlx::query_as::<_, CompactionLogRow>(
        "SELECT id, user_id, conversation_id, evicted_turns, tokens_before, tokens_after, \
         summary_chars, created_at \
         FROM peco_compaction_log \
         WHERE user_id = ? AND conversation_id = ? \
         ORDER BY created_at ASC, rowid ASC",
    )
    .bind(user_id)
    .bind(conversation_id)
    .fetch_all(pool)
    .await
}

/// 删除某会话的全部压缩日志（清空会话时随会话生命周期回收）。
///
/// Peco 永续会话的 conversation_id 是确定性的（清空重置后复用），
/// 快照删除后必须同步清理日志，否则新会话的指标被旧会话污染。
pub async fn delete_by_conversation(
    pool: &SqlitePool,
    user_id: &str,
    conversation_id: &str,
) -> Result<u64, sqlx::Error> {
    let result =
        sqlx::query("DELETE FROM peco_compaction_log WHERE user_id = ? AND conversation_id = ?")
            .bind(user_id)
            .bind(conversation_id)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
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
    async fn test_insert_and_list_chronological() {
        let (pool, _dir) = test_pool().await;

        insert(
            &pool,
            "id-1",
            "u1",
            "u1-private-session",
            3,
            24000,
            9000,
            400,
        )
        .await
        .unwrap();
        insert(
            &pool,
            "id-2",
            "u1",
            "u1-private-session",
            2,
            21000,
            8500,
            450,
        )
        .await
        .unwrap();
        insert(
            &pool,
            "id-3",
            "u2",
            "u2-private-session",
            1,
            20000,
            8000,
            300,
        )
        .await
        .unwrap();

        let rows = list_by_conversation(&pool, "u1", "u1-private-session")
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "id-1");
        assert_eq!(rows[0].evicted_turns, 3);
        assert_eq!(rows[1].id, "id-2");
    }

    #[tokio::test]
    async fn test_list_empty() {
        let (pool, _dir) = test_pool().await;
        let rows = list_by_conversation(&pool, "nobody", "nobody-private-session")
            .await
            .unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn test_delete_by_conversation_scoped() {
        let (pool, _dir) = test_pool().await;
        insert(&pool, "id-1", "u1", "u1-private-session", 1, 100, 50, 10)
            .await
            .unwrap();
        insert(&pool, "id-2", "u1", "u1-other-session", 1, 100, 50, 10)
            .await
            .unwrap();
        insert(&pool, "id-3", "u2", "u1-private-session", 1, 100, 50, 10)
            .await
            .unwrap();

        let deleted = delete_by_conversation(&pool, "u1", "u1-private-session")
            .await
            .unwrap();
        assert_eq!(deleted, 1);

        // 其他用户 / 其他会话的日志不受影响
        assert_eq!(
            list_by_conversation(&pool, "u1", "u1-other-session")
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            list_by_conversation(&pool, "u2", "u1-private-session")
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
