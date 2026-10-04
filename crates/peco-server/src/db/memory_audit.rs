// ============================================================================
// memory_audit 表 DAO — 记忆删除审计（outbox + 回滚依据）
// ============================================================================
//
// outbox 语义：删除前先写 pending，删除成功 mark_done，失败 mark_cancelled；
// 收口失败（如 mark_done 时 DB 故障）会残留 pending 行 — 原文仍在行内，可人工恢复。
// 回滚 = 按审计行重放 add_text（doc_id 由内容哈希派生、摄入为替换语义，重放幂等），
// 完成后回填 restored_at / restored_doc_id。
// 审计含被删记忆原文，只落 SQLite（不在任何检索面上）。
// 保留期：purge_older_than 按 reason 分档物理清除终态行
// （superseded 30d / 其余 90d，pending 不清）；worker 与启动双通道清理。

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
    /// 取代槽键（仅 `reason='superseded'` 行）：**取代方 fact 的 topic**（M2 口径）。
    pub topic_key: Option<String>,
    /// 后继 doc id（仅 `reason='superseded'` 行）：取代方新条的 doc id。
    pub successor_doc_id: Option<String>,
}

const ROW_COLUMNS: &str = "id, user_id, kb_name, doc_id, title, content, source, reason, \
     deleted_by, status, deleted_at, restored_at, restored_doc_id, topic_key, successor_doc_id";

/// 写入一条 pending 审计，返回审计行 id。
///
/// 参数直接取 [`MemoryAuditEntry`]（命名字段，杜绝同型参数换位）。
pub async fn insert_pending(
    pool: &SqlitePool,
    entry: &MemoryAuditEntry,
) -> Result<i64, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO memory_audit \
         (user_id, kb_name, doc_id, title, content, source, reason, deleted_by, deleted_at, \
          topic_key, successor_doc_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .bind(&entry.topic_key)
    .bind(&entry.successor_doc_id)
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
    list_by_user_reason(pool, user_id, None, limit, offset).await
}

