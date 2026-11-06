-- Migration 015: artifacts 表 —— 对话产物（图片/视频/音频/文档）注册表
-- 归属由 conversation_id 决定；rel_path 相对 work/<user_id>/，换 data_dir 根时自动跟随。
-- 文件本身不存库，只存投影：文件被清理后行仍在，UI 可据此提示「已过期」。
-- 注意：注释内禁用半角分号（迁移按半角分号拆分语句执行）。

CREATE TABLE IF NOT EXISTS artifacts (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    mime TEXT NOT NULL,
    title TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    size_bytes INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
DELETE FROM artifacts WHERE rowid NOT IN (SELECT MIN(rowid) FROM artifacts GROUP BY conversation_id, rel_path);
CREATE UNIQUE INDEX IF NOT EXISTS idx_artifacts_conv_path ON artifacts(conversation_id, rel_path);
CREATE INDEX IF NOT EXISTS idx_artifacts_conversation ON artifacts(conversation_id, created_at)
