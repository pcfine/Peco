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

-- Agent 配置表（轻量索引：完整配置存储在 agents/{name}/agent.md）
CREATE TABLE IF NOT EXISTS agents (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    icon TEXT NOT NULL DEFAULT '🤖',
    color TEXT NOT NULL DEFAULT '#6366f1',
    background_color TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'idle',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(user_id, name)
);

-- 对话表
CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    agent_id TEXT,
    agent_name TEXT NOT NULL DEFAULT 'unknown',
    title TEXT NOT NULL DEFAULT '新对话',
    archived_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
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
CREATE TABLE IF NOT EXISTS knowledge_bases (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
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