/// 按用户分页列出审计行，可按 `reason` 过滤（design §11 历史 tab：
/// `GET /memory/audit?reason=superseded`）。
///
/// `reason = None` 时不过滤；排序保持 `deleted_at DESC, id DESC`。
pub async fn list_by_user_reason(
    pool: &SqlitePool,
    user_id: &str,
    reason: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<MemoryAuditRow>, sqlx::Error> {
    sqlx::query_as::<_, MemoryAuditRow>(&format!(
        "SELECT {ROW_COLUMNS} FROM memory_audit \
             WHERE user_id = ? AND (? IS NULL OR reason = ?) \
             ORDER BY deleted_at DESC, id DESC LIMIT ? OFFSET ?"
    ))
    .bind(user_id)
    .bind(reason)
    .bind(reason)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// 某 doc_id 在审计面最新一条 `status='done'` 行的标题（§11 后继标题解析
/// 第二路 —— 后继已再次退役时取这里）。
///
/// **必须先过滤 `status='done'`**：cancelled 重复行可能更新，不滤会顶替
/// 正确标题（design nit10）。
pub async fn latest_done_row_title(
    pool: &SqlitePool,
    user_id: &str,
    kb_name: &str,
    doc_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT title FROM memory_audit \
         WHERE user_id = ? AND kb_name = ? AND doc_id = ? AND status = 'done' \
         ORDER BY deleted_at DESC, id DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(kb_name)
    .bind(doc_id)
    .fetch_optional(pool)
    .await
}

/// 物理清除保留期之外的终态审计行（done / cancelled），返回删除行数。
///
/// 按 reason 分档（design-v4 §6.6）：`superseded` 走 `superseded_cutoff`
/// （默认 30d），其余 reason 走 `cutoff`（默认 90d）。
/// pending 视为未决（删除流程尚未收口），不在此清除 —
/// 调用方应先让 outbox 行落到终态，再依赖保留期清理。
pub async fn purge_older_than(
    pool: &SqlitePool,
    cutoff: &str,
    superseded_cutoff: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM memory_audit \
         WHERE status IN ('done', 'cancelled') \
           AND ((reason = 'superseded' AND deleted_at < ?) \
             OR (COALESCE(reason, '') != 'superseded' AND deleted_at < ?))",
    )
    .bind(superseded_cutoff)
    .bind(cutoff)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 按 (user, kb, doc_id) 取唯一一条 unrestored + done 的 superseded 行（§7.2）。
///
/// 同 doc_id 多行只可能来自复活路径；`ORDER BY deleted_at DESC, id DESC LIMIT 1`
/// 保证选行唯一。
pub async fn latest_superseded_row(
    pool: &SqlitePool,
    user_id: &str,
    kb_name: &str,
    doc_id: &str,
) -> Result<Option<MemoryAuditRow>, sqlx::Error> {
    sqlx::query_as::<_, MemoryAuditRow>(&format!(
        "SELECT {ROW_COLUMNS} FROM memory_audit \
         WHERE user_id = ? AND kb_name = ? AND doc_id = ? AND reason = 'superseded' \
           AND status = 'done' AND restored_at IS NULL \
         ORDER BY deleted_at DESC, id DESC LIMIT 1"
    ))
    .bind(user_id)
    .bind(kb_name)
    .bind(doc_id)
    .fetch_optional(pool)
    .await
}

/// 扫超时的 pending superseded 行（对账收口用，§6.5-2）。
///
/// 跨用户返回 — 调用方按 `user_id` 内存过滤（对账按用户收口，签名保持无 user 维度）。
pub async fn list_pending_superseded_before(
    pool: &SqlitePool,
    before: &str,
    limit: i64,
) -> Result<Vec<MemoryAuditRow>, sqlx::Error> {
    sqlx::query_as::<_, MemoryAuditRow>(&format!(
        "SELECT {ROW_COLUMNS} FROM memory_audit \
         WHERE reason = 'superseded' AND status = 'pending' AND deleted_at < ? \
         ORDER BY deleted_at ASC, id ASC LIMIT ?"
    ))
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// 把 audit 行 status 置 done（对账补 done）。
///
/// 语义别名：与 [`mark_done`] 同一 CAS（仅 pending 可迁移），
/// 对账面收口处用本名表意。
pub async fn mark_audit_row_done(pool: &SqlitePool, id: i64) -> Result<(), sqlx::Error> {
    mark_done(pool, id).await?;
    Ok(())
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
                topic_key: None,
                successor_doc_id: None,
            },
        )
        .await
        .unwrap()
    }

    /// 取代行写入辅助（带 topic_key / successor_doc_id 两新列）。
    async fn insert_superseded_row(
        pool: &SqlitePool,
        user_id: &str,
        doc_id: &str,
        deleted_at: &str,
    ) -> i64 {
        insert_pending(
            pool,
            &MemoryAuditEntry {
                user_id: user_id.into(),
                kb_name: "@private_memory".into(),
                doc_id: doc_id.into(),
                title: "memory_1".into(),
                content: "旧事实".into(),
                source: "ppa_semantic".into(),
                reason: "superseded".into(),
                deleted_by: "hook:supersede".into(),
                deleted_at: deleted_at.into(),
                topic_key: Some("project_phase".into()),
                successor_doc_id: Some("new-1".into()),
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

        let purged = purge_older_than(
            &pool,
            "2026-09-01T00:00:00+00:00",
            "2026-09-01T00:00:00+00:00",
        )
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

    /// 新列往返：superseded 行写入并读回 topic_key / successor_doc_id；
    /// 非取代行两列为 NULL。
    #[tokio::test]
    async fn test_topic_and_successor_columns_roundtrip() {
        let (pool, _dir) = test_pool().await;
        let sid = insert_superseded_row(&pool, "u1", "old-1", "2026-10-01T00:00:00+00:00").await;
        let nid = insert_row(&pool, "u1", "doc-a", "2026-10-01T00:00:00+00:00").await;

        let srow = get(&pool, sid).await.unwrap().unwrap();
        assert_eq!(srow.topic_key.as_deref(), Some("project_phase"));
        assert_eq!(srow.successor_doc_id.as_deref(), Some("new-1"));

        let nrow = get(&pool, nid).await.unwrap().unwrap();
        assert!(nrow.topic_key.is_none());
        assert!(nrow.successor_doc_id.is_none());
    }

    /// purge 分档：superseded 走短档（30d 口径），其余走长档（90d 口径）；
    /// 同一时刻的两行只有 superseded 被清。
    #[tokio::test]
    async fn test_purge_tiers_by_reason() {
        let (pool, _dir) = test_pool().await;
        let sup = insert_superseded_row(&pool, "u1", "old-sup", "2026-08-01T00:00:00+00:00").await;
        let manual = insert_row(&pool, "u1", "doc-manual", "2026-08-01T00:00:00+00:00").await;
        mark_done(&pool, sup).await.unwrap();
        mark_done(&pool, manual).await.unwrap();

        // 短档 2026-09-01（≈30d）/ 长档 2026-07-03（≈90d）
        let purged = purge_older_than(
            &pool,
            "2026-07-03T00:00:00+00:00",
            "2026-09-01T00:00:00+00:00",
        )
        .await
        .unwrap();
        assert_eq!(purged, 1, "只有 superseded 行落入短档");
        assert!(
            get(&pool, sup).await.unwrap().is_none(),
            "superseded 已过短档"
        );
        assert!(
            get(&pool, manual).await.unwrap().is_some(),
            "同一时刻的其余 reason 未过长档，必须保留"
        );
    }

    /// latest_superseded_row 唯一选行：done 且未回滚、取最新一条。
    #[tokio::test]
    async fn test_latest_superseded_row_unique_pick() {
        let (pool, _dir) = test_pool().await;
        let older = insert_superseded_row(&pool, "u1", "doc-x", "2026-09-01T00:00:00+00:00").await;
        let newer = insert_superseded_row(&pool, "u1", "doc-x", "2026-10-01T00:00:00+00:00").await;
        mark_done(&pool, older).await.unwrap();
        mark_done(&pool, newer).await.unwrap();
        // 已回滚的行不参与选行
        mark_restored(&pool, newer, "2026-10-02T00:00:00+00:00", "doc-x2")
            .await
            .unwrap();

        let row = latest_superseded_row(&pool, "u1", "@private_memory", "doc-x")
            .await
            .unwrap()
            .expect("应选到未回滚的 done 行");
        assert_eq!(row.id, older, "restored 行被过滤后应回退到较早那条");
        assert_eq!(row.successor_doc_id.as_deref(), Some("new-1"));

        // 用户 / kb 隔离
        assert!(
            latest_superseded_row(&pool, "u2", "@private_memory", "doc-x")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            latest_superseded_row(&pool, "u1", "@other_kb", "doc-x")
                .await
                .unwrap()
                .is_none()
        );
    }

    /// reason 过滤：Some 只回该 reason 的行，None 不过滤（历史 tab 两态）。
    #[tokio::test]
    async fn test_list_by_user_reason_filter() {
        let (pool, _dir) = test_pool().await;
        insert_row(&pool, "u1", "doc-manual", "2026-10-01T00:00:00+00:00").await;
        insert_superseded_row(&pool, "u1", "doc-sup", "2026-10-02T00:00:00+00:00").await;
        insert_superseded_row(&pool, "u2", "doc-other", "2026-10-03T00:00:00+00:00").await;

        let all = list_by_user_reason(&pool, "u1", None, 10, 0).await.unwrap();
        assert_eq!(all.len(), 2, "None 不过滤");

        let sup = list_by_user_reason(&pool, "u1", Some("superseded"), 10, 0)
            .await
            .unwrap();
        assert_eq!(sup.len(), 1);
        assert_eq!(sup[0].doc_id, "doc-sup");

        let none = list_by_user_reason(&pool, "u1", Some("manual_organize"), 10, 0)
            .await
            .unwrap();
        assert_eq!(none.len(), 1);
        assert_eq!(none[0].doc_id, "doc-manual");

        // 排序与分页口径不因过滤改变
        let empty = list_by_user_reason(&pool, "u2", Some("superseded"), 1, 1)
            .await
            .unwrap();
        assert!(empty.is_empty(), "分页偏移越过第二条");
    }

    /// latest_done_row_title：先滤 done（cancelled 更新行不得顶替），
    /// 取最新；无 done 行 / 无行 → None；按 user+kb 隔离。
    #[tokio::test]
    async fn test_latest_done_row_title_filters_cancelled() {
        let (pool, _dir) = test_pool().await;
        let done_id = insert_row(&pool, "u1", "doc-t", "2026-09-01T00:00:00+00:00").await;
        mark_done(&pool, done_id).await.unwrap();
        // 同 doc_id 的 cancelled 行更晚写入 — 不滤 done 会顶替
        let cancelled_id = insert_row(&pool, "u1", "doc-t", "2026-10-01T00:00:00+00:00").await;
        mark_cancelled(&pool, cancelled_id).await.unwrap();

        assert_eq!(
            latest_done_row_title(&pool, "u1", "@private_memory", "doc-t")
                .await
                .unwrap()
                .as_deref(),
            Some("memory_1"),
            "cancelled 行不得顶替 done 行标题"
        );

        // 隔离：用户 / kb / doc_id 任一不符 → None
        assert!(
            latest_done_row_title(&pool, "u2", "@private_memory", "doc-t")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            latest_done_row_title(&pool, "u1", "@other_kb", "doc-t")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            latest_done_row_title(&pool, "u1", "@private_memory", "doc-missing")
                .await
                .unwrap()
                .is_none()
        );

        // 只有 cancelled 行 → None（status='done' 过滤生效）
        let only_cancelled = insert_row(&pool, "u1", "doc-c", "2026-10-01T00:00:00+00:00").await;
        mark_cancelled(&pool, only_cancelled).await.unwrap();
        assert!(
            latest_done_row_title(&pool, "u1", "@private_memory", "doc-c")
                .await
                .unwrap()
                .is_none()
        );
    }

    /// 对账收口面：超时 pending superseded 扫描 + mark_audit_row_done。
    #[tokio::test]
    async fn test_pending_superseded_scan_and_close() {
        let (pool, _dir) = test_pool().await;
        let stale = insert_superseded_row(&pool, "u1", "doc-s", "2026-09-01T00:00:00+00:00").await;
        let fresh = insert_superseded_row(&pool, "u1", "doc-f", "2026-10-01T00:00:00+00:00").await;
        // 非 superseded 的 pending 不进收口面
        let other = insert_row(&pool, "u1", "doc-o", "2026-09-01T00:00:00+00:00").await;

        let rows = list_pending_superseded_before(&pool, "2026-10-01T00:00:00+00:00", 50)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "只扫超时的 pending superseded 行");
        assert_eq!(rows[0].id, stale);

        mark_audit_row_done(&pool, stale).await.unwrap();
        assert_eq!(
            get(&pool, stale).await.unwrap().unwrap().status,
            "done",
            "对账补 done"
        );
        assert_eq!(get(&pool, fresh).await.unwrap().unwrap().status, "pending");
        assert_eq!(get(&pool, other).await.unwrap().unwrap().status, "pending");
    }
}
