// ============================================================================
// memory_audit 表 DAO — 记忆删除审计（outbox + 回滚依据）
// ============================================================================
//
// outbox 语义：删除前先写 pending，删除成功 mark_done，失败 mark_cancelled；
// 收口失败（如 mark_done 时 DB 故障）会残留 pending 行 — 原文仍在行内，可人工恢复。
// 回滚 = 按审计行重放 add_text（doc_id 由内容哈希派生、摄入为替换语义，重放幂等），
// 完成后回填 restored_at / restored_doc_id。
// 审计含被删记忆原文，只落 SQLite（不在任何检索面上）。
// 保留期：purge_older_than 支持按截止时刻物理清除终态行；
// 定时清理（默认 90 天）待接入调度器后生效。

use sqlx::SqlitePool;

use peco_core::tools::MemoryAuditEntry;

/// 审计完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct MemoryAuditRow {
    pub id: i64,
    pub user_id: String,
    pub kb_name: String,
    pub doc_id: String,
    pub title: String,
    pub content: String,
    pub source: String,
    pub reason: String,
    pub deleted_by: String,
    pub status: String,
    pub deleted_at: String,
    pub restored_at: Option<String>,
    pub restored_doc_id: Option<String>,
}

const ROW_COLUMNS: &str = "id, user_id, kb_name, doc_id, title, content, source, reason, \
     deleted_by, status, deleted_at, restored_at, restored_doc_id";

/// 写入一条 pending 审计，返回审计行 id。
///
/// 参数直接取 [`MemoryAuditEntry`]（命名字段，杜绝同型参数换位）。
pub async fn insert_pending(
    pool: &SqlitePool,
    entry: &MemoryAuditEntry,
) -> Result<i64, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO memory_audit \
         (user_id, kb_name, doc_id, title, content, source, reason, deleted_by, deleted_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&entry.user_id)
    .bind(&entry.kb_name)
    .bind(&entry.doc_id)
    .bind(&entry.title)
    .bind(&entry.content)
    .bind(&entry.source)
    .bind(&entry.reason)
    .bind(&entry.deleted_by)
    .bind(&entry.deleted_at)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// pending → done。仅 pending 可迁移，返回受影响行数（0 = 行不存在或已迁移）。
pub async fn mark_done(pool: &SqlitePool, id: i64) -> Result<u64, sqlx::Error> {
    let result =
        sqlx::query("UPDATE memory_audit SET status = 'done' WHERE id = ? AND status = 'pending'")
            .bind(id)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
}

