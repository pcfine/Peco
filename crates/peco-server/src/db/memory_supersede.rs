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
}
