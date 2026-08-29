// ============================================================================
// peco_session_archives 表 DAO — 归档式清空的会话存档
// ============================================================================
//
// DELETE /api/peco/session 清空前写入；只追加，不做更新。
// 列表 UI 延后 — 当前仅有 list/get 供归档端点使用。

use sqlx::SqlitePool;

/// 归档完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct SessionArchiveRow {
    pub id: String,
    pub user_id: String,
    pub conversation_id: String,
    pub turn_count: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub content_md: String,
    pub created_at: String,
}

/// 归档元数据（不含正文）。
#[derive(Debug, sqlx::FromRow)]
pub struct SessionArchiveMeta {
    pub id: String,
    pub user_id: String,
    pub conversation_id: String,
    pub turn_count: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub created_at: String,
}

/// 插入一条归档。
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    pool: &SqlitePool,
    id: &str,
    user_id: &str,
    conversation_id: &str,
    turn_count: usize,
    total_input_tokens: u64,
    total_output_tokens: u64,
    content_md: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO peco_session_archives \
         (id, user_id, conversation_id, turn_count, total_input_tokens, total_output_tokens, content_md) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(user_id)
    .bind(conversation_id)
    .bind(turn_count as i64)
    .bind(total_input_tokens as i64)
    .bind(total_output_tokens as i64)
    .bind(content_md)
    .execute(pool)
    .await?;
    Ok(())
}

/// 按用户列出归档元数据（时间倒序）。
pub async fn list_by_user(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<SessionArchiveMeta>, sqlx::Error> {
    sqlx::query_as::<_, SessionArchiveMeta>(
        "SELECT id, user_id, conversation_id, turn_count, total_input_tokens, \
         total_output_tokens, created_at \
         FROM peco_session_archives WHERE user_id = ? ORDER BY created_at DESC, rowid DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// 取一条归档（限定所属用户 — 防越权读取）。
pub async fn get(
    pool: &SqlitePool,
    user_id: &str,
    archive_id: &str,
) -> Result<Option<SessionArchiveRow>, sqlx::Error> {
    sqlx::query_as::<_, SessionArchiveRow>(
        "SELECT id, user_id, conversation_id, turn_count, total_input_tokens, \
         total_output_tokens, content_md, created_at \
         FROM peco_session_archives WHERE id = ? AND user_id = ?",
    )
    .bind(archive_id)
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
    async fn test_insert_list_get_roundtrip() {
        let (pool, _dir) = test_pool().await;

        insert(
            &pool,
            "a-1",
            "u1",
            "u1-private-session",
            12,
            1000,
            500,
            "# archive",
        )
        .await
        .unwrap();
        insert(
            &pool,
            "a-2",
            "u1",
            "u1-private-session",
            3,
            100,
            50,
            "# newer",
        )
        .await
        .unwrap();
        insert(
            &pool,
            "a-3",
            "u2",
            "u2-private-session",
            1,
            10,
            5,
            "# other user",
        )
        .await
        .unwrap();

        let metas = list_by_user(&pool, "u1").await.unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].id, "a-2"); // 倒序：新归档在前

        let row = get(&pool, "u1", "a-1").await.unwrap().unwrap();
        assert_eq!(row.content_md, "# archive");
        assert_eq!(row.turn_count, 12);

        // 越权读取返回 None
        assert!(get(&pool, "u2", "a-1").await.unwrap().is_none());
        assert!(get(&pool, "u1", "missing").await.unwrap().is_none());
    }
}
