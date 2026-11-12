-- ============================================================================
-- peco-server 数据库 DDL (SQLite)
-- ============================================================================
-- 所有 UUID 主键和外键使用 TEXT 类型（SQLite 无原生 UUID 类型）。
-- 时间戳使用 TEXT 类型，默认值为 datetime('now')。
-- 使用 IF NOT EXISTS 确保重复执行幂等。

-- 用户表
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    email TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    avatar TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Agent 配置表（轻量索引：完整配置存储在 teams/<team_id>/agents/{name}/agent.md）
--
-- 唯一性按 team_id：daemon 单用户，`user_id` 恒为 'local'，用它做唯一维度会让
-- 两个团队的同名 agent（尤其 @master）只能存成一行，改一个就改到另一个团队。
-- `user_id` 列保留为单用户身份，不再是资源唯一维度。
CREATE TABLE IF NOT EXISTS agents (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    team_id TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'idle',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(team_id, name)
);

-- 对话表
-- scope 区分 agent 会话（'agent'）与 peco 私有会话（'peco'）：后者是真实会话行
-- （产物靠它的外键归属），但不属于任何 agent，不进列表 / 计数 / 自动归档。
-- workspace_id 是会话的隔离维度：同团队两个空间聊同一个 agent 时，
-- `user_id + agent_name` 无法区分两份历史（`user_id` 恒为 'local'）。
CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    agent_id TEXT,
    agent_name TEXT NOT NULL DEFAULT 'unknown',
    workspace_id TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '新对话',
    archived_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    scope TEXT NOT NULL DEFAULT 'agent'
);

-- 消息表（数据库层面记录概要，详细消息由 Session 持久化文件保存）
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    agent_id TEXT,
    agent_name TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 知识库表
