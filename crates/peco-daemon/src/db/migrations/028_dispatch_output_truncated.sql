-- 028: d_dispatches 的正文截断标志
-- 0 = output 是原文完整副本；1 = 落库时被上限截断，长度不再是原文总长。
-- 单独一份文件：027 已执行、这条还没执行的库必须能靠自己的门控补上。
ALTER TABLE d_dispatches ADD COLUMN output_truncated INTEGER NOT NULL DEFAULT 0;
