// ============================================================================
// memory_supersede_shadow 表 DAO — 取代机制 shadow 观测（阶段一）
// ============================================================================
//
// 只观测不动作：每轮提取后落一行「候选快照 + 提取事实 + 取代决策」，
// 供效果门（design-v4 §10）离线标定。本表不参与任何检索/展示路径，
// 本阶段无调度器接线 —— purge_shadow_older_than 只提供清理函数。

use sqlx::SqlitePool;

/// shadow 观测行（写入侧字段集）。
#[derive(Debug, Clone)]
pub struct ShadowRow {
    pub user_id: String,
    /// ISO 8601
    pub created_at: String,
    pub candidates_json: String,
    pub facts_json: String,
    pub decisions_json: String,
    pub acted: bool,
    pub extracted_topic_cnt: i64,
    pub episodic_cnt: i64,
}

/// 写入一条 shadow 行，返回行 id。
pub async fn insert_shadow(pool: &SqlitePool, row: &ShadowRow) -> Result<i64, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO memory_supersede_shadow \
         (user_id, created_at, candidates_json, facts_json, decisions_json, \
          acted, extracted_topic_cnt, episodic_cnt) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&row.user_id)
    .bind(&row.created_at)
    .bind(&row.candidates_json)
    .bind(&row.facts_json)
    .bind(&row.decisions_json)
    .bind(row.acted)
    .bind(row.extracted_topic_cnt)
    .bind(row.episodic_cnt)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// 物理清除早于 cutoff 的 shadow 行，返回删除行数。
pub async fn purge_shadow_older_than(pool: &SqlitePool, cutoff: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM memory_supersede_shadow WHERE created_at < ?")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 统计某用户的 shadow 行数（健康/标定用）。
pub async fn count_for_user(pool: &SqlitePool, user_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memory_supersede_shadow WHERE user_id = ?")
        .bind(user_id)
        .fetch_one(pool)
        .await
}

// ============================================================================
// memory_supersede_intent 表 DAO — 取代意图 WAL（outbox，阶段二）
// ============================================================================
//
// 写路径定序（design-v4 §6.1）：① write_intent(pending) → ② add(new) →
// ③ delete_with_audit(old) → ④ CAS intent → done。任一环节崩溃残留的
// pending 行由对账（reconcile）领取后幂等收口；本表不参与检索/展示面。

const INTENT_COLUMNS: &str = "id, user_id, kb_name, topic_key, old_doc_id, old_title, \
     old_source, new_doc_id, new_title, new_content, new_source, attempts, created_at, updated_at";

/// 取代意图行（写入侧字段集）。
///
/// **不存 `old_content`**（design-v4 §8）：旧条全量由退役时的
/// `memory_audit.content` 承载；对账需要旧条时 `get_document` 现取。
#[derive(Debug, Clone)]
pub struct IntentRow {
    pub user_id: String,
    pub kb_name: String,
    pub topic_key: Option<String>,
    pub old_doc_id: String,
    pub old_title: String,
    pub old_source: String,
    pub new_doc_id: String,
    pub new_title: String,
    pub new_content: String,
    pub new_source: String,
    /// ISO 8601
    pub created_at: String,
    /// ISO 8601
    pub updated_at: String,
}

/// 领取到的意图（对账/执行用，含 id / attempts）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClaimedIntent {
    pub id: i64,
    pub user_id: String,
    pub kb_name: String,
    pub topic_key: Option<String>,
    pub old_doc_id: String,
    pub old_title: String,
    pub old_source: String,
    pub new_doc_id: String,
    pub new_title: String,
    pub new_content: String,
    pub new_source: String,
    /// 每次成功领取 +1（含陈旧回收）→ 超 `reconcile_max_attempts` 转 failed。
    pub attempts: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// ① 写意图（pending）。返回 intent id。
pub async fn write_intent(pool: &SqlitePool, row: &IntentRow) -> Result<i64, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO memory_supersede_intent \
         (user_id, kb_name, topic_key, old_doc_id, old_title, old_source, \
          new_doc_id, new_title, new_content, new_source, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&row.user_id)
    .bind(&row.kb_name)
    .bind(&row.topic_key)
    .bind(&row.old_doc_id)
    .bind(&row.old_title)
    .bind(&row.old_source)
    .bind(&row.new_doc_id)
    .bind(&row.new_title)
    .bind(&row.new_content)
    .bind(&row.new_source)
    .bind(&row.created_at)
    .bind(&row.updated_at)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// ② 领取下一条可执行意图（CAS，M3 逐条领取）。
///
/// `reclaim_processing_before` = now − `reconcile_claim_timeout`：可领取条件为
/// `status='pending'`，或 `status='processing' AND claimed_at < 该时刻`（陈旧回收）。
/// 领取置 `status='processing', claimed_at=now, attempts=attempts+1`；
/// **`rows_affected==1` 才算领取**，否则返回 `None`（并发方抢先 → 本轮未领取，
/// 下轮重试）。CAS `Err` 同语义，由调用方 warn 后放弃本轮。
pub async fn claim_next(
    pool: &SqlitePool,
    user_id: &str,
    reclaim_processing_before: &str,
) -> Result<Option<ClaimedIntent>, sqlx::Error> {
    let Some(id) = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM memory_supersede_intent \
         WHERE user_id = ? AND (status = 'pending' \
             OR (status = 'processing' AND claimed_at < ?)) \
         ORDER BY id LIMIT 1",
    )
    .bind(user_id)
    .bind(reclaim_processing_before)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };

    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE memory_supersede_intent \
         SET status = 'processing', claimed_at = ?, attempts = attempts + 1, updated_at = ? \
         WHERE id = ? AND (status = 'pending' OR (status = 'processing' AND claimed_at < ?))",
    )
    .bind(&now)
    .bind(&now)
    .bind(id)
    .bind(reclaim_processing_before)
    .execute(pool)
    .await?;
    if result.rows_affected() != 1 {
        return Ok(None);
    }

    let row = sqlx::query_as::<_, ClaimedIntent>(&format!(
        "SELECT {INTENT_COLUMNS} FROM memory_supersede_intent WHERE id = ?"
    ))
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(Some(row))
}

