-- 019: d_dispatches.state 改名为 task_state
-- state 同时被读成任务状态与唤醒状态，改名消除歧义
ALTER TABLE d_dispatches RENAME COLUMN state TO task_state;
