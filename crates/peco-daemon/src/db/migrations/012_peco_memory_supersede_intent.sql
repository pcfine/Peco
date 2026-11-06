-- ============================================================================
-- Migration 012: 记忆取代机制 · 阶段二（audit 扩展 + 意图 WAL）
-- ============================================================================
-- (a) memory_audit 扩展：历史 tab + 回滚沿链所需
ALTER TABLE memory_audit ADD COLUMN topic_key        TEXT;
ALTER TABLE memory_audit ADD COLUMN successor_doc_id TEXT;
CREATE INDEX IF NOT EXISTS idx_memory_audit_topic
    ON memory_audit(user_id, topic_key, deleted_at DESC);

-- (b) 取代意图 WAL（outbox）
CREATE TABLE IF NOT EXISTS memory_supersede_intent (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     TEXT NOT NULL,  kb_name TEXT NOT NULL,  topic_key TEXT,
    old_doc_id  TEXT NOT NULL,  old_title TEXT NOT NULL, old_source TEXT NOT NULL,
    new_doc_id  TEXT NOT NULL,  new_title TEXT NOT NULL,
    new_content TEXT NOT NULL,  new_source TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending',   -- pending|processing|done|failed
    attempts    INTEGER NOT NULL DEFAULT 0,
    claimed_at  TEXT,
    created_at  TEXT NOT NULL,  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_msi_scan ON memory_supersede_intent(user_id, status);
