// ============================================================================
// SqliteMemoryAudit — MemoryAuditAccess 的 SQLite 适配
// ============================================================================
//
// peco-core 的删除工具经窄接口 [`MemoryAuditAccess`] 写审计，不感知 SQLite；
// 本类型把调用委托给 `db::memory_audit` DAO（outbox 语义由 DAO 保证：
// pending → done / cancelled，仅 pending 可迁移）。
//
// 注入点是 `space/open.rs`：空间每被打开一次就注入一次，因此任何从注册表
// 取到的工作空间都携带审计；未注入时删除类工具运行时拒绝执行（fail-closed）。
//
// `MemoryAuditEntry.user_id` 在 core 侧是知识库隔离维度（team_id），而 DB 的
// `user_id` 列存单用户身份 —— 本适配器负责把两者分列写入与读回。

use peco_core::tools::{MemoryAuditAccess, MemoryAuditEntry};
use sqlx::SqlitePool;

use crate::LOCAL_USER_ID;
use crate::db::memory_audit as dao;

/// 持有连接池的审计实现 — 每个 workspace 注入一个（连接池可共享）。
pub struct SqliteMemoryAudit {
    pool: SqlitePool,
}

impl SqliteMemoryAudit {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

/// DAO 完整行 → 窄接口条目。
///
/// status / restored_at / restored_doc_id 仅服务 REST 回滚面，不属于删除侧
/// 最小字段集，在此丢弃。
fn row_to_entry(row: dao::MemoryAuditRow) -> MemoryAuditEntry {
    MemoryAuditEntry {
        // 读回原 scope：调用方（回滚重放）看到的必须与写入时同一个维度。
        user_id: row.kb_scope,
        kb_name: row.kb_name,
        doc_id: row.doc_id,
        title: row.title,
        content: row.content,
        source: row.source,
        reason: row.reason,
        deleted_by: row.deleted_by,
        deleted_at: row.deleted_at,
        topic_key: row.topic_key,
        successor_doc_id: row.successor_doc_id,
    }
}

#[async_trait::async_trait]
impl MemoryAuditAccess for SqliteMemoryAudit {
    async fn record_pending(&self, entry: MemoryAuditEntry) -> Result<i64, String> {
        let kb_scope = entry.user_id.clone();
        let entry = MemoryAuditEntry {
            user_id: LOCAL_USER_ID.to_string(),
            ..entry
        };
        dao::insert_pending(&self.pool, &entry, &kb_scope)
            .await
            .map_err(|e| e.to_string())
    }

    async fn mark_done(&self, id: i64) -> Result<(), String> {
        dao::mark_done(&self.pool, id)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn mark_cancelled(&self, id: i64) -> Result<(), String> {
        dao::mark_cancelled(&self.pool, id)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn get(&self, id: i64) -> Result<Option<MemoryAuditEntry>, String> {
        dao::get(&self.pool, id)
            .await
            .map_err(|e| e.to_string())
            .map(|opt| opt.map(row_to_entry))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn test_audit() -> (SqliteMemoryAudit, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = db::connect(&url).await.unwrap();
        db::run_migrations(&pool).await.unwrap();
        (SqliteMemoryAudit::new(pool), dir)
    }

    fn sample_entry(doc_id: &str) -> MemoryAuditEntry {
        MemoryAuditEntry {
            user_id: "u1".into(),
            kb_name: "@private_memory".into(),
            doc_id: doc_id.into(),
            title: "memory_1".into(),
            content: "用户偏好 Rust".into(),
            source: "ppa_semantic".into(),
            reason: "manual_organize".into(),
            deleted_by: "agent:@memory".into(),
            deleted_at: "2026-09-10T00:00:00+00:00".into(),
            topic_key: None,
            successor_doc_id: None,
        }
    }

    /// trait 往返：写入的条目经 `get` 读回后字段逐项一致（回滚重放的依据），
    /// 而 DB 侧 `user_id` 恒为单用户身份、scope 落在 `kb_scope`。
    #[tokio::test]
    async fn record_pending_then_get_roundtrips_entry() {
        let (audit, _dir) = test_audit().await;

        let id = audit
            .record_pending(sample_entry("doc-roundtrip"))
            .await
            .unwrap();
        assert!(id > 0);

        let entry = audit.get(id).await.unwrap().expect("entry");
        assert_eq!(entry.user_id, "u1");
        assert_eq!(entry.kb_name, "@private_memory");
        assert_eq!(entry.doc_id, "doc-roundtrip");
        assert_eq!(entry.title, "memory_1");
        assert_eq!(entry.content, "用户偏好 Rust");
        assert_eq!(entry.source, "ppa_semantic");
        assert_eq!(entry.reason, "manual_organize");
        assert_eq!(entry.deleted_by, "agent:@memory");
        assert_eq!(entry.deleted_at, "2026-09-10T00:00:00+00:00");

        assert!(audit.get(999).await.unwrap().is_none());

        let (user_id, kb_scope) = sqlx::query_as::<_, (String, String)>(
            "SELECT user_id, kb_scope FROM memory_audit WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&audit.pool)
        .await
        .unwrap();
        assert_eq!(user_id, LOCAL_USER_ID, "user_id 列不得写入 scope");
        assert_eq!(kb_scope, "u1", "scope 必须落在 kb_scope 列");
    }

    /// outbox 迁移经 trait 调用与经 DAO 调用语义一致。
    #[tokio::test]
    async fn mark_done_and_cancelled_follow_outbox_rules() {
        let (audit, _dir) = test_audit().await;

        let done_id = audit
            .record_pending(sample_entry("doc-done"))
            .await
            .unwrap();
        audit.mark_done(done_id).await.unwrap();
        // 仅 pending 可迁移：done 后再迁移无效且不报错
        audit.mark_done(done_id).await.unwrap();

        let cancelled_id = audit
            .record_pending(sample_entry("doc-cancelled"))
            .await
            .unwrap();
        audit.mark_cancelled(cancelled_id).await.unwrap();
        // cancelled 是终态：mark_done 不再生效
        audit.mark_done(cancelled_id).await.unwrap();

        let pool_rows = dao::list_by_user(&audit.pool, LOCAL_USER_ID, 10, 0)
            .await
            .unwrap();
        let status_by_doc: std::collections::HashMap<&str, &str> = pool_rows
            .iter()
            .map(|r| (r.doc_id.as_str(), r.status.as_str()))
            .collect();
        assert_eq!(status_by_doc["doc-done"], "done");
        assert_eq!(status_by_doc["doc-cancelled"], "cancelled");
    }
}
