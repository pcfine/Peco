-- Migration 016: conversations 增加 scope 列，区分 agent 会话与 peco 私有会话
-- 私有会话承载 peco 主对话的会话快照与产物归属，不属于任何 agent，
-- 不应出现在 agent 会话列表 / 活跃计数 / 自动归档里。
-- 注意：注释内禁用半角分号（迁移按半角分号拆分语句执行）。
-- 全文只有这一条语句：执行器逐条执行且无事务，多条 DDL 中途失败会留下半迁移库，
-- 而 ALTER ADD COLUMN 不可重跑（重跑报 duplicate column name: scope）。
ALTER TABLE conversations ADD COLUMN scope TEXT NOT NULL DEFAULT 'agent'