/// pending/processing → done（写路径 ④ / 对账补 done）。
///
/// 已终态（done/failed）行 no-op — 幂等收口。
pub async fn mark_done(pool: &SqlitePool, id: i64, now: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE memory_supersede_intent SET status = 'done', updated_at = ? \
         WHERE id = ? AND status IN ('pending', 'processing')",
    )
    .bind(now)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// pending/processing → failed（对账超 `reconcile_max_attempts` 上界）。
pub async fn mark_failed(pool: &SqlitePool, id: i64, now: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE memory_supersede_intent SET status = 'failed', updated_at = ? \
         WHERE id = ? AND status IN ('pending', 'processing')",
    )
    .bind(now)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// processing → pending（对账本轮未收口，释放回队列待下轮重领）。
///
/// **不重置 `attempts`**：重试上界正是靠它跨轮单调增长实现 —— 调用方
/// 领取时判定 `attempts > reconcile_max_attempts` 转 failed。终态行 no-op。
pub async fn release_to_pending(pool: &SqlitePool, id: i64, now: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE memory_supersede_intent SET status = 'pending', claimed_at = NULL, updated_at = ? \
         WHERE id = ? AND status = 'processing'",
    )
    .bind(now)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// health 计数（design-v4 §11 / `GET /memory/supersede/health`）。
#[derive(Debug, Clone, Default)]
pub struct SupersedeHealth {
    pub pending: i64,
    pub processing: i64,
    pub failed: i64,
}

