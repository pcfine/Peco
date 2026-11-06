-- ============================================================================
-- Migration 007: Peco 上下文压缩日志表
-- ============================================================================
-- 每次滚动压缩成功后追加一条记录，供 GET /api/peco/session 的
-- context_metrics 汇总（压缩次数 / token 变化时间线 / 摘要长度曲线）。
-- 只追加、不更新 — 历史记录即观测数据本身。

CREATE TABLE IF NOT EXISTS peco_compaction_log (
    id TEXT PRIMARY KEY,                    -- UUID v4
    user_id TEXT NOT NULL,                  -- 所属用户
    conversation_id TEXT NOT NULL,          -- 会话 ID（Peco 永续会话）
    evicted_turns INTEGER NOT NULL,         -- 本次驱逐的轮数
    tokens_before INTEGER NOT NULL,         -- 压缩前估算 token（pinned + committed 全量口径）
    tokens_after INTEGER NOT NULL,          -- 压缩后估算 token
    summary_chars INTEGER NOT NULL,         -- 压缩后摘要正文字符数（观测摘要质量漂移）
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_pcl_user_created
    ON peco_compaction_log(user_id, created_at);