/// pending → cancelled。仅 pending 可迁移，返回受影响行数。
pub async fn mark_cancelled(pool: &SqlitePool, id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE memory_audit SET status = 'cancelled' WHERE id = ? AND status = 'pending'",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 回滚回填 restored_at / restored_doc_id。
///
/// 仅「done 且未回滚」的行允许回滚（`restored_at IS NULL` 是权威防重守卫，
/// 并发回滚时只有一个请求能命中），返回受影响行数（0 = 行不存在、已迁移或已回滚）。
pub async fn mark_restored(
    pool: &SqlitePool,
    id: i64,
    restored_at: &str,
    restored_doc_id: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE memory_audit SET restored_at = ?, restored_doc_id = ? \
         WHERE id = ? AND status = 'done' AND restored_at IS NULL",
    )
    .bind(restored_at)
    .bind(restored_doc_id)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 按 id 读取单条审计（回滚前校验归属与状态用）。
pub async fn get(pool: &SqlitePool, id: i64) -> Result<Option<MemoryAuditRow>, sqlx::Error> {
    sqlx::query_as::<_, MemoryAuditRow>(&format!(
        "SELECT {ROW_COLUMNS} FROM memory_audit WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// 按用户分页列出审计行（deleted_at 倒序，走 idx_memory_audit_user 索引）。
pub async fn list_by_user(
    pool: &SqlitePool,
    user_id: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<MemoryAuditRow>, sqlx::Error> {
    sqlx::query_as::<_, MemoryAuditRow>(&format!(
        "SELECT {ROW_COLUMNS} FROM memory_audit WHERE user_id = ? \
             ORDER BY deleted_at DESC, id DESC LIMIT ? OFFSET ?"
    ))
    .bind(user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// 物理清除保留期之外的终态审计行（done / cancelled），返回删除行数。
///
/// pending 视为未决（删除流程尚未收口），不在此清除 —
/// 调用方应先让 outbox 行落到终态，再依赖保留期清理。
pub async fn purge_older_than(pool: &SqlitePool, cutoff: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM memory_audit \
         WHERE deleted_at < ? AND status IN ('done', 'cancelled')",
    )
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

    async fn insert_row(pool: &SqlitePool, user_id: &str, doc_id: &str, deleted_at: &str) -> i64 {
        insert_pending(
            pool,
            &MemoryAuditEntry {
                user_id: user_id.into(),
                kb_name: "@private_memory".into(),
                doc_id: doc_id.into(),
                title: "memory_1".into(),
                content: "用户偏好 Rust".into(),
                source: "ppa_semantic".into(),
                reason: "manual_organize".into(),
                deleted_by: "agent:@memory".into(),
                deleted_at: deleted_at.into(),
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn test_outbox_pending_to_done() {
        let (pool, _dir) = test_pool().await;
        let id = insert_row(&pool, "u1", "doc-a", "2026-09-10T00:00:00+00:00").await;

        let row = get(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.status, "pending");
        assert_eq!(row.doc_id, "doc-a");
        assert!(row.restored_at.is_none());

        assert_eq!(mark_done(&pool, id).await.unwrap(), 1);
        assert_eq!(get(&pool, id).await.unwrap().unwrap().status, "done");

        // 仅 pending 可迁移：再次 mark_done 无效果
        assert_eq!(mark_done(&pool, id).await.unwrap(), 0);
        assert_eq!(get(&pool, id).await.unwrap().unwrap().status, "done");
    }

    #[tokio::test]
    async fn test_outbox_pending_to_cancelled() {
        let (pool, _dir) = test_pool().await;
        let id = insert_row(&pool, "u1", "doc-b", "2026-09-10T00:00:00+00:00").await;

        assert_eq!(mark_cancelled(&pool, id).await.unwrap(), 1);
        assert_eq!(get(&pool, id).await.unwrap().unwrap().status, "cancelled");

        // cancelled 是终态：mark_done 不再生效
        assert_eq!(mark_done(&pool, id).await.unwrap(), 0);
        assert_eq!(get(&pool, id).await.unwrap().unwrap().status, "cancelled");
    }

    #[tokio::test]
    async fn test_mark_restored_backfills_fields() {
        let (pool, _dir) = test_pool().await;
        let id = insert_row(&pool, "u1", "doc-c", "2026-09-10T00:00:00+00:00").await;

        // pending 行不允许回滚回填
        assert_eq!(
            mark_restored(&pool, id, "2026-09-11T00:00:00+00:00", "doc-c")
                .await
                .unwrap(),
            0
        );

        mark_done(&pool, id).await.unwrap();
        assert_eq!(
            mark_restored(&pool, id, "2026-09-11T00:00:00+00:00", "doc-c")
                .await
                .unwrap(),
            1
        );

        // 已回滚的行不可重复回滚（restored_at IS NULL 守卫），首次回填不被覆盖
        assert_eq!(
            mark_restored(&pool, id, "2026-09-12T00:00:00+00:00", "doc-c")
                .await
                .unwrap(),
            0
        );

        let row = get(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            row.restored_at.as_deref(),
            Some("2026-09-11T00:00:00+00:00")
        );
        assert_eq!(row.restored_doc_id.as_deref(), Some("doc-c"));
    }

    #[tokio::test]
    async fn test_get_missing_row_returns_none() {
        let (pool, _dir) = test_pool().await;
        assert!(get(&pool, 999).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_list_by_user_orders_desc_and_paginates() {
        let (pool, _dir) = test_pool().await;
        insert_row(&pool, "u1", "doc-old", "2026-09-01T00:00:00+00:00").await;
        insert_row(&pool, "u1", "doc-mid", "2026-09-05T00:00:00+00:00").await;
        insert_row(&pool, "u1", "doc-new", "2026-09-10T00:00:00+00:00").await;
        insert_row(&pool, "u2", "doc-other", "2026-09-10T00:00:00+00:00").await;

        let page1 = list_by_user(&pool, "u1", 2, 0).await.unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].doc_id, "doc-new");
        assert_eq!(page1[1].doc_id, "doc-mid");

        let page2 = list_by_user(&pool, "u1", 2, 2).await.unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].doc_id, "doc-old");

        // 用户隔离
        assert_eq!(list_by_user(&pool, "u2", 10, 0).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_purge_older_than_keeps_recent_and_pending() {
        let (pool, _dir) = test_pool().await;
        let old_done = insert_row(&pool, "u1", "doc-old-done", "2026-06-01T00:00:00+00:00").await;
        mark_done(&pool, old_done).await.unwrap();
        let old_cancelled = insert_row(
            &pool,
            "u1",
            "doc-old-cancelled",
            "2026-06-02T00:00:00+00:00",
        )
        .await;
        mark_cancelled(&pool, old_cancelled).await.unwrap();
        insert_row(&pool, "u1", "doc-old-pending", "2026-06-03T00:00:00+00:00").await;
        insert_row(&pool, "u1", "doc-recent", "2026-09-10T00:00:00+00:00").await;

        let purged = purge_older_than(&pool, "2026-09-01T00:00:00+00:00")
            .await
            .unwrap();
        assert_eq!(purged, 2, "只清除保留期外的终态行");

        assert!(get(&pool, old_done).await.unwrap().is_none());
        assert!(get(&pool, old_cancelled).await.unwrap().is_none());
        // pending 未决行与保留期内行不受影响
        let remaining = list_by_user(&pool, "u1", 10, 0).await.unwrap();
        let doc_ids: Vec<&str> = remaining.iter().map(|r| r.doc_id.as_str()).collect();
        assert!(doc_ids.contains(&"doc-old-pending"));
        assert!(doc_ids.contains(&"doc-recent"));
    }
}