/// 按用户聚合 intent 状态计数（done 不入响应 — 终态即历史）。
pub async fn health_counts(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<SupersedeHealth, sqlx::Error> {
    let (pending, processing, failed) = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT \
             COALESCE(SUM(status = 'pending'), 0), \
             COALESCE(SUM(status = 'processing'), 0), \
             COALESCE(SUM(status = 'failed'), 0) \
         FROM memory_supersede_intent WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(SupersedeHealth {
        pending,
        processing,
        failed,
    })
}

/// 保留期清理（design-v4 §6.6）：`done` 早于 `cutoff_done`、
/// `failed/cancelled` 早于 `cutoff_failed`（按 `updated_at` = 落终态时刻）
/// 物理清除；`pending/processing` 不清（对账未收口）。返回删除行数。
pub async fn purge_intent_older_than(
    pool: &SqlitePool,
    cutoff_done: &str,
    cutoff_failed: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM memory_supersede_intent \
         WHERE (status = 'done' AND updated_at < ?) \
            OR (status IN ('failed', 'cancelled') AND updated_at < ?)",
    )
    .bind(cutoff_done)
    .bind(cutoff_failed)
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

    fn sample_row(user_id: &str, created_at: &str) -> ShadowRow {
        ShadowRow {
            user_id: user_id.to_string(),
            created_at: created_at.to_string(),
            candidates_json: r#"[{"id":"doc-a","source":"ppa_semantic","text":"旧事实","channel":"search"}]"#
                .to_string(),
            facts_json: r#"[{"category":"semantic","content":"新事实","topic":"project_phase","supersedes":["doc-a"]}]"#
                .to_string(),
            decisions_json:
                r#"{"per_turn_cap":3,"raw_count":1,"dropped":0,"would_act":1,"items":[]}"#.to_string(),
            acted: false,
            extracted_topic_cnt: 1,
            episodic_cnt: 0,
        }
    }

    #[tokio::test]
    async fn test_insert_shadow_roundtrip_fields() {
        let (pool, _dir) = test_pool().await;
        let row = sample_row("u1", "2026-10-01T00:00:00+00:00");

        let id = insert_shadow(&pool, &row).await.unwrap();
        assert!(id > 0);

        let (created_at, candidates, facts, decisions, acted, topics, episodic) =
            sqlx::query_as::<_, (String, String, String, String, i64, i64, i64)>(
                "SELECT created_at, candidates_json, facts_json, decisions_json, \
             acted, extracted_topic_cnt, episodic_cnt \
             FROM memory_supersede_shadow WHERE id = ?",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(created_at, "2026-10-01T00:00:00+00:00");
        assert_eq!(candidates, row.candidates_json);
        assert_eq!(facts, row.facts_json);
        assert_eq!(decisions, row.decisions_json);
        assert_eq!(acted, 0, "阶段一恒不执行（acted=false）");
        assert_eq!(topics, 1);
        assert_eq!(episodic, 0);
    }

    #[tokio::test]
    async fn test_purge_shadow_older_than_keeps_recent() {
        let (pool, _dir) = test_pool().await;
        insert_shadow(&pool, &sample_row("u1", "2026-06-01T00:00:00+00:00"))
            .await
            .unwrap();
        insert_shadow(&pool, &sample_row("u1", "2026-06-15T00:00:00+00:00"))
            .await
            .unwrap();
        insert_shadow(&pool, &sample_row("u1", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();

        let purged = purge_shadow_older_than(&pool, "2026-09-01T00:00:00+00:00")
            .await
            .unwrap();
        assert_eq!(purged, 2, "只清除 cutoff 之前的行");
        assert_eq!(count_for_user(&pool, "u1").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_count_for_user_isolates_users() {
        let (pool, _dir) = test_pool().await;
        insert_shadow(&pool, &sample_row("u1", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();
        insert_shadow(&pool, &sample_row("u1", "2026-10-02T00:00:00+00:00"))
            .await
            .unwrap();
        insert_shadow(&pool, &sample_row("u2", "2026-10-02T00:00:00+00:00"))
            .await
            .unwrap();

        assert_eq!(count_for_user(&pool, "u1").await.unwrap(), 2);
        assert_eq!(count_for_user(&pool, "u2").await.unwrap(), 1);
        assert_eq!(count_for_user(&pool, "u3").await.unwrap(), 0);
    }

    // ── intent WAL（阶段二）────────────────────────────────────────────────

    fn sample_intent(user_id: &str, updated_at: &str) -> IntentRow {
        IntentRow {
            user_id: user_id.to_string(),
            kb_name: "@private_memory".to_string(),
            topic_key: Some("answer_style".to_string()),
            old_doc_id: "old-1".to_string(),
            old_title: "memory_1000_0".to_string(),
            old_source: "ppa_profile".to_string(),
            new_doc_id: "new-1".to_string(),
            new_title: "memory_2000_0".to_string(),
            new_content: "用户偏好详尽的回答".to_string(),
            new_source: "ppa_profile".to_string(),
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
        }
    }

    /// write → claim → done；done 后同批内无可再领取的行。
    #[tokio::test]
    async fn test_intent_write_claim_done_flow() {
        let (pool, _dir) = test_pool().await;
        let id = write_intent(&pool, &sample_intent("u1", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();
        assert!(id > 0);

        let claimed = claim_next(&pool, "u1", "2026-10-01T00:05:00+00:00")
            .await
            .unwrap()
            .expect("pending 意图应可领取");
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.attempts, 1, "首次领取 attempts=1");
        assert_eq!(claimed.new_doc_id, "new-1");
        assert_eq!(claimed.topic_key.as_deref(), Some("answer_style"));

        mark_done(&pool, id, "2026-10-01T00:10:00+00:00")
            .await
            .unwrap();
        assert!(
            claim_next(&pool, "u1", "2026-10-01T00:15:00+00:00")
                .await
                .unwrap()
                .is_none(),
            "done 终态不得再被领取"
        );
    }

    /// 已领取未超时的 processing 行不可重复领取（CAS 单方领取）。
    #[tokio::test]
    async fn test_second_claim_skips_fresh_processing() {
        let (pool, _dir) = test_pool().await;
        write_intent(&pool, &sample_intent("u1", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();

        assert!(
            claim_next(&pool, "u1", "2000-01-01T00:00:00+00:00")
                .await
                .unwrap()
                .is_some()
        );
        // claimed_at 是当下时刻，不可能早于 2000 年 → 不构成陈旧
        assert!(
            claim_next(&pool, "u1", "2000-01-01T00:00:00+00:00")
                .await
                .unwrap()
                .is_none(),
            "未超时的 processing 行不得被再次领取"
        );
        // 用户隔离：他人名下无可领取行
        assert!(
            claim_next(&pool, "u2", "2100-01-01T00:00:00+00:00")
                .await
                .unwrap()
                .is_none()
        );
    }

    /// 陈旧回收：claimed_at 早于阈值 → 可重新领取且 attempts 递增。
    #[tokio::test]
    async fn test_stale_reclaim_increments_attempts() {
        let (pool, _dir) = test_pool().await;
        write_intent(&pool, &sample_intent("u1", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();

        let first = claim_next(&pool, "u1", "2100-01-01T00:00:00+00:00")
            .await
            .unwrap()
            .expect("首取");
        assert_eq!(first.attempts, 1);

        let second = claim_next(&pool, "u1", "2100-01-01T00:00:00+00:00")
            .await
            .unwrap()
            .expect("claimed_at 必早于 2100 → 陈旧回收");
        assert_eq!(second.attempts, 2, "每次成功领取 +1（含陈旧回收）");
        assert_eq!(second.id, first.id);
    }

    /// 释放回 pending：立即可重领、attempts 跨次递增（不重置）。
    #[tokio::test]
    async fn test_release_to_pending_allows_reclaim_without_resetting_attempts() {
        let (pool, _dir) = test_pool().await;
        write_intent(&pool, &sample_intent("u1", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();

        let first = claim_next(&pool, "u1", "2000-01-01T00:00:00+00:00")
            .await
            .unwrap()
            .expect("首取");
        assert_eq!(first.attempts, 1);

        release_to_pending(&pool, first.id, "2026-10-01T00:05:00+00:00")
            .await
            .unwrap();

        // 释放后为 pending —— 无需等陈旧回收窗口即可重领
        let second = claim_next(&pool, "u1", "2000-01-01T00:00:00+00:00")
            .await
            .unwrap()
            .expect("released 行应立即可重领");
        assert_eq!(second.id, first.id);
        assert_eq!(
            second.attempts, 2,
            "release 不得重置 attempts（重试上界依赖它）"
        );

        // 终态行释放是 no-op
        mark_done(&pool, first.id, "2026-10-01T00:10:00+00:00")
            .await
            .unwrap();
        release_to_pending(&pool, first.id, "2026-10-01T00:11:00+00:00")
            .await
            .unwrap();
        let status: String =
            sqlx::query_scalar("SELECT status FROM memory_supersede_intent WHERE id = ?")
                .bind(first.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "done", "done 终态不得被 release 回退");
    }

    /// health 按用户聚合三态计数。
    #[tokio::test]
    async fn test_health_counts_by_status() {
        let (pool, _dir) = test_pool().await;
        write_intent(&pool, &sample_intent("u1", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();
        write_intent(&pool, &sample_intent("u1", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();
        let c = write_intent(&pool, &sample_intent("u1", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();
        write_intent(&pool, &sample_intent("u2", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();

        claim_next(&pool, "u1", "2000-01-01T00:00:00+00:00")
            .await
            .unwrap()
            .expect("领走 a");
        mark_failed(&pool, c, "2026-10-01T00:10:00+00:00")
            .await
            .unwrap();

        let health = health_counts(&pool, "u1").await.unwrap();
        assert_eq!(health.pending, 1, "剩一条未领取");
        assert_eq!(health.processing, 1, "a 处于 processing");
        assert_eq!(health.failed, 1, "c 已转 failed");

        let other = health_counts(&pool, "u2").await.unwrap();
        assert_eq!(
            (other.pending, other.processing, other.failed),
            (1, 0, 0),
            "计数按用户隔离"
        );
        let none = health_counts(&pool, "u3").await.unwrap();
        assert_eq!((none.pending, none.processing, none.failed), (0, 0, 0));
    }

    /// 保留期分档：done 按 cutoff_done、failed 按 cutoff_failed、pending 不清。
    #[tokio::test]
    async fn test_purge_intent_older_than_by_tier() {
        let (pool, _dir) = test_pool().await;

        // done 落在 6 月（两档阈值都已过期）
        let old_done = write_intent(&pool, &sample_intent("u1", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();
        mark_done(&pool, old_done, "2026-06-01T00:00:00+00:00")
            .await
            .unwrap();
        // failed 落在 6 月
        let old_failed = write_intent(&pool, &sample_intent("u1", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();
        mark_failed(&pool, old_failed, "2026-06-05T00:00:00+00:00")
            .await
            .unwrap();
        // done 落在 10 月（保留期内）
        let recent_done = write_intent(&pool, &sample_intent("u1", "2026-10-01T00:00:00+00:00"))
            .await
            .unwrap();
        mark_done(&pool, recent_done, "2026-10-01T00:00:00+00:00")
            .await
            .unwrap();
        // pending（updated_at 极旧也不清 — 对账未收口）
        let pending = write_intent(&pool, &sample_intent("u1", "2026-01-01T00:00:00+00:00"))
            .await
            .unwrap();

        let purged = purge_intent_older_than(
            &pool,
            "2026-09-01T00:00:00+00:00",
            "2026-09-01T00:00:00+00:00",
        )
        .await
        .unwrap();
        assert_eq!(purged, 2, "只清除两档各自的过期终态行");

        let health = health_counts(&pool, "u1").await.unwrap();
        assert_eq!(health.pending, 1, "pending 保留");
        assert_eq!(health.processing, 0);
        assert_eq!(health.failed, 0, "过期 failed 已清");
        // recent_done 保留：done 不入 health 计数，直接点数
        let done_left = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM memory_supersede_intent WHERE id = ? AND status = 'done'",
        )
        .bind(recent_done)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(done_left, 1, "保留期内 done 不得清除");
        let pending_left = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM memory_supersede_intent WHERE id = ?",
        )
        .bind(pending)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(pending_left, 1, "pending 行必须保留");
    }
}
