-- Migration 003: Add agent_name + archived_at to conversations table
-- Safe migration: add nullable columns, backfill, then add NOT NULL + index

-- Step 1: Add nullable columns
ALTER TABLE conversations ADD COLUMN agent_name TEXT;
ALTER TABLE conversations ADD COLUMN archived_at TEXT;

-- Step 2: Backfill agent_name from agents table
UPDATE conversations SET agent_name = (
    SELECT a.name FROM agents a WHERE a.id = conversations.agent_id
) WHERE agent_name IS NULL AND agent_id IS NOT NULL;

-- Step 3: For conversations without agent_id, mark as unknown
UPDATE conversations SET agent_name = 'unknown' WHERE agent_name IS NULL;

-- Step 4: Rebuild table with NOT NULL constraint on agent_name.
-- SQLite does not support ALTER COLUMN, so we create a new table,
-- copy data, drop old, and rename.
CREATE TABLE conversations_new (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    agent_id TEXT,
    agent_name TEXT NOT NULL DEFAULT 'unknown',
    title TEXT NOT NULL DEFAULT '新对话',
    archived_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO conversations_new
    SELECT id, user_id, agent_id, agent_name, title, archived_at, created_at, updated_at
    FROM conversations;

DROP TABLE conversations;
ALTER TABLE conversations_new RENAME TO conversations;

-- Step 5: Re-create indexes (lost after DROP TABLE)
CREATE INDEX IF NOT EXISTS idx_conversations_user_id
    ON conversations(user_id);
CREATE INDEX IF NOT EXISTS idx_conversations_user_agent_active
    ON conversations(user_id, agent_name, archived_at, updated_at DESC);
