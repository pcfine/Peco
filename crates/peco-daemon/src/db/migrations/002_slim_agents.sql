-- Migration 002: Slim the agents table (remove config columns stored in agent.md)
-- Safe migration: create new table, copy index columns, drop old, rename.
-- Only runs when old columns exist (checked by caller or manual execution).

-- Step 1: Create new table with lightweight schema
CREATE TABLE IF NOT EXISTS agents_v2 (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    icon TEXT NOT NULL DEFAULT '🤖',
    color TEXT NOT NULL DEFAULT '#6366f1',
    status TEXT NOT NULL DEFAULT 'idle',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(user_id, name)
);

-- Step 2: Copy only the lightweight columns from old table
INSERT OR IGNORE INTO agents_v2 (id, user_id, name, description, icon, color, status, created_at, updated_at)
    SELECT id, user_id, name, description, icon, color, status, created_at, updated_at FROM agents;

-- Step 3: Drop old table and rename
DROP TABLE agents;
ALTER TABLE agents_v2 RENAME TO agents;

-- Recreate indexes
CREATE INDEX IF NOT EXISTS idx_agents_user_id ON agents(user_id);
