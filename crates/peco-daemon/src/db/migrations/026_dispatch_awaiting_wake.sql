-- 026: 重建 d_dispatches —— wake_state 增加 'awaiting'，新增 dispatch_id
-- 注册时任务还没跑完，唤醒要等终态才能入队（'awaiting'），因此唤醒态多一档。
-- SQLite 不能改 CHECK 约束，只能按新形状重建表：建新表 → 搬行 → 删旧表 → 改名 → 重建索引。
-- 先丢残留的 d_dispatches_new：迁移逐条执行且无事务，上次崩在中途会留下它。
-- 建表语句刻意不带 IF NOT EXISTS —— 旧表还在时它必须失败，而不是往一张形状未定的
-- 表里搬行。这条 DROP 只收拾崩在删旧表之前那半段：崩在删旧表与改名之间时，残表
-- 已是唯一一份台账，由迁移门控之前的 `recover_interrupted_026` 升格回正式表。
-- 注意：注释内禁用半角分号（迁移按半角分号拆分语句执行）。
DROP TABLE IF EXISTS d_dispatches_new;

CREATE TABLE d_dispatches_new (
    id              TEXT PRIMARY KEY,
    task_id         TEXT NOT NULL,
    target_ws_id    TEXT NOT NULL,
    target_work_dir TEXT NOT NULL,
    target_team_id  TEXT NOT NULL,
    origin_ws_id    TEXT NOT NULL,
    agent_name      TEXT NOT NULL,
    origin_kind     TEXT NOT NULL
                    CHECK (origin_kind IN ('run_machine_task','dispatch_to_workspace')),
    task_state      TEXT NOT NULL,
    wake_state      TEXT NOT NULL DEFAULT 'pending'
                    CHECK (wake_state IN ('awaiting','pending','delivering','delivered','skipped','failed')),
    -- 仅 wake_state='skipped' 时非空；历史行留 NULL（成因不可考）
    wake_skip_reason TEXT
                    CHECK (wake_skip_reason IS NULL OR wake_skip_reason IN (
                      'not_peco_origin', 'peco_identity_missing', 'orphaned',
                      'space_closed', 'attempts_exhausted'
                    )),
    wake_attempts   INTEGER NOT NULL DEFAULT 0,
    wake_claimed_at TEXT,
    delivered_at    TEXT,
    result_digest   TEXT,
    -- 派发工具入参 prompt 的前 512 字；旧行留 NULL（未记录）
    prompt_digest   TEXT,
    -- 派发回执里的 dispatch_id；旧行留 NULL（当时没有记录）
    dispatch_id     TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    terminal_at     TEXT,
    UNIQUE (target_ws_id, task_id)
);

-- 搬行：`dispatch_id` 无旧值可搬，一律 NULL —— 旧行有没有派发回执是事实，
-- 不是可以猜出来的。其余列原样照搬，`wake_skip_reason` 不清洗：历史跳过行的成因
-- 只能来自当时写它的那条路径。
INSERT INTO d_dispatches_new (
    id, task_id, target_ws_id, target_work_dir, target_team_id, origin_ws_id,
    agent_name, origin_kind, task_state, wake_state, wake_skip_reason, wake_attempts,
    wake_claimed_at, delivered_at, result_digest, prompt_digest, dispatch_id,
    created_at, terminal_at
)
SELECT
    id, task_id, target_ws_id, target_work_dir, target_team_id, origin_ws_id,
    agent_name, origin_kind, task_state, wake_state, wake_skip_reason, wake_attempts,
    wake_claimed_at, delivered_at, result_digest, prompt_digest, NULL,
    created_at, terminal_at
FROM d_dispatches;

DROP TABLE d_dispatches;
ALTER TABLE d_dispatches_new RENAME TO d_dispatches;
CREATE INDEX IF NOT EXISTS idx_dispatches_wake ON d_dispatches(wake_state, wake_claimed_at);
CREATE INDEX IF NOT EXISTS idx_dispatches_task_state ON d_dispatches(target_ws_id, task_state);
