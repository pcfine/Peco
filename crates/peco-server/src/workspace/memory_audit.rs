// ============================================================================
// SqliteMemoryAudit — MemoryAuditAccess 的 SQLite 适配
// ============================================================================
//
// peco-core 的删除工具经窄接口 [`MemoryAuditAccess`] 写审计，不感知 SQLite；
// 本类型把调用委托给 `db::memory_audit` DAO（outbox 语义由 DAO 保证：
// pending → done / cancelled，仅 pending 可迁移）。
//
// 注入点在 `WorkspaceManager::open_workspace`（get / get_synced 的公共必经点，
// 管理器持有连接池）— 任何路径创建的 workspace 都携带审计；
// 审计存储确实不可用（如无 DB 的 CLI 场景）时删除工具运行时拒绝执行（fail-closed）。

use peco_core::tools::{MemoryAuditAccess, MemoryAuditEntry};
use sqlx::SqlitePool;

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
        user_id: row.user_id,
        kb_name: row.kb_name,
        doc_id: row.doc_id,
        title: row.title,
        content: row.content,
        source: row.source,
        reason: row.reason,
        deleted_by: row.deleted_by,
        deleted_at: row.deleted_at,
    }
}

#[async_trait::async_trait]
impl MemoryAuditAccess for SqliteMemoryAudit {
    async fn record_pending(&self, entry: MemoryAuditEntry) -> Result<i64, String> {
        dao::insert_pending(&self.pool, &entry)
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
        }
    }

    /// trait 往返：写入的条目经 `get` 读回后字段逐项一致（回滚重放的依据）。
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

        let pool_rows = dao::list_by_user(&audit.pool, "u1", 10, 0).await.unwrap();
        let status_by_doc: std::collections::HashMap<&str, &str> = pool_rows
            .iter()
            .map(|r| (r.doc_id.as_str(), r.status.as_str()))
            .collect();
        assert_eq!(status_by_doc["doc-done"], "done");
        assert_eq!(status_by_doc["doc-cancelled"], "cancelled");
    }
}
