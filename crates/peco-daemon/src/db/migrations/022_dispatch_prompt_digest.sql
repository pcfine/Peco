-- 022: d_dispatches 增加 prompt_digest
-- 派发工具入参里那段任务描述的前 512 字，让任务卡片能显示「谁让它做什么」。
-- 旧行留 NULL：当时没有记录入参，接口原样返回 null，不编造一条描述。
ALTER TABLE d_dispatches ADD COLUMN prompt_digest TEXT;
