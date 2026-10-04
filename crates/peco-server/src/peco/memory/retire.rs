// ============================================================================
// retire — 记忆删除共享原语（审计先行 outbox：pending → 删除 → done/cancelled）
// ============================================================================
//
// 取代事务 §6.1 ③（hook）与巩固 worker（dedup/TTL）共用，自
// `ConsolidationWorker::delete_with_audit` 抽取。fail-closed：审计写不进 →
// 在删除之前 return Err。文档已不在（delete 返回 NotFound）不产生
// cancelled 行 — 审计行直接 done，返回 `AlreadyAbsent` 交调用方按
// §6.4 ③c 视为成功。

use sqlx::SqlitePool;

use knowledge_base::KnowledgeError;
use peco_core::knowledge::{KnowledgeManager, KnowledgeModuleError};
use peco_core::tools::MemoryAuditEntry;
use tracing::{info, warn};

/// 删除结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireOutcome {
    /// 文档已删除，审计行 pending → done。
    Deleted,
    /// 文档已不在 KB（delete 返回 NotFound）：目标已达成，审计行直接
    /// done，不产生 cancelled 行 — 调用方按 §6.4 ③c 视为成功。
    AlreadyAbsent,
}

/// 审计先行删除：写 pending → 删文档 → done / cancelled。
///
/// 入参即 `MemoryAuditEntry`（`deleted_by` / `kb_name` / `reason` 随条目
/// 参数化）+ `km`；`insert_pending` 失败时不碰 KB（fail-closed）。
/// 其它删除失败（含 KB 缺失）→ mark_cancelled + Err，交调用方 warn。
pub async fn delete_with_audit(
    db: &SqlitePool,
    km: &KnowledgeManager,
    entry: &MemoryAuditEntry,
) -> Result<RetireOutcome, String> {
    let audit_id = crate::db::memory_audit::insert_pending(db, entry)
        .await
        .map_err(|e| format!("audit write failed (fail-closed): {e}"))?;

    match km.delete_document(&entry.kb_name, &entry.doc_id).await {
        Ok(report) => {
            if let Err(e) = crate::db::memory_audit::mark_done(db, audit_id).await {
                warn!(
                    audit_id,
                    error = %e,
                    "Audit row left pending after successful delete"
                );
            }
            info!(
                user_id = %entry.user_id,
                doc_id = %entry.doc_id,
                reason = %entry.reason,
                removed_chunks = report.removed_chunks,
                "Memory document retired"
            );
            Ok(RetireOutcome::Deleted)
        }
        Err(KnowledgeModuleError::Knowledge(KnowledgeError::NotFound(_))) => {
            if let Err(e) = crate::db::memory_audit::mark_done(db, audit_id).await {
                warn!(
                    audit_id,
                    error = %e,
                    "Audit row left pending after absent delete"
                );
            }
            Ok(RetireOutcome::AlreadyAbsent)
        }
        Err(e) => {
            if let Err(mark_err) = crate::db::memory_audit::mark_cancelled(db, audit_id).await {
                warn!(audit_id, error = %mark_err, "Failed to cancel audit row");
            }
            Err(format!("delete failed for {}: {e}", entry.doc_id))
        }
    }
}
