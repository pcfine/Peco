-- 031: 工作空间回收站。
--
-- `deleted_at` 非空即「在回收站」：空间行、会话与记忆都留着，只是不再出现在
-- 空间清单、派发目标与打开路径里。恢复 = 清空该列（marker 由应用层补回）。
-- 注意：注释内禁用半角分号（迁移按半角分号拆分语句执行）。
ALTER TABLE workspaces ADD COLUMN deleted_at TEXT;
CREATE INDEX IF NOT EXISTS idx_workspaces_deleted ON workspaces(deleted_at);
