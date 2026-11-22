-- 032: d_dispatches 记录发起派发的聊天会话
-- NULL = 机器执行体发起的派发（没有聊天会话），终态通知回落到来源空间的空间会话。
ALTER TABLE d_dispatches ADD COLUMN origin_conversation_id TEXT;
