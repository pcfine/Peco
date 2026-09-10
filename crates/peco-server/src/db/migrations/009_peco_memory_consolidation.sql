-- ============================================================================
-- Migration 009: 记忆巩固基建（删除审计 / 召回统计 / 整理水位）
-- ============================================================================
-- memory_audit：删除审计（含回滚所需完整原文）。审计绝不写回 @private_memory，
--   只落在 SQLite —— 不在任何检索面上，与"删除后不被召回"的隐私承诺一致。
--   outbox 语义：删除前写 pending，成功 mark_done，失败 mark_cancelled；
--   回滚 = 按审计行重放 add_text（doc_id 为内容哈希，天然幂等）。
-- memory_recall_stats：文档级召回命中统计，供巩固候选收集与 TTL 判定使用。
-- memory_consolidation_state：整理水位与最近一次运行统计（幂等断点续跑）。

CREATE TABLE IF NOT EXISTS memory_audit (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id         TEXT NOT NULL,                    -- 所属用户
    kb_name         TEXT NOT NULL,                    -- 被删文档所在知识库
    doc_id          TEXT NOT NULL,                    -- 被删文档 ID（内容哈希前缀）
    title           TEXT NOT NULL,
    content         TEXT NOT NULL,                    -- 完整原文，回滚重放用
    source          TEXT NOT NULL,                    -- ppa_profile / ppa_semantic / ppa_episodic
    reason          TEXT NOT NULL,                    -- manual_organize / consolidation_dedup /
                                                      -- consolidation_ttl / consolidation_distill
    deleted_by      TEXT NOT NULL,                    -- 'agent:@memory' | 'worker' | 'user'
    status          TEXT NOT NULL DEFAULT 'pending',  -- pending | done | cancelled
    deleted_at      TEXT NOT NULL,                    -- ISO 8601
    restored_at     TEXT,                             -- 回滚时间（NULL = 未回滚）
    restored_doc_id TEXT                              -- 重放后的 doc_id（应与 doc_id 一致）
);

CREATE INDEX IF NOT EXISTS idx_memory_audit_user
    ON memory_audit(user_id, deleted_at DESC);

CREATE TABLE IF NOT EXISTS memory_recall_stats (
    user_id          TEXT NOT NULL,
    doc_id           TEXT NOT NULL,
    last_recalled_at TEXT NOT NULL,                   -- ISO 8601
    recall_count     INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (user_id, doc_id)
);

CREATE TABLE IF NOT EXISTS memory_consolidation_state (
    user_id         TEXT PRIMARY KEY,
    last_scanned_ts TEXT,                             -- 候选收集水位（ISO 8601）
    last_run_at     TEXT,                             -- 最近一次巩固运行时刻
    last_run_stats  TEXT                              -- JSON：扫描/合并/删除计数
);
