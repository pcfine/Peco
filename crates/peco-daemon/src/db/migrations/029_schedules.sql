-- 定时任务：一条调度一行，一次触发一行 run。
--
-- 时间一律 UTC 文本（与 datetime('now') 同格式）：cron 计算用本机时区，落库统一
-- UTC，读取侧才不会因为进程换时区看到两份时间线。
CREATE TABLE IF NOT EXISTS scheduled_tasks (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    target_ws_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    prompt       TEXT NOT NULL,
    cron_expr    TEXT NOT NULL,
    enabled      INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    next_run_at  TEXT,
    last_run_at  TEXT,
    last_status  TEXT CHECK (last_status IS NULL OR last_status IN ('completed','failed','skipped','timeout','interrupted')),
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_scheduled_due ON scheduled_tasks(enabled, next_run_at);

-- notify_attempt_at 与 next_attempt_at 分开：前者是通知重试的时刻，后者是「会话忙、
-- 稍后重跑」的时刻，一列两用会让两个重试互相把对方的时刻覆盖掉。
CREATE TABLE IF NOT EXISTS scheduled_runs (
    id              TEXT PRIMARY KEY,
    schedule_id     TEXT NOT NULL REFERENCES scheduled_tasks(id) ON DELETE CASCADE,
    trigger_kind    TEXT NOT NULL CHECK (trigger_kind IN ('cron','manual')),
    scheduled_for   TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('pending','running','completed','failed','skipped','timeout','interrupted')),
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT,
    conversation_id TEXT,
    started_at      TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at     TEXT,
    result_digest   TEXT,
    error           TEXT,
    notify_state    TEXT NOT NULL DEFAULT 'none' CHECK (notify_state IN ('none','pending','sent','skipped','failed')),
    notify_attempts INTEGER NOT NULL DEFAULT 0,
    notify_attempt_at TEXT,
    notified_at     TEXT
);
CREATE INDEX IF NOT EXISTS idx_scheduled_runs_by_schedule ON scheduled_runs(schedule_id, started_at DESC);
-- 同一槽位只能有一行 cron run：进程在「插入」与「标记 running」之间被重启时，
-- 下一次触发算出的还是同一个槽位，唯一索引让重复触发收敛到同一行而不是双发。
CREATE UNIQUE INDEX IF NOT EXISTS uq_scheduled_run_slot ON scheduled_runs(schedule_id, scheduled_for) WHERE trigger_kind = 'cron';
