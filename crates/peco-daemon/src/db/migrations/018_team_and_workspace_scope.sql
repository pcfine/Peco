-- Migration 018: 投影表按资源边界重定 scope
--
-- agents 的唯一维度从 user_id 改为 team_id。SQLite 不能删除表级 UNIQUE，
-- 只能重建表：建新表 → 搬行 → 删旧表 → 改名 → 重建索引。存量行的 team_id
-- 留空：它们的团队归属无从推断，下一次 ensure_agents_indexed 会按磁盘补出
-- 带团队的行走，空团队行因不在任何团队查询里而自然隐去。
-- 注释内禁用半角分号（迁移按半角分号拆分语句执行）。
--
-- 重建前置清掉上次中断留下的残表：建表 → 搬行 → 删旧表 → 改名之间没有事务，
-- 从第一步重跑时必须从一个空的新表开始。agents 不被其它表引用
-- （conversations.agent_id 无外键），重建不会牵动任何子表。

DROP TABLE IF EXISTS agents_new;

CREATE TABLE agents_new (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    team_id TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    icon TEXT NOT NULL DEFAULT '🤖',
    background_color TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'idle',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(team_id, name)
);

INSERT INTO agents_new (id, user_id, team_id, name, description, icon, background_color, status, created_at, updated_at)
    SELECT id, user_id, '', name, description, icon, background_color, status, created_at, updated_at FROM agents;

DROP TABLE agents;

ALTER TABLE agents_new RENAME TO agents;

CREATE INDEX IF NOT EXISTS idx_agents_user_id ON agents(user_id);

CREATE INDEX IF NOT EXISTS idx_agents_team ON agents(team_id);

ALTER TABLE knowledge_bases ADD COLUMN team_id TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS idx_knowledge_bases_team ON knowledge_bases(team_id);

ALTER TABLE conversations ADD COLUMN workspace_id TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS idx_conversations_workspace_agent ON conversations(workspace_id, agent_name, scope);