-- 归属维度是 team_id：同名 KB 在不同团队是两份资源，而 `user_id` 恒为 'local'，
-- 按它查询等于所有团队共用一份 KB 列表。`user_id` 列只留作单用户身份。
CREATE TABLE IF NOT EXISTS knowledge_bases (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    team_id TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 文档表
CREATE TABLE IF NOT EXISTS documents (
    id TEXT PRIMARY KEY,
    kb_id TEXT NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    filename TEXT NOT NULL,
    filepath TEXT NOT NULL,
    file_size INTEGER NOT NULL DEFAULT 0,
    mime_type TEXT NOT NULL DEFAULT 'application/octet-stream',
    status TEXT NOT NULL DEFAULT 'pending',
    error_msg TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 会话快照表（Session 持久化）
CREATE TABLE IF NOT EXISTS session_snapshots (
    conversation_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    description TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    snapshot_json TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 在途轮检查点（崩溃恢复 — 一批工具刚落地时写入，收尾时删除）
--
-- 不并进 session_snapshots：那张表是 O(历史长度) 全量序列化，
-- 每批工具写一次 = 成本随总历史 × 工具批次数增长。
CREATE TABLE IF NOT EXISTS conversation_inflight_turns (
    conversation_id TEXT PRIMARY KEY,
    session_id      TEXT NOT NULL,
    turn_index      INTEGER NOT NULL,
    reason          TEXT NOT NULL,
    payload_json    TEXT NOT NULL,
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Token 用量日志表
CREATE TABLE IF NOT EXISTS usage_logs (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    agent_name TEXT NOT NULL,
    conversation_id TEXT,
    model TEXT NOT NULL DEFAULT '',
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_usage_logs_user_created ON usage_logs(user_id, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_logs_agent ON usage_logs(agent_name);

-- 服务级配置（键值对存储，如 JWT 密钥等）
CREATE TABLE IF NOT EXISTS server_config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 工作空间模块哈希表：追踪各模块文件系统状态，用于增量同步
CREATE TABLE IF NOT EXISTS workspace_hashes (
    user_id TEXT NOT NULL,
    module TEXT NOT NULL,        -- 'agents' | 'skills' | 'mcp' | 'workflows' | 'providers'
    hash TEXT NOT NULL,          -- SHA-256 hex（64字符）
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, module)
);

-- 索引
CREATE INDEX IF NOT EXISTS idx_agents_user_id ON agents(user_id);
CREATE INDEX IF NOT EXISTS idx_conversations_user_id ON conversations(user_id);
CREATE INDEX IF NOT EXISTS idx_conversations_user_agent_active ON conversations(user_id, agent_name, archived_at, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_messages_conversation_id ON messages(conversation_id);
CREATE INDEX IF NOT EXISTS idx_knowledge_bases_user_id ON knowledge_bases(user_id);
CREATE INDEX IF NOT EXISTS idx_documents_kb_id ON documents(kb_id);
-- Workflow 调度配置表
CREATE TABLE IF NOT EXISTS workflow_schedules (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    workflow_name TEXT NOT NULL,
    cron_expr TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    timezone TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(user_id, workflow_name)
);
CREATE INDEX IF NOT EXISTS idx_ws_user ON workflow_schedules(user_id);
CREATE INDEX IF NOT EXISTS idx_ws_enabled ON workflow_schedules(enabled);

-- Workflow 执行记录表
CREATE TABLE IF NOT EXISTS workflow_executions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    workflow_name TEXT NOT NULL,
    trigger_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running',
    inputs_json TEXT,
    total_steps INTEGER NOT NULL,
    steps_completed INTEGER NOT NULL DEFAULT 0,
    steps_failed INTEGER NOT NULL DEFAULT 0,
    steps_skipped INTEGER NOT NULL DEFAULT 0,
    total_duration_ms INTEGER,
    error TEXT,
    snapshot_json TEXT,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_we_user_status ON workflow_executions(user_id, status);
CREATE INDEX IF NOT EXISTS idx_we_user_name ON workflow_executions(user_id, workflow_name);
CREATE INDEX IF NOT EXISTS idx_we_user_started ON workflow_executions(user_id, started_at DESC);

-- ============================================================================
-- daemon 增量：单用户种子 + 三张新表
-- ============================================================================

-- 单用户种子 —— username / email / password_hash 均 NOT NULL UNIQUE，
-- 空 password_hash 表示「不可密码登录」，与单个本地用户无鉴权一致。
INSERT OR IGNORE INTO users (id, username, email, password_hash)
VALUES ('local', 'local', 'local@localhost', '');

-- 团队：能力根 teams/<team_id>/ 的索引，真源是目录
CREATE TABLE IF NOT EXISTS teams (
    id               TEXT PRIMARY KEY,
    display_name     TEXT NOT NULL,
    template_version INTEGER NOT NULL DEFAULT 0,
    icon             TEXT NOT NULL DEFAULT '',
    created_at       TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 空间：可重建的索引 —— 身份三元组以 .workspace.json 为准，
-- exit_policy / label 等运行属性只存这里
CREATE TABLE IF NOT EXISTS workspaces (
    id             TEXT PRIMARY KEY,
    work_dir       TEXT NOT NULL UNIQUE,
    team_id        TEXT NOT NULL REFERENCES teams(id) ON DELETE RESTRICT,
    label          TEXT,
    exit_policy    TEXT NOT NULL DEFAULT 'inherit'
                   CHECK (exit_policy IN ('inherit','keep','stop')),
    created_at     TEXT NOT NULL DEFAULT (datetime('now')),
    last_opened_at TEXT,
    closed_at      TEXT
);
CREATE INDEX IF NOT EXISTS idx_workspaces_team ON workspaces(team_id);

-- 派发台账：跨空间派发与同空间异步任务的唯一对账依据。
-- target_ws_id 故意不设外键（方案 A）：台账必须长于空间行，CASCADE 会把
-- 「删空间」变成「删审计」，而 ON DELETE SET NULL 会让 UNIQUE(target_ws_id, task_id)
-- 对 NULL 行失效（SQLite 视 NULL 互不相等），幂等保证随之消失。
-- 删除空间的正确性由应用层保障：写入前校验、硬删前置 orphan 化、快照列。
CREATE TABLE IF NOT EXISTS d_dispatches (
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
CREATE INDEX IF NOT EXISTS idx_dispatches_wake ON d_dispatches(wake_state, wake_claimed_at);

-- @after-migrations
-- 上一行是执行器的分界：以下的语句等版本化迁移跑完再执行。
-- 团队 / 空间索引建在迁移 018 给旧库补出的新列上 —— 迁移前建索引会在旧库上
-- 报 no such column，而新库又需要本文件一次建全（含这些索引）。
CREATE INDEX IF NOT EXISTS idx_agents_team ON agents(team_id);
CREATE INDEX IF NOT EXISTS idx_knowledge_bases_team ON knowledge_bases(team_id);
CREATE INDEX IF NOT EXISTS idx_conversations_workspace_agent ON conversations(workspace_id, agent_name, scope);
-- 任务状态索引依赖迁移 019 把旧库的 state 改名为 task_state。旧索引名要显式
-- 丢弃：RENAME COLUMN 会改写索引定义却不改索引名，留着会让「同一列两个名字」
-- 永久留在库里。DROP 也覆盖「019 已成功、进程随后崩溃」的窗口。
DROP INDEX IF EXISTS idx_dispatches_state;
CREATE INDEX IF NOT EXISTS idx_dispatches_task_state ON d_dispatches(target_ws_id, task_state);
