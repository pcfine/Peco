-- ============================================================================
-- Migration 008: Peco 会话归档表
-- ============================================================================
-- DELETE /api/peco/session 清空前，先将会话全文导出为 Markdown 存入本表，
-- 避免用户误删即永久丢失。仅静默归档，
-- 归档内容经 GET /api/peco/archives 端点可取回。

CREATE TABLE IF NOT EXISTS peco_session_archives (
    id TEXT PRIMARY KEY,                    -- UUID v4
    user_id TEXT NOT NULL,                  -- 所属用户
    conversation_id TEXT NOT NULL,          -- 被清空的会话 ID
    turn_count INTEGER NOT NULL,            -- 归档时的轮数
    total_input_tokens INTEGER NOT NULL DEFAULT 0,   -- 归档时的累计用量
    total_output_tokens INTEGER NOT NULL DEFAULT 0,
    content_md TEXT NOT NULL,               -- 归档正文（Markdown，含 pinned 摘要）
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_psa_user_created
    ON peco_session_archives(user_id, created_at);
