// ============================================================================
// peco_context_shadow 表 DAO — 压缩边界观测（shadow）
// ============================================================================
//
// 每个 turn 边界一行：记录该边界的 token 口径与削减评估值，与
// peco_compaction_log（只记折叠）互补。只追加、不更新；
// 过期行由 purge_older_than 按保留期回收。

use sqlx::SqlitePool;

/// 边界观测完整行（19 列，与迁移 013 同序）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ContextShadowRow {
    pub id: String,
    pub user_id: String,
    pub conversation_id: String,
    pub turn_index: i64,
    /// ISO 8601（rfc3339）—— 写入侧显式生成，与保留期 cutoff 同格式，
    /// 避免 `datetime('now')` 的空格格式与 rfc3339 的 `T` 在字符串比较中错位。
    pub created_at: String,
    pub trigger_caliber: String,
    pub pinned_tokens: i64,
    pub committed_full_tokens: i64,
    pub committed_view_tokens: i64,
    pub view_window_tokens: i64,
    pub fold_triggered: i64,
    pub evicted_turns: Option<i64>,
    pub guard_triggered: i64,
    pub strip_evaluated: i64,
    pub strip_enforced: i64,
    pub stripped_items: i64,
    pub released_tokens: i64,
    pub tool_output_items: i64,
    pub tool_output_over_cap: i64,
    pub image_output_items: i64,
    pub image_base64_bytes: i64,
    pub snapshot_json_bytes: i64,
}

const COLUMNS: &str = "id, user_id, conversation_id, turn_index, created_at, trigger_caliber, \
     pinned_tokens, committed_full_tokens, committed_view_tokens, view_window_tokens, \
     fold_triggered, evicted_turns, guard_triggered, strip_evaluated, strip_enforced, \
     stripped_items, released_tokens, tool_output_items, tool_output_over_cap, \
     image_output_items, image_base64_bytes, snapshot_json_bytes";

/// 插入一条边界观测行。
pub async fn insert(pool: &SqlitePool, row: &ContextShadowRow) -> Result<(), sqlx::Error> {
    let sql = format!(
        "INSERT INTO peco_context_shadow ({COLUMNS}) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"
    );
    sqlx::query(&sql)
        .bind(&row.id)
        .bind(&row.user_id)
        .bind(&row.conversation_id)
        .bind(row.turn_index)
        .bind(&row.created_at)
        .bind(&row.trigger_caliber)
        .bind(row.pinned_tokens)
        .bind(row.committed_full_tokens)
        .bind(row.committed_view_tokens)
        .bind(row.view_window_tokens)
        .bind(row.fold_triggered)
        .bind(row.evicted_turns)
        .bind(row.guard_triggered)
        .bind(row.strip_evaluated)
        .bind(row.strip_enforced)
        .bind(row.stripped_items)
        .bind(row.released_tokens)
        .bind(row.tool_output_items)
        .bind(row.tool_output_over_cap)
        .bind(row.image_output_items)
        .bind(row.image_base64_bytes)
        .bind(row.snapshot_json_bytes)
        .execute(pool)
        .await?;
    Ok(())
}

/// 按用户列出边界观测行（时间正序）。
pub async fn list_by_user(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<ContextShadowRow>, sqlx::Error> {
    sqlx::query_as::<_, ContextShadowRow>(&format!(
        "SELECT {COLUMNS} FROM peco_context_shadow WHERE user_id = ? ORDER BY created_at ASC, rowid ASC"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// 删除 cutoff 之前的全部观测行，返回删除行数。
pub async fn purge_older_than(pool: &SqlitePool, cutoff: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM peco_context_shadow WHERE created_at < ?")
        .bind(cutoff)
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

    fn row(id: &str, created_at: &str) -> ContextShadowRow {
        ContextShadowRow {
            id: id.to_string(),
            user_id: "u1".to_string(),
            conversation_id: "u1-private-session".to_string(),
            turn_index: 7,
            created_at: created_at.to_string(),
            trigger_caliber: "full".to_string(),
            pinned_tokens: 1000,
            committed_full_tokens: 5000,
            committed_view_tokens: 2000,
            view_window_tokens: 1800,
            fold_triggered: 1,
            evicted_turns: Some(2),
            guard_triggered: 0,
            strip_evaluated: 1,
            strip_enforced: 0,
            stripped_items: 5,
            released_tokens: 300,
            tool_output_items: 9,
            tool_output_over_cap: 3,
            image_output_items: 1,
            image_base64_bytes: 4096,
            snapshot_json_bytes: 12345,
        }
    }

    #[tokio::test]
    async fn insert_and_list_roundtrip() {
        let (pool, _dir) = test_pool().await;
        insert(&pool, &row("s-1", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();
        insert(&pool, &row("s-2", "2026-10-02T00:00:00+00:00"))
            .await
            .unwrap();

        let rows = list_by_user(&pool, "u1").await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "s-1");
        assert_eq!(rows[1].trigger_caliber, "full");
        assert_eq!(rows[0].evicted_turns, Some(2));
        assert_eq!(rows[0].snapshot_json_bytes, 12345);
    }

    #[tokio::test]
    async fn purge_removes_only_expired_rows() {
        let (pool, _dir) = test_pool().await;
        insert(&pool, &row("old", "2026-09-01T00:00:00+00:00"))
            .await
            .unwrap();
        insert(&pool, &row("recent", "2026-10-06T00:00:00+00:00"))
            .await
            .unwrap();

        let removed = purge_older_than(&pool, "2026-10-01T00:00:00+00:00")
            .await
            .unwrap();
        assert_eq!(removed, 1);

        let rows = list_by_user(&pool, "u1").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "recent");
    }
}
