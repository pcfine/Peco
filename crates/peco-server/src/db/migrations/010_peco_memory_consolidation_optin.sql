-- ============================================================================
-- Migration 010: 自动整理用户级 opt-in 开关
-- ============================================================================
-- memory_consolidation_optin：用户对「自动记忆整理（云端 Flash 批量调用）」
--   的显式同意开关，fail-closed —— 无行 = 未 opt-in。
--   控制面（同意）落 SQLite、数据面（记忆）留 LanceDB，物理隔离。
--   opted_in_at 记录首次开启时刻（关闭不清除、重开不覆盖），updated_at 恒刷新。

CREATE TABLE IF NOT EXISTS memory_consolidation_optin (
    user_id     TEXT PRIMARY KEY,
    enabled     INTEGER NOT NULL DEFAULT 0,   -- 0 = 未 opt-in（fail-closed）
    opted_in_at TEXT,                          -- 首次开启时刻（ISO 8601）
    updated_at  TEXT NOT NULL                  -- 最近一次变更时刻
);
