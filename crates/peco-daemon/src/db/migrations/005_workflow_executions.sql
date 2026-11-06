-- ============================================================================
-- Migration 005: Workflow 管理模块 — 替换旧 Task 系统
-- ============================================================================
-- 删除旧的 tasks / task_logs 表，新建 workflow_schedules / workflow_executions 表。
-- 此迁移不可逆 — 执行前请备份数据库。

-- ⚠️ 破坏性操作：删除旧表及全部数据
DROP TABLE IF EXISTS task_logs;
DROP TABLE IF EXISTS tasks;

-- ============================================================================
-- Workflow 调度配置表
-- ============================================================================
-- 每个 Workflow 最多一条调度记录。独立于 workflow.md 文件，
-- 避免将运行时可变状态写入静态定义文件。
CREATE TABLE IF NOT EXISTS workflow_schedules (
    id TEXT PRIMARY KEY,                    -- UUID v4
    user_id TEXT NOT NULL,                  -- 所属用户
    workflow_name TEXT NOT NULL,            -- workflow.md 中定义的 name
    cron_expr TEXT NOT NULL,                -- 标准 5 字段 cron 表达式
    enabled INTEGER NOT NULL DEFAULT 1,     -- 0=禁用, 1=启用
    timezone TEXT,                          -- IANA 时区名称（NULL = UTC）

    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),

    -- 每个用户+Workflow 最多一条调度记录
    UNIQUE(user_id, workflow_name)
);

CREATE INDEX IF NOT EXISTS idx_ws_user ON workflow_schedules(user_id);
CREATE INDEX IF NOT EXISTS idx_ws_enabled ON workflow_schedules(enabled);

-- ============================================================================
-- Workflow 执行记录表
-- ============================================================================
-- 每次手动触发或定时调度触发产生一条记录。
-- step_results 嵌入在 snapshot_json 中（完整的 WorkflowSnapshot JSON）。
-- 常用查询字段提升为列，避免每次解析 JSON。
CREATE TABLE IF NOT EXISTS workflow_executions (
    id TEXT PRIMARY KEY,                  -- run_id (UUID v4)
    user_id TEXT NOT NULL,                -- 所属用户
    workflow_name TEXT NOT NULL,          -- workflow.md 中定义的 name
    trigger_type TEXT NOT NULL,           -- 'manual' | 'scheduled'
    status TEXT NOT NULL DEFAULT 'running',  -- running|paused|completed|failed|cancelled

    -- 输入/输出概要
    inputs_json TEXT,                     -- JSON: 外部输入参数
    total_steps INTEGER NOT NULL,         -- 总步骤数
    steps_completed INTEGER NOT NULL DEFAULT 0,
    steps_failed INTEGER NOT NULL DEFAULT 0,
    steps_skipped INTEGER NOT NULL DEFAULT 0,
    total_duration_ms INTEGER,            -- 总耗时（毫秒）
    error TEXT,                           -- 顶层错误信息

    -- 完整快照（JSON: WorkflowSnapshot 运行时状态，不含 definition）
    snapshot_json TEXT,

    started_at TEXT NOT NULL,
    finished_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 索引：按用户+状态查询（活跃执行列表）
CREATE INDEX IF NOT EXISTS idx_we_user_status
    ON workflow_executions(user_id, status);

-- 索引：按用户+工作流名查询（某工作流的执行历史）
CREATE INDEX IF NOT EXISTS idx_we_user_name
    ON workflow_executions(user_id, workflow_name);

-- 索引：按用户+时间倒序（最近的执行记录）
CREATE INDEX IF NOT EXISTS idx_we_user_started
    ON workflow_executions(user_id, started_at DESC);
